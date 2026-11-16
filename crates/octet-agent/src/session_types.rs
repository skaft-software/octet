//! Shared entry identifiers and errors, without session persistence or replay.
use serde::{Deserialize, Serialize};

/// Identifier of a session entry. Unique within one session file.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct EntryId(pub String);

/// Session errors.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Filesystem failure.
    #[error("session io error: {0}")]
    Io(#[from] std::io::Error),
    /// A record failed to serialize.
    #[error("session serialization error: {0}")]
    Serde(String),
    /// A completed (non-final) record failed to parse or violated an invariant.
    #[error("corrupt session record at line {line}: {message}")]
    Corrupt {
        /// 1-based line number of the offending record.
        line: usize,
        /// What was wrong with it.
        message: String,
    },
    /// A configured session parsing/resource bound was exceeded.
    #[error("session limit exceeded: {0}")]
    Limit(String),
    /// Another session handle changed the file after this one was opened.
    #[error("session was modified by another process; reopen it before writing")]
    ConcurrentModification,
    /// An operation referenced an entry ID that does not exist.
    #[error("unknown session entry: {0:?}")]
    UnknownEntry(EntryId),
    /// A compaction/checkpoint entry is not an ancestor of the current head.
    #[error("entry {0:?} is not an ancestor of the current head")]
    NotAncestor(EntryId),
    /// An operation requires a non-empty session.
    #[error("the session has no current head")]
    EmptySession,
    /// No durable checkpoint exists for the supplied prompt entry.
    #[error("no checkpoint exists for prompt entry {0:?}")]
    UnknownCheckpoint(EntryId),
    /// A Responses assistant sidecar belongs to another endpoint/model route.
    #[error(
        "Responses replay route mismatch for assistant {assistant:?}: expected {expected_endpoint}/{expected_model}, found {actual_endpoint}/{actual_model}"
    )]
    ResponsesRouteMismatch {
        /// Assistant entry whose sidecar was inspected.
        assistant: EntryId,
        /// Endpoint selected for the new request.
        expected_endpoint: String,
        /// Model selected for the new request.
        expected_model: String,
        /// Endpoint recorded on the sidecar.
        actual_endpoint: String,
        /// Model recorded on the sidecar.
        actual_model: String,
    },
    /// A Responses sidecar was not appended at its required branch boundary.
    #[error("invalid Responses sidecar: {0}")]
    InvalidResponsesSidecar(String),
}
