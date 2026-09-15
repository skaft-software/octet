#![allow(missing_docs)]

//! Stable composer facade.
//!
//! Ownership is intentionally split below this module: [`attachments`] owns
//! media classification and the attachment ledger, [`paste`] owns paste/path
//! admission, [`picker`] owns inline completion discovery, and [`composition`]
//! owns submit-time projection into model parts. This facade keeps existing
//! `crate::tui::composer::*` imports and public APIs stable while picker event
//! loops and model/session/extension/subagent flows remain with `pickers.rs`
//! and their view owners.

mod attachments;
mod composition;
mod paste;
mod picker;

pub use attachments::{
    file_kind_for_path, media_kind_for_path, AttachError, Attachment, AttachmentLedger,
    AttachmentPayload, FileKind, MediaKind, MAX_AUDIO_BYTES, MAX_IMAGE_BYTES,
};
pub use composition::{compose, ComposedInput};
pub use paste::{
    classify_paste, classify_paste_paths, explicit_dropped_paths, looks_like_absolute_path,
    parse_dropped_path, parse_dropped_paths, DroppedPath, PasteKind, LARGE_PASTE_CHARS,
    LARGE_PASTE_LINES,
};
pub use picker::{
    active_mention, active_path, is_path_query, mention_matches, path_matches, workspace_files,
    PathSuggestion,
};

#[cfg(test)]
mod tests;
