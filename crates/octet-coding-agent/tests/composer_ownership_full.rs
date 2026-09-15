//! Bounded parity fixtures for the ownership split.
//!
//! The production TUI is private behind `octet_sdk`; these fixtures include
//! the four ownership modules directly so integration compilation still checks
//! their sibling handoffs without widening the public crate API. Picker event
//! loops and model/session/extension/subagent flows are intentionally not
//! represented here: those remain owned by `tui/pickers.rs` and its callers.

#[path = "../src/tui/composer/attachments.rs"]
mod attachments;
#[path = "../src/tui/composer/composition.rs"]
mod composition;
#[path = "../src/tui/composer/paste.rs"]
mod paste;
#[path = "../src/tui/composer/picker.rs"]
mod picker;

use attachments::AttachmentLedger;
use composition::compose;
use octet_ai::{Modality, ModalitySet};
use std::fs;
use std::path::Path;

fn all_modalities() -> ModalitySet {
    ModalitySet::none()
        .with(Modality::Image)
        .with(Modality::Audio)
}

#[test]
fn owner_handoffs_preserve_path_to_ledger_to_composition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let image = temp.path().join("handoff.png");
    fs::write(&image, b"image").expect("write image");

    let dropped = paste::explicit_dropped_paths(&image.display().to_string())
        .expect("explicit drop")
        .into_iter()
        .next()
        .expect("one dropped path");
    assert_eq!(dropped.path, image);

    let mut ledger = AttachmentLedger::default();
    let chip = ledger
        .attach_media(&dropped.path, all_modalities())
        .expect("ledger admission");
    let composed = compose(format!("see {chip}"), &mut ledger);
    assert_eq!(composed.parts.len(), 2);
    assert!(ledger.is_empty());
}

#[test]
fn picker_discovery_does_not_admit_files_by_itself() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(temp.path().join("src")).expect("create source directory");
    fs::write(temp.path().join("src/main.rs"), b"fn main() {}").expect("write source");

    let suggestions = picker::path_matches(temp.path(), "src/", 8);
    assert_eq!(suggestions.len(), 1);
    assert!(suggestions[0].completion.ends_with("main.rs"));
    assert_eq!(attachments::media_kind_for_path(Path::new("main.rs")), None);
}
