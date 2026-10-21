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
fn path_led_multiline_prompts_stay_verbatim_pastes_and_classify_as_paths() {
    let prompt = "/Users/example/project/new-file.txt\n\nPlease review this destination.";
    assert_eq!(classify_paste(prompt), PasteKind::Verbatim);
    assert!(looks_like_absolute_path(prompt));

    // Explicit shell quoting/escaping disambiguates missing one-component
    // destinations from slash commands without creating test files.
    assert!(looks_like_absolute_path(
        "\"/octet-missing-path-with-space\"\n\nCreate this file."
    ));
    assert!(looks_like_absolute_path(
        "/octet-missing-path\\ with-space\n\nCreate this file."
    ));

    // An unknown one-segment slash name remains on the slash-command path.
    assert!(!looks_like_absolute_path(
        "/unknown-command\n\nPlease explain this command."
    ));
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
        .attach_media_batch(
            &[first.clone(), second.clone()],
            all_modalities(),
            octet_ai::Protocol::OpenAiChat.audio_input_formats(),
        )
        .expect("batch attaches");
    assert_eq!(chips, ["[Image #1]", "[Image #2]"]);
    assert_eq!(ledger.entries.len(), 2);

    let missing = temp.path().join("missing.png");
    assert!(ledger
        .attach_media_batch(
            &[first, missing],
            all_modalities(),
            octet_ai::Protocol::OpenAiChat.audio_input_formats()
        )
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
        .attach_explicit_paths(
            &input,
            all_modalities(),
            octet_ai::Protocol::OpenAiChat.audio_input_formats(),
        )
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
        .attach_media(
            &image,
            all_modalities(),
            octet_ai::Protocol::OpenAiChat.audio_input_formats(),
        )
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
        .attach_media(
            &image,
            all_modalities(),
            octet_ai::Protocol::OpenAiChat.audio_input_formats(),
        )
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
        composed
            .parts
            .iter()
            .any(|part| matches!(part, InputPart::Media(_))),
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
    let index = workspace_index_from(files);
    assert_eq!(mention_matches(&index, "read", 5), vec!["docs/README.md"]);
    assert_eq!(active_mention("open @src"), Some("src"));
    assert_eq!(active_path("open ./src/"), Some("./src/"));
    assert_eq!(active_path("/model gpt"), None);
}

/// Build an index over explicit paths, as the workspace walk would.
///
/// The walk itself needs a real directory; the ranking and case-folding
/// behavior under test does not.
fn workspace_index_from(paths: Vec<String>) -> WorkspaceFileIndex {
    let mut sorted = paths;
    sorted.sort();
    WorkspaceFileIndex::from_paths(sorted)
}

#[test]
fn prompt_composition_keeps_a_pdf_path_that_the_transcript_shows_as_a_chip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pdf = temp.path().join("Quarterly Report.pdf");
    fs::write(&pdf, b"%PDF-1.4").expect("write pdf");
    let mut ledger = AttachmentLedger::default();
    let chip = ledger
        .attach_file_reference(&pdf, ModalitySet::none())
        .expect("pdf reference");
    let pasted = ledger.attach_pasted_text("line one\nline two".into());

    let mut composed = compose(format!("read {chip} and {pasted}"), &mut ledger);
    let path = pdf.to_string_lossy().into_owned();
    assert_eq!(
        composed.transcript_text,
        format!("read {chip} and line one\nline two")
    );
    assert_eq!(
        composed.model_text,
        format!("read {path} and line one\nline two")
    );

    // Prompt composition rebuilds the model message from this text; the path
    // is the only thing that tells the model which file the chip stood for.
    composed.replace_model_text(composed.model_text.clone());
    let text: String = composed
        .parts
        .iter()
        .filter_map(|part| match part {
            InputPart::Text(text) => Some(text.as_str()),
            InputPart::Media(_) => None,
        })
        .collect();
    assert!(text.contains(&path) && !text.contains(&chip), "{text:?}");
}

#[test]
fn answer_input_composes_its_directive_not_its_command_display() {
    let composed = ComposedInput::for_answer("/answer now".into(), "Answer now.".into());
    assert_eq!(composed.transcript_text, "/answer now");
    assert_eq!(composed.model_text, "Answer now.");
}

