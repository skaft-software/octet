use super::*;
use octet_agent::InputPart;
use octet_ai::{Modality, ModalitySet};
use std::fs;
use std::path::Path;

fn all_modalities() -> ModalitySet {
    ModalitySet::none()
        .with(Modality::Image)
        .with(Modality::Audio)
}

#[test]
fn facade_preserves_extension_classification() {
    assert_eq!(
        media_kind_for_path(Path::new("a.PNG")),
        Some(MediaKind::Image(mime::IMAGE_PNG))
    );
    assert_eq!(
        media_kind_for_path(Path::new("b.m4a")),
        Some(MediaKind::Audio(octet_ai::AudioFormat::Aac))
    );
    assert_eq!(media_kind_for_path(Path::new("c.rs")), None);
    assert_eq!(
        file_kind_for_path(Path::new("brief.PDF")),
        Some(FileKind::Pdf)
    );
}

#[test]
fn paste_classification_checks_paths_before_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("photo.png");
    fs::write(&path, b"image").expect("write image");
    assert_eq!(
        classify_paste(&path.display().to_string()),
        PasteKind::MediaFile(path)
    );
    assert_eq!(classify_paste("short prose"), PasteKind::Verbatim);
    assert_eq!(
        classify_paste(&"line\n".repeat(LARGE_PASTE_LINES + 1)),
        PasteKind::LargeText
    );
}

#[test]
fn media_batch_is_atomic_and_ordered() {
    let temp = tempfile::tempdir().expect("tempdir");
    let first = temp.path().join("first.png");
    let second = temp.path().join("second.png");
    fs::write(&first, b"one").expect("write first");
    fs::write(&second, b"two").expect("write second");

    let mut ledger = AttachmentLedger::default();
    let chips = ledger
        .attach_media_batch(&[first.clone(), second.clone()], all_modalities())
        .expect("batch attaches");
    assert_eq!(chips, ["[Image #1]", "[Image #2]"]);
    assert_eq!(ledger.entries.len(), 2);

    let missing = temp.path().join("missing.png");
    assert!(ledger
        .attach_media_batch(&[first, missing], all_modalities())
        .is_err());
    assert_eq!(
        ledger.entries.len(),
        2,
        "failed batch must not partially append"
    );
}

#[test]
fn explicit_paths_replace_supported_tokens_without_partial_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let image = temp.path().join("photo.png");
    let pdf = temp.path().join("brief.pdf");
    fs::write(&image, b"image").expect("write image");
    fs::write(&pdf, b"pdf").expect("write pdf");

    let input = format!("{} {}", image.display(), pdf.display());
    let mut ledger = AttachmentLedger::default();
    let replaced = ledger
        .attach_explicit_paths(&input, all_modalities())
        .expect("path admission")
        .expect("supported path payload");
    assert!(replaced.contains("[Image #1]"));
    assert!(replaced.contains("[PDF #2]"));
    assert!(matches!(
        ledger.entries[1].payload,
        AttachmentPayload::FileReference(_)
    ));
}

#[test]
fn composition_keeps_text_and_media_in_display_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let image = temp.path().join("photo.png");
    fs::write(&image, b"image").expect("write image");

    let mut ledger = AttachmentLedger::default();
    let media_chip = ledger
        .attach_media(&image, all_modalities())
        .expect("attach image");
    let pasted_chip = ledger.attach_pasted_text("expanded paste".into());
    let display = format!("before {media_chip} middle {pasted_chip} after");
    let composed = compose(display.clone(), &mut ledger);

    assert_eq!(composed.display_text, display);
    assert_eq!(
        composed.transcript_text,
        "before [Image #1] middle expanded paste after"
    );
    // The transcript keeps the plain chip label; the absolute path reaches only
    // the model-bound parts, so the human-visible text is unchanged.
    assert!(
        !composed.transcript_text.contains("photo.png"),
        "the file path must not leak into the visible transcript"
    );
    // Display order is preserved, with a per-attachment annotation naming the
    // source file inserted immediately before the media it describes. The wire
    // formats carry no filename for inline media, so this is the only way the
    // model learns which file the bytes came from.
    assert_eq!(composed.parts.len(), 4);
    assert!(matches!(&composed.parts[0], InputPart::Text(text) if text == "before "));
    assert!(
        matches!(&composed.parts[1], InputPart::Text(text) if text.contains("photo.png")
            && text.starts_with("[attached image: "))
    );
    assert!(matches!(&composed.parts[2], InputPart::Media(_)));
    assert!(
        matches!(&composed.parts[3], InputPart::Text(text) if text == " middle expanded paste after")
    );
    assert!(ledger.is_empty(), "compose drains the ledger");
}

#[test]
fn model_text_replacement_keeps_the_media_filename_annotation() {
    // Template expansion rewrites the free text ahead of media. The filename
    // annotation must survive that, or the model is left with bare bytes again.
    let temp = tempfile::tempdir().expect("tempdir");
    let image = temp.path().join("photo.png");
    fs::write(&image, b"image").expect("write image");

    let mut ledger = AttachmentLedger::default();
    let chip = ledger
        .attach_media(&image, all_modalities())
        .expect("attach image");
    let mut composed = compose(format!("look at {chip}"), &mut ledger);

    composed.replace_model_text("expanded prompt".into());

    let annotation = composed.parts.iter().find_map(|part| match part {
        InputPart::Text(text) if text.contains("photo.png") => Some(text.clone()),
        _ => None,
    });
    assert!(
        annotation.is_some(),
        "the attachment path must survive model-text replacement"
    );
    assert!(
        composed.parts.iter().any(|part| matches!(part, InputPart::Media(_))),
        "media must survive model-text replacement"
    );
}

#[test]
fn completion_owners_keep_mentions_and_paths_separate() {
    let files = vec![
        "src/main.rs".into(),
        "docs/README.md".into(),
        "src/lib.rs".into(),
    ];
    assert_eq!(mention_matches(&files, "read", 5), vec!["docs/README.md"]);
    assert_eq!(active_mention("open @src"), Some("src"));
    assert_eq!(active_path("open ./src/"), Some("./src/"));
    assert_eq!(active_path("/model gpt"), None);
}
