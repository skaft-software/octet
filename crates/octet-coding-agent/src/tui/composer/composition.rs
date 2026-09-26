#![allow(missing_docs)]

//! Submit-time composition of editable chip text into model input parts.
//!
//! The composer ledger supplies resolved payloads; this module owns only the
//! deterministic projection from display text to transcript text and ordered
//! `InputPart` values.

use octet_agent::{InputPart, UserInput};
use octet_ai::Media;

use super::attachments::{
    Attachment, AttachmentLedger, AttachmentPayload, MAX_AUDIO_BYTES, MAX_IMAGE_BYTES,
};

/// The drained composer content: editable chip text, readable transcript text,
/// ordered model parts, and attachments retained for steering restore.
#[derive(Clone, Debug)]
pub struct ComposedInput {
    /// Text as it appeared in the editor. Media and large pastes remain chips.
    pub display_text: String,
    /// Text shown after submission. Pasted-text chips expand in place; media
    /// chips remain readable labels because their payload is non-textual.
    pub transcript_text: String,
    pub parts: Vec<InputPart>,
    pub attachments: Vec<Attachment>,
    /// When true, the owning provider run must expose no tools.
    pub answer_only: bool,
}

impl ComposedInput {
    pub fn from_text(text: String) -> Self {
        Self {
            parts: vec![InputPart::Text(text.clone())],
            display_text: text.clone(),
            transcript_text: text,
            attachments: Vec::new(),
            answer_only: false,
        }
    }

    pub fn for_answer(display_text: String, model_text: String) -> Self {
        let mut input = Self::from_text(model_text);
        input.display_text = display_text.clone();
        input.transcript_text = display_text;
        input.answer_only = true;
        input
    }

    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|part| match part {
            InputPart::Text(text) => text.trim().is_empty(),
            InputPart::Media(_) => false,
        })
    }

    pub fn into_user_input(self) -> UserInput {
        UserInput::from(self.parts)
    }

    /// Replace textual model input after deterministic prompt composition while
    /// retaining every attached media payload. Text is consolidated ahead of
    /// media so extension/template expansion never mutates opaque media bytes.
    ///
    /// Per-attachment annotation lines (the text part immediately preceding each
    /// media part) are retained too, because they name the file the bytes came
    /// from. Dropping them here would leave the model with bare media again after
    /// any template expansion, which is exactly the state this change exists to
    /// remove.
    pub fn replace_model_text(&mut self, text: String) {
        // Pair each media part with the annotation that directly precedes it. A
        // media part with no preceding text simply has no annotation, which is
        // the case for a bare media payload composed elsewhere.
        let mut annotations = Vec::new();
        let mut pending: Option<String> = None;
        let mut media = Vec::new();
        for part in self.parts.drain(..) {
            match part {
                InputPart::Media(media_part) => {
                    annotations.push(pending.take());
                    media.push(InputPart::Media(media_part));
                }
                InputPart::Text(text) => {
                    // A text part that is an annotation is short and matches the
                    // annotation shape; anything else is free text and replaces
                    // the pending annotation only if none is already collected.
                    if pending.is_none() && is_media_annotation(&text) {
                        pending = Some(text);
                    } else {
                        pending = None;
                    }
                }
            }
        }

        let mut parts = vec![InputPart::Text(text)];
        for (annotation, part) in annotations.into_iter().zip(media) {
            if let Some(annotation) = annotation {
                parts.push(InputPart::Text(annotation));
            }
            parts.push(part);
        }
        self.parts = parts;
    }
}

/// Whether a composed text part is a per-attachment media annotation.
///
/// The annotation is the short bracketed line naming an attached file, emitted
/// immediately before its media part. Recognising it by shape keeps the media
/// filename attached to its bytes across a model-text replacement.
fn is_media_annotation(text: &str) -> bool {
    let trimmed = text.trim();
    (trimmed.starts_with("[attached image: ") || trimmed.starts_with("[attached audio: "))
        && trimmed.ends_with(']')
}

/// Resolve chips against the ledger, draining it entirely.
pub fn compose(display_text: String, ledger: &mut AttachmentLedger) -> ComposedInput {
    let entries = ledger.take_all();
    // Locate the first occurrence of each entry's chip; unmatched entries drop.
    let mut found: Vec<(usize, &Attachment)> = entries
        .iter()
        .filter_map(|entry| display_text.find(&entry.chip).map(|at| (at, entry)))
        .collect();
    found.sort_by_key(|(at, _)| *at);

    let mut parts: Vec<InputPart> = Vec::new();
    let mut text_run = String::new();
    let mut cursor = 0usize;
    for (at, entry) in &found {
        // Overlapping matches cannot happen: chips contain a unique "#id".
        text_run.push_str(&display_text[cursor..*at]);
        match &entry.payload {
            AttachmentPayload::PastedText(pasted) => text_run.push_str(pasted),
            AttachmentPayload::FileReference(path) => text_run.push_str(path),
            AttachmentPayload::Media { media, byte_len, path } => {
                let limit = match media {
                    Media::Image(_) => MAX_IMAGE_BYTES,
                    Media::Audio(_) => MAX_AUDIO_BYTES,
                };
                debug_assert!(*byte_len <= limit, "attached media exceeded its size cap");
                if !text_run.is_empty() {
                    parts.push(InputPart::Text(std::mem::take(&mut text_run)));
                }
                // Name the file the bytes came from. The wire formats carry no
                // filename for inline media, so without this the model sees
                // opaque bytes with no idea which file they belong to and cannot
                // re-read, crop, or compare it. This is model-bound only: the
                // human-visible transcript keeps the plain chip label, so an
                // absolute path is never printed to the user. The wording is
                // stable for a given attachment so it does not churn the
                // prompt-cache prefix.
                let annotation = match media {
                    Media::Image(_) => format!("[attached image: {path}]"),
                    Media::Audio(_) => format!("[attached audio: {path}]"),
                };
                parts.push(InputPart::Text(annotation));
                parts.push(InputPart::Media(media.clone()));
            }
        }
        cursor = at + entry.chip.len();
    }
    text_run.push_str(&display_text[cursor..]);
    if !text_run.is_empty() || parts.is_empty() {
        parts.push(InputPart::Text(text_run));
    }

    // Transcript text is a separate projection from the editor. Replace from
    // right to left so byte offsets discovered in display_text stay valid.
    // Media chips intentionally survive as human-readable attachment labels.
    let mut transcript_text = display_text.clone();
    for (at, entry) in found.iter().rev() {
        if let AttachmentPayload::PastedText(pasted) = &entry.payload {
            transcript_text.replace_range(*at..*at + entry.chip.len(), pasted);
        }
    }

    let matched = found.into_iter().map(|(_, entry)| entry.clone()).collect();
    ComposedInput {
        display_text,
        transcript_text,
        parts,
        attachments: matched,
        answer_only: false,
    }
}
