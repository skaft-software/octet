//! The crate's single error type and its result alias.
//!
//! A migration document fails in exactly two ways that a caller can act on: the
//! document is malformed for this schema version, or it violates a documented
//! bound. Both are [`ValidationError`]; a version mismatch is a distinguished
//! [`SchemaVersionMismatch`] carried *inside* it rather than a second error type,
//! so a caller that only wants "was this rejected" never has to match on two
//! enums. This module owns that decision, and the `Display`/`Error` impls that
//! make the message a caller logs match the bound it actually broke.

use std::fmt;

/// An unsupported schema version encountered while decoding a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaVersionMismatch {
    /// The schema-bearing document that rejected the version.
    pub document: &'static str,
    /// The exact portable integer supplied by the input document.
    pub found: u64,
    /// The version supported by this crate release.
    pub expected: u32,
}

impl fmt::Display for SchemaVersionMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported {} schema version {}; expected {}",
            self.document, self.found, self.expected
        )
    }
}

impl std::error::Error for SchemaVersionMismatch {}

/// A decoding, resource-limit, or typed-construction validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    message: String,
}

impl ValidationError {
    /// Returns the stable, human-readable validation failure message.
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(super) fn from_serde(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ValidationError {}

/// A result returned by validated schema constructors and wire entry points.
pub type Result<T> = std::result::Result<T, ValidationError>;
