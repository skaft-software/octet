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

pub use attachments::{file_kind_for_path, media_kind_for_path, Attachment, AttachmentLedger};
#[cfg(test)]
pub use attachments::{AttachmentPayload, FileKind, MediaKind, MAX_IMAGE_BYTES};
pub use composition::{compose, ComposedInput};
#[cfg(test)]
pub use paste::LARGE_PASTE_LINES;
pub use paste::{classify_paste, looks_like_absolute_path, PasteKind};
pub use picker::{
    active_mention, active_path, is_path_query, mention_matches, path_matches, workspace_files,
    PathSuggestion,
};

#[cfg(test)]
mod tests;
