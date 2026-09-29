//! The bounded partial-assistant frame journal: the disposable sidecar a
//! streaming attempt writes beside the session so a killed process can
//! republish partial progress on reopen.
//!
//! Separate because this is the one part of the session subsystem that is
//! never a session record and never provider-visible. It owns its own
//! path, its own byte/frame bounds and its own torn-tail rule, and
//! conflating it with the entry log is exactly how a partial assistant
//! message would end up being replayed as a completed one.

use super::*;

/// Maximum bytes retained in one durable partial-assistant frame journal.
///
/// The journal is a bounded, disposable recovery aid: once the bound is
/// reached the prefix already written is kept and later frames are dropped, so
/// a crash mid-stream can never leave an unbounded file behind.
pub const MAX_PARTIAL_FRAME_JOURNAL_BYTES: usize = 1024 * 1024;

/// Maximum frames retained in one durable partial-assistant frame journal.
pub const MAX_PARTIAL_FRAME_JOURNAL_FRAMES: usize = 8192;

/// Durable, bounded journal of [`octet_ai::AssistantMessageFrame`]s for one
/// in-flight assistant attempt.
///
/// Frame deltas are *not* session entries: they never enter provider-visible
/// context and never affect usage, cost, or uncertainty accounting. A journal
/// is an owner-only sidecar file beside the session log that exists only while
/// an attempt is streaming. It is removed at terminal settlement, so a
/// completed turn is never replayed as partial progress.
///
/// Writes are deliberately not `fsync`ed: the journal must survive a killed
/// *process* (the bytes are already in the kernel), not power loss, because it
/// is discarded the moment the authoritative assistant entry is durably
/// appended. The sidecar is never parsed as a session record.
pub struct AssistantFrameJournal {
    path: PathBuf,
    pub(super) file: File,
    bytes: usize,
    frames: usize,
    bounded: bool,
    settled: bool,
}

impl AssistantFrameJournal {
    /// Records one frame unless the journal's bounds are already reached.
    ///
    /// A bounded journal keeps its existing prefix and silently drops later
    /// frames: a recovery aid must never fail or destabilize the provider
    /// stream it observes.
    pub fn append(&mut self, frame: &octet_ai::AssistantMessageFrame) -> Result<(), SessionError> {
        if self.settled || self.bounded {
            return Ok(());
        }
        let mut line =
            serde_json::to_vec(frame).map_err(|error| SessionError::Serde(error.to_string()))?;
        line.push(b'\n');
        if self.frames >= MAX_PARTIAL_FRAME_JOURNAL_FRAMES
            || self.bytes.saturating_add(line.len()) > MAX_PARTIAL_FRAME_JOURNAL_BYTES
        {
            self.bounded = true;
            return Ok(());
        }
        self.file.write_all(&line)?;
        self.bytes += line.len();
        self.frames += 1;
        Ok(())
    }

