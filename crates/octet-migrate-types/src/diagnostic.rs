//! Migration diagnostics: a structured, severity-tagged explanation of why a
//! source item could not be used.
//!
//! A diagnostic is separate from [`crate::Result`] because it is *content*, not
//! failure. An unmapped source item is a successful migration that recorded why
//! it could not map, so it travels inside a
//! [`MigrationOutcome`](crate::MigrationOutcome) rather than out of a fallible
//! constructor. That distinction is the reason this type has its own module: the
//! failure path and the report path must not be able to be confused by sharing a
//! representation.

use serde::Serialize;

use crate::error::Result;
use crate::validate::{validate_diagnostic_reason, validate_normalized_path, ResourceUsage};

/// Severity assigned to a migration diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// The source item was retained but needs user review before it can be used.
    Warning,
    /// The source item cannot be used without an explicit correction or port.
    Error,
}

/// An actionable migration diagnostic for a normalized source path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub(super) path: String,
    pub(super) severity: DiagnosticSeverity,
    pub(super) reason: String,
}

impl Diagnostic {
    /// Creates an actionable diagnostic.
    ///
    /// `path` must be [`crate::ROOT_DIAGNOSTIC_PATH`] or a normalized source-relative
    /// path with no default-ignorable scalar. `reason` must contain at least one
    /// scalar that is neither Unicode whitespace nor default-ignorable. C0/C1,
    /// line/paragraph separators, and bidirectional controls are rejected in
    /// both fields. Validation does not normalize or rewrite either string, so
    /// visible international text and non-bidi variation selectors in a visible
    /// reason remain literal.
    pub fn new(
        path: impl Into<String>,
        severity: DiagnosticSeverity,
        reason: impl Into<String>,
    ) -> Result<Self> {
        let diagnostic = Self {
            path: path.into(),
            severity,
            reason: reason.into(),
        };
        let mut usage = ResourceUsage::default();
        diagnostic.validate_with_usage(&mut usage)?;
        Ok(diagnostic)
    }

    /// Returns the normalized source path or [`crate::ROOT_DIAGNOSTIC_PATH`].
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the diagnostic severity.
    pub const fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    /// Returns the actionable explanation.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        usage.take_diagnostic()?;
        validate_normalized_path(&self.path, "diagnostic path", usage)?;
        validate_diagnostic_reason(&self.reason, usage)
    }
}