#[test]
fn follow_up_batches_join_model_text_with_their_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pdf = temp.path().join("brief.pdf");
    fs::write(&pdf, b"%PDF-1.4").expect("write pdf");
    let mut ledger = AttachmentLedger::default();
    let chip = ledger
        .attach_file_reference(&pdf, ModalitySet::none())
        .expect("pdf reference");
    let mut first = ComposedInput::from_text("first".into());
    first.append_follow_up(compose(format!("then {chip}"), &mut ledger));
    assert_eq!(
        first.model_text,
        format!("first\n\nthen {}", pdf.to_string_lossy())
    );
    assert_eq!(first.transcript_text, format!("first\n\nthen {chip}"));
}

fn pdf_modalities() -> ModalitySet {
    all_modalities().with(Modality::Pdf)
}

#[test]
fn a_pdf_dropped_on_a_pdf_model_is_sent_as_its_document_bytes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pdf = temp.path().join("Quarterly Report.pdf");
    fs::write(&pdf, b"%PDF-1.7\nbody\n%%EOF\n").expect("write pdf");
    let mut ledger = AttachmentLedger::default();
    let input = pdf.display().to_string().replace(' ', "\\ ");
    let replaced = ledger
        .attach_explicit_paths(
            &input,
            pdf_modalities(),
            octet_ai::Protocol::OpenAiChat.audio_input_formats(),
        )
        .expect("path admission")
        .expect("supported path payload");
    assert_eq!(replaced, "[PDF #1]");

    let composed = compose(format!("read {replaced}"), &mut ledger);
    let path = pdf.to_string_lossy().into_owned();
    // The chip stays readable; the annotation names the file next to its bytes.
    assert_eq!(composed.transcript_text, "read [PDF #1]");
    let InputPart::Text(annotation) = &composed.parts[1] else {
        panic!("annotation precedes the document: {:?}", composed.parts);
    };
    assert_eq!(annotation, &format!("[attached PDF: {path}]"));
    let InputPart::Media(octet_ai::Media::Document(document)) = &composed.parts[2] else {
        panic!("document part: {:?}", composed.parts);
    };
    assert_eq!(&document.data[..], b"%PDF-1.7\nbody\n%%EOF\n");
    assert_eq!(document.name.as_deref(), Some("Quarterly Report.pdf"));
    assert_eq!(document.media_type, mime::APPLICATION_PDF);

    // Prompt composition keeps the document and its annotation.
    let mut replaced = composed.clone();
    replaced.replace_model_text(composed.model_text.clone());
    assert!(replaced
        .parts
        .iter()
        .any(|part| matches!(part, InputPart::Media(octet_ai::Media::Document(_)))));
    assert!(replaced.parts.iter().any(
        |part| matches!(part, InputPart::Text(text) if text == &format!("[attached PDF: {path}]"))
    ));
}

#[test]
fn pdf_mentions_follow_the_model_and_refuse_impostors_and_oversized_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pdf = temp.path().join("brief.pdf");
    fs::write(&pdf, b"%PDF-1.4").expect("write pdf");
    let mut ledger = AttachmentLedger::default();

    ledger
        .attach_file_reference(&pdf, pdf_modalities())
        .expect("document");
    ledger
        .attach_file_reference(&pdf, all_modalities())
        .expect("path reference");
    assert!(matches!(
        ledger.entries[0].payload,
        AttachmentPayload::Media {
            media: octet_ai::Media::Document(_),
            ..
        }
    ));
    assert!(matches!(
        ledger.entries[1].payload,
        AttachmentPayload::FileReference(_)
    ));

    let impostor = temp.path().join("notes.pdf");
    fs::write(&impostor, b"plain text").expect("write impostor");
    assert!(matches!(
        ledger.attach_file_reference(&impostor, pdf_modalities()),
        Err(AttachError::NotAPdf)
    ));

    let large = temp.path().join("large.pdf");
    let file = fs::File::create(&large).expect("create large pdf");
    file.set_len(MAX_PDF_BYTES + 1).expect("size large pdf");
    assert!(matches!(
        ledger.attach_file_reference(&large, pdf_modalities()),
        Err(AttachError::TooLarge { limit_bytes }) if limit_bytes == MAX_PDF_BYTES
    ));
    assert_eq!(ledger.entries.len(), 2, "refusals leave no partial chips");
}
