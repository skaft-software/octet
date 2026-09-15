#![allow(missing_docs)]

//! Media classification and the chip-backed attachment ledger.
//!
//! This module owns attachment admission, capability/size validation, payload
//! storage, chip deletion, and restoration. Paste parsing and submit-time
//! composition are separate siblings so the ledger remains the only owner of
//! opaque attachment state.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use octet_ai::{AudioFormat, Media, Modality, ModalitySet};
use sexy_tui_rs::TextEditor;
use unicode_segmentation::UnicodeSegmentation;

use super::paste::explicit_dropped_paths;

/// Attach-time size cap for inline images.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Attach-time size cap for inline audio.
pub const MAX_AUDIO_BYTES: u64 = 20 * 1024 * 1024;

/// Media classification of a file path, by extension (no content sniffing).
#[derive(Clone, Debug, PartialEq)]
pub enum MediaKind {
    Image(mime::Mime),
    Audio(AudioFormat),
}

/// Non-media file kinds represented as path references in the model prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
}

/// Classify a supported document extension.
pub fn file_kind_for_path(path: &Path) -> Option<FileKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "pdf" => Some(FileKind::Pdf),
        _ => None,
    }
}

/// Classify native inline media by extension, without content sniffing.
pub fn media_kind_for_path(path: &Path) -> Option<MediaKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(MediaKind::Image(mime::IMAGE_PNG)),
        "jpg" | "jpeg" => Some(MediaKind::Image(mime::IMAGE_JPEG)),
        "gif" => Some(MediaKind::Image(mime::IMAGE_GIF)),
        "webp" => Some(MediaKind::Image("image/webp".parse().expect("static mime"))),
        "wav" => Some(MediaKind::Audio(AudioFormat::Wav)),
        "mp3" => Some(MediaKind::Audio(AudioFormat::Mp3)),
        "flac" => Some(MediaKind::Audio(AudioFormat::Flac)),
        "opus" => Some(MediaKind::Audio(AudioFormat::Opus)),
        "aac" | "m4a" => Some(MediaKind::Audio(AudioFormat::Aac)),
        _ => None,
    }
}

fn unsupported_media_type_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "m4v" | "mov" | "avi" | "mkv" | "webm" | "wmv" | "flv" | "mpeg" | "mpg" | "3gp"
        | "ogv" => Some("video"),
        "bmp" | "tif" | "tiff" | "ico" | "heic" | "heif" | "avif" | "svg" => Some("image"),
        _ => None,
    }
}

/// Why an attachment was refused. The caller renders this as a composer
/// notice; validation remains next to the ledger that owns the admission.
#[derive(Debug)]
pub enum AttachError {
    Unreadable(String),
    TooLarge { limit_bytes: u64 },
    UnsupportedModality { modality: &'static str },
    UnsupportedAudioFormat(AudioFormat),
    UnsupportedMediaType(&'static str),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(reason) => write!(f, "cannot read file: {reason}"),
            Self::TooLarge { limit_bytes } => {
                write!(
                    f,
                    "file exceeds the {} MB limit",
                    limit_bytes / (1024 * 1024)
                )
            }
            Self::UnsupportedModality { modality } => {
                write!(f, "the active model does not accept {modality} input")
            }
            Self::UnsupportedAudioFormat(format) => write!(
                f,
                "the active provider route does not accept {format:?} audio input (use WAV or MP3)",
            ),
            Self::UnsupportedMediaType(kind) => write!(
                f,
                "unsupported {kind} input (native input supports images and WAV/MP3 audio)",
            ),
        }
    }
}

struct PreparedMedia {
    label: &'static str,
    media: Media,
    byte_len: u64,
}