    /// Removes the journal after terminal settlement.
    ///
    /// Called once an attempt reaches its terminal event, so the sequence of
    /// partial frames is never mistaken for an in-flight turn on the next
    /// start. Idempotent.
    pub fn settle(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        let _ = self.file.sync_data();
        // Read from the descriptor we created, then compare-and-delete. A
        // replaced pathname or parent must not redirect cleanup to a new file.
        if self.file.rewind().is_ok() {
            let mut bytes = Vec::new();
            if Read::by_ref(&mut self.file)
                .take((MAX_PARTIAL_FRAME_JOURNAL_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .is_ok()
            {
                let _ = crate::secure_fs::remove_private_file_if_unchanged(
                    &self.path,
                    &bytes,
                    MAX_PARTIAL_FRAME_JOURNAL_BYTES,
                );
            }
        }
    }

    /// Durable sidecar path of this journal.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes written so far.
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    /// Frames written so far.
    pub fn retained_frames(&self) -> usize {
        self.frames
    }

    /// Whether the journal stopped accepting frames at its bound.
    pub fn is_bounded(&self) -> bool {
        self.bounded
    }
}

impl Session {
    /// Durable sidecar path used for partial-assistant recovery.
    ///
    /// Kept beside the session so a journal and the log it belongs to move
    /// together. It is never a session record and is never replayed as one.
    pub(super) fn partial_assistant_frames_path(&self) -> Result<PathBuf, SessionError> {
        let name = self.path.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "session has no filename")
        })?;
        let mut name = name.to_os_string();
        name.push(".partial-assistant-frames");
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        Ok(parent.canonicalize()?.join(name))
    }

    /// Opens a fresh durable journal for one in-flight assistant attempt.
    ///
    /// A stale journal must first be consumed through [`Self::take_partial_assistant`].
    /// Exclusive, descriptor-bound creation never follows links or truncates an
    /// existing target. The file is created owner-only next to the session log.
    pub fn begin_assistant_frame_journal(&mut self) -> Result<AssistantFrameJournal, SessionError> {
        let path = self.partial_assistant_frames_path()?;
        let file = crate::secure_fs::create_regular_file_for_append(&path)
            .map_err(partial_journal_file_error)?;
        Ok(AssistantFrameJournal {
            path,
            file,
            bytes: 0,
            frames: 0,
            bounded: false,
            settled: false,
        })
    }

    /// Consumes any partial assistant turn left by a killed stream.
    ///
    /// Reduces the durable frame prefix into an [`octet_ai::AssistantMessage`]
    /// (partial text/reasoning content only — a partial tool call is not a
    /// result and never becomes one), then removes the journal so a partial is
    /// published exactly once. `Ok(None)` means the last attempt settled
    /// terminally (or never started), so there is no progress to republish.
    pub fn take_partial_assistant(
        &mut self,
    ) -> Result<Option<octet_ai::AssistantMessage>, SessionError> {
        let path = self.partial_assistant_frames_path()?;
        let bytes = match crate::secure_fs::read_private_file_bounded(
            &path,
            MAX_PARTIAL_FRAME_JOURNAL_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(crate::secure_fs::SecureFileError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(partial_journal_file_error(error)),
        };
        let frames = read_partial_assistant_frames(&bytes)?;
        // Never acknowledge consumption if cleanup failed or a replacement
        // changed the snapshot. Otherwise a later start could republish it.
        crate::secure_fs::remove_private_file_if_unchanged(
            &path,
            &bytes,
            MAX_PARTIAL_FRAME_JOURNAL_BYTES,
        )
        .map_err(partial_journal_file_error)?;
        if frames.is_empty() {
            return Ok(None);
        }
        octet_ai::reduce_assistant_message_frames(&frames)
            .map_err(|error| SessionError::Serde(error.to_string()))
    }
}

/// Reads a partial-assistant frame journal, keeping the valid prefix.
///
/// A torn final line is the expected crash shape; it is dropped rather than
/// treated as corruption. An oversized file is rejected instead of read.
pub(super) fn read_partial_assistant_frames(
    bytes: &[u8],
) -> Result<Vec<octet_ai::AssistantMessageFrame>, SessionError> {
    if bytes.len() > MAX_PARTIAL_FRAME_JOURNAL_BYTES {
        return Err(SessionError::Limit(
            "partial assistant frame journal exceeded its byte bound".into(),
        ));
    }
    let mut frames = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        // Even a syntactically valid final value is uncommitted without the
        // newline; invalid UTF-8 in a torn tail is discarded the same way.
        if !line.ends_with(b"\n") {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if frames.len() == MAX_PARTIAL_FRAME_JOURNAL_FRAMES {
            return Err(SessionError::Limit(
                "partial assistant frame journal exceeded its frame bound".into(),
            ));
        }
        match serde_json::from_slice::<octet_ai::AssistantMessageFrame>(line) {
            Ok(frame) => frames.push(frame),
            Err(_) => break,
        }
    }
    Ok(frames)
}

pub(super) fn partial_journal_file_error(error: crate::secure_fs::SecureFileError) -> SessionError {
    match error {
        crate::secure_fs::SecureFileError::Io(error) => SessionError::Io(error),
        crate::secure_fs::SecureFileError::TooLarge { .. } => {
            SessionError::Limit(error.to_string())
        }
        error => SessionError::Io(std::io::Error::other(error)),
    }
}