fn prepare_media(path: &Path, modalities: ModalitySet) -> Result<PreparedMedia, AttachError> {
    let kind = match media_kind_for_path(path) {
        Some(kind) => kind,
        None => {
            return Err(match unsupported_media_type_for_path(path) {
                Some(kind) => AttachError::UnsupportedMediaType(kind),
                None => AttachError::Unreadable(
                    "unsupported media extension (images: PNG/JPEG/GIF/WebP; audio: WAV/MP3)"
                        .into(),
                ),
            });
        }
    };
    let (label, modality, modality_name, limit) = match &kind {
        MediaKind::Image(_) => ("Image", Modality::Image, "image", MAX_IMAGE_BYTES),
        MediaKind::Audio(_) => ("Audio", Modality::Audio, "audio", MAX_AUDIO_BYTES),
    };
    if !modalities.contains(modality) {
        return Err(AttachError::UnsupportedModality {
            modality: modality_name,
        });
    }
    if let MediaKind::Audio(format) = &kind {
        if !matches!(format, AudioFormat::Wav | AudioFormat::Mp3) {
            return Err(AttachError::UnsupportedAudioFormat(*format));
        }
    }
    let metadata = fs::metadata(path).map_err(|e| AttachError::Unreadable(e.to_string()))?;
    if !metadata.is_file() {
        return Err(AttachError::Unreadable("not a regular file".into()));
    }
    if metadata.len() > limit {
        return Err(AttachError::TooLarge { limit_bytes: limit });
    }
    let file = std::fs::File::open(path).map_err(|e| AttachError::Unreadable(e.to_string()))?;
    let mut limited = file.take(limit + 1);
    let mut data = Vec::new();
    limited
        .read_to_end(&mut data)
        .map_err(|e| AttachError::Unreadable(e.to_string()))?;
    if data.len() as u64 > limit {
        return Err(AttachError::TooLarge { limit_bytes: limit });
    }
    let byte_len = data.len() as u64;
    let media = match kind {
        MediaKind::Image(mime) => Media::image_bytes(bytes::Bytes::from(data), mime),
        MediaKind::Audio(format) => Media::audio_bytes(bytes::Bytes::from(data), format),
    };
    Ok(PreparedMedia {
        label,
        media,
        byte_len,
    })
}

/// What a chip stands for.
#[derive(Clone, Debug)]
pub enum AttachmentPayload {
    PastedText(String),
    FileReference(String),
    Media { media: Media, byte_len: u64 },
}

/// One chip-backed attachment awaiting submit.
#[derive(Clone, Debug)]
pub struct Attachment {
    pub id: u64,
    pub chip: String,
    pub payload: AttachmentPayload,
}

/// Chip-keyed attachments owned by the composer.
#[derive(Clone, Debug, Default)]
pub struct AttachmentLedger {
    next_id: u64,
    // Crate visibility lets the physical composer parity fixture inspect the
    // ledger without making storage details part of the public API.
    pub(crate) entries: Vec<Attachment>,
}

impl AttachmentLedger {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn record_media(&mut self, prepared: PreparedMedia) -> String {
        let id = self.next_id();
        let chip = format!("[{} #{id}]", prepared.label);
        self.entries.push(Attachment {
            id,
            chip: chip.clone(),
            payload: AttachmentPayload::Media {
                media: prepared.media,
                byte_len: prepared.byte_len,
            },
        });
        chip
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Discard every pending attachment while preserving the monotonic chip ID.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Handle Backspace on a registered chip, removing its mask and payload.
    /// Returns false without mutation when ordinary editor Backspace should run.
    ///
    /// Any exact occurrence of a registered chip is eligible, including a
    /// copied duplicate. Removing one revokes its ledger entry, so remaining
    /// copies are literal text and cannot unexpectedly submit the deleted
    /// payload.
    pub fn backspace_chip(&mut self, editor: &mut TextEditor) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let text = editor.text();
        let cursor = editor.cursor();
        let Some((target, _)) = text[..cursor].grapheme_indices(true).next_back() else {
            return false;
        };
        let Some((index, start, end)) =
            self.entries.iter().enumerate().find_map(|(index, entry)| {
                text.match_indices(&entry.chip).find_map(|(start, _)| {
                    let end = start + entry.chip.len();
                    (start < cursor && target < end).then_some((index, start, end))
                })
            })
        else {
            return false;
        };

        // A neighboring combining mark can share a grapheme with a bracket.
        // Replace complete graphemes as required by TextEditor, but preserve
        // every byte outside the chip rather than deleting neighboring text.
        let mut range_start = 0;
        let mut range_end = text.len();
        for (boundary, _) in text.grapheme_indices(true) {
            if boundary <= start {
                range_start = boundary;
            }
            if boundary >= end {
                range_end = boundary;
                break;
            }
        }
        let replacement = format!("{}{}", &text[range_start..start], &text[end..range_end]);
        if !editor.replace_range(range_start..range_end, &replacement) {
            return false;
        }
        self.entries.remove(index);
        true
    }

    /// Collapse a large paste into a chip; the text returns at compose time.
    pub fn attach_pasted_text(&mut self, text: String) -> String {
        let id = self.next_id();
        let lines = text.lines().count();
        let chip = format!("[Pasted text #{id}: {lines} lines]");
        self.entries.push(Attachment {
            id,
            chip: chip.clone(),
            payload: AttachmentPayload::PastedText(text),
        });
        chip
    }

    /// Gate, cap, read, and record a media file. Returns the chip on success.
    pub fn attach_media(
        &mut self,
        path: &Path,
        modalities: ModalitySet,
    ) -> Result<String, AttachError> {
        let paths = [path.to_path_buf()];
        let chip = self
            .attach_media_batch(&paths, modalities)?
            .into_iter()
            .next()
            .expect("a one-file batch returns one chip");
        Ok(chip)
    }

    /// Admit several media files as one atomic batch.
    ///
    /// All files are classified, capability-gated, size-gated, and read before
    /// the ledger is mutated. If any file fails, no partial chips or payloads
    /// remain. The returned chips and ledger entries retain the input order;
    /// repeated paths intentionally receive distinct chips and payloads.
    pub fn attach_media_batch(
        &mut self,
        paths: &[PathBuf],
        modalities: ModalitySet,
    ) -> Result<Vec<String>, AttachError> {
        let prepared = paths
            .iter()
            .map(|path| prepare_media(path, modalities))
            .collect::<Result<Vec<_>, _>>()?;
        let mut chips = Vec::with_capacity(prepared.len());
        for prepared in prepared {
            chips.push(self.record_media(prepared));
        }
        Ok(chips)
    }

    /// Atomically admit supported files from an explicit paste/drop payload and
    /// return the same payload with each admitted path replaced by its chip.
    ///
    /// Ordinary source files remain literal text. A malformed or mixed
    /// path/prose payload returns `Ok(None)`, while a media read/capability
    /// failure returns an error before the ledger is changed.
    pub fn attach_explicit_paths(
        &mut self,
        text: &str,
        modalities: ModalitySet,
    ) -> Result<Option<String>, AttachError> {
        let dropped = match explicit_dropped_paths(text) {
            Some(dropped) => dropped,
            None => return Ok(None),
        };
        let prepared = dropped
            .iter()
            .map(|dropped| {
                if media_kind_for_path(&dropped.path).is_some()
                    || unsupported_media_type_for_path(&dropped.path).is_some()
                {
                    prepare_media(&dropped.path, modalities).map(Some)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !prepared.iter().any(Option::is_some)
            && !dropped
                .iter()
                .any(|dropped| file_kind_for_path(&dropped.path).is_some())
        {
            return Ok(None);
        }

        let mut replacements = Vec::new();
        for (dropped, prepared) in dropped.into_iter().zip(prepared) {
            let Some(attachment) = prepared else {
                if file_kind_for_path(&dropped.path).is_none() {
                    continue;
                }
                let label = match file_kind_for_path(&dropped.path) {
                    Some(FileKind::Pdf) => "PDF",
                    None => continue,
                };
                let id = self.next_id();
                let chip = format!("[{label} #{id}]");
                self.entries.push(Attachment {
                    id,
                    chip: chip.clone(),
                    payload: AttachmentPayload::FileReference(
                        dropped.path.to_string_lossy().into_owned(),
                    ),
                });
                replacements.push((dropped.range, chip));
                continue;
            };
            let chip = self.record_media(attachment);
            replacements.push((dropped.range, chip));
        }

        let mut replaced = text.to_owned();
        for (range, chip) in replacements.into_iter().rev() {
            replaced.replace_range(range, &chip);
        }
        Ok(Some(replaced))
    }

    /// Record a PDF as a path reference. The provider boundary has image/audio
    /// media types but no document variant, so the model receives the path as
    /// text and can inspect it with its file tools.
    pub fn attach_file_reference(&mut self, path: &Path) -> Result<String, AttachError> {
        let label = match file_kind_for_path(path) {
            Some(FileKind::Pdf) => "PDF",
            None => return Err(AttachError::Unreadable("unsupported file extension".into())),
        };
        let path = path.to_string_lossy().into_owned();
        let id = self.next_id();
        let chip = format!("[{label} #{id}]");
        self.entries.push(Attachment {
            id,
            chip: chip.clone(),
            payload: AttachmentPayload::FileReference(path),
        });
        Ok(chip)
    }

    /// Put restored steering attachments back. IDs continue from the highest
    /// ever issued.
    pub fn restore(&mut self, entries: Vec<Attachment>) {
        if let Some(highest) = entries.iter().map(|entry| entry.id).max() {
            self.next_id = self.next_id.max(highest);
        }
        self.entries.extend(entries);
    }

    /// Move pending entries out without changing the monotonic chip ID.
    pub(crate) fn take_all(&mut self) -> Vec<Attachment> {
        std::mem::take(&mut self.entries)
    }
}
