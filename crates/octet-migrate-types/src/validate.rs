//! Bound enforcement: the predicates the checked types cite, and the per-document
//! resource budget that bounds them in aggregate.
//!
//! A per-value check answers "is *this* field legal?"; only
//! [`ResourceUsage`] answers "is this whole document still within its budget?".
//! Splitting the two is what stops a document from smuggling an unbounded
//! aggregate through a field that is individually legal — the reason the setup
//! and report constructors thread one `ResourceUsage` through every child
//! instead of validating each child in isolation. The predicates are also shared
//! verbatim: a model name, a skill name, an MCP server and a permission are
//! different fields with one set of rules, and the whole point of listing them
//! side by side is that a future field cannot quietly get a laxer copy.

use std::collections::BTreeMap;

use crate::error::{Result, ValidationError};
use crate::limits::{
    MAX_DIAGNOSTICS, MAX_MAP_ENTRIES, MAX_PORTABLE_JSON_INTEGER, MAX_STRING_BYTES,
    MAX_TOTAL_DECODED_COLLECTION_ENTRIES, MAX_TOTAL_DECODED_RECORDS,
    MAX_TOTAL_DECODED_STRING_BYTES, ROOT_DIAGNOSTIC_PATH,
};
use crate::markdown::{is_default_ignorable_scalar, requires_visible_escape};
use crate::migrated::{
    McpServer, MigrationOutcome, MigrationOutcomeInner, Model, Permission, Skill,
};

pub(super) fn validate_model_outcome(
    outcome: &MigrationOutcome<Model>,
    usage: &mut ResourceUsage,
) -> Result<()> {
    validate_migration_outcome(outcome, usage, Model::validate_with_usage)
}

pub(super) fn validate_skill_outcome(
    outcome: &MigrationOutcome<Skill>,
    usage: &mut ResourceUsage,
) -> Result<()> {
    validate_migration_outcome(outcome, usage, Skill::validate_with_usage)
}

pub(super) fn validate_mcp_server_outcome(
    outcome: &MigrationOutcome<McpServer>,
    usage: &mut ResourceUsage,
) -> Result<()> {
    validate_migration_outcome(outcome, usage, McpServer::validate_with_usage)
}

pub(super) fn validate_permission_outcome(
    outcome: &MigrationOutcome<Permission>,
    usage: &mut ResourceUsage,
) -> Result<()> {
    validate_migration_outcome(outcome, usage, Permission::validate_with_usage)
}

pub(super) fn validate_migration_outcome<T, F>(
    outcome: &MigrationOutcome<T>,
    usage: &mut ResourceUsage,
    validate_value: F,
) -> Result<()>
where
    F: FnOnce(&T, &mut ResourceUsage) -> Result<()>,
{
    match &outcome.inner {
        MigrationOutcomeInner::Mapped { path, value } => {
            validate_source_path(path, usage)?;
            validate_value(value, usage)
        }
        MigrationOutcomeInner::Unmapped { diagnostic } => diagnostic.validate_with_usage(usage),
    }
}

pub(super) fn validate_metadata_map(
    values: &BTreeMap<String, String>,
    name: &str,
    usage: &mut ResourceUsage,
) -> Result<()> {
    validate_collection_count(values.len(), MAX_MAP_ENTRIES, name)?;
    for (key, value) in values {
        usage.take_collection_entry("comparison metadata")?;
        usage.take_string("comparison metadata key", key)?;
        usage.take_string("comparison metadata value", value)?;
    }
    Ok(())
}

pub(super) fn validate_collection_count(count: usize, maximum: usize, name: &str) -> Result<()> {
    if count > maximum {
        return Err(ValidationError::new(format!(
            "{name} exceeds its limit of {maximum} entries"
        )));
    }
    Ok(())
}

pub(super) fn ensure_can_append(current: usize, maximum: usize, name: &str) -> Result<()> {
    if current >= maximum {
        return Err(ValidationError::new(format!(
            "{name} exceeds its limit of {maximum} entries"
        )));
    }
    Ok(())
}

pub(super) fn validate_portable_metric(value: u64, field: &str) -> Result<()> {
    if value > MAX_PORTABLE_JSON_INTEGER {
        return Err(ValidationError::new(format!(
            "{field} must be an exact portable JSON integer no greater than {MAX_PORTABLE_JSON_INTEGER}"
        )));
    }
    Ok(())
}

pub(super) fn validate_source_path(path: &str, usage: &mut ResourceUsage) -> Result<()> {
    usage.take_string("mapped path", path)
}

pub(super) fn validate_normalized_path(
    path: &str,
    field: &str,
    usage: &mut ResourceUsage,
) -> Result<()> {
    usage.take_string(field, path)?;
    if path.is_empty() {
        return Err(ValidationError::new(format!("{field} must not be empty")));
    }
    if path.trim() != path {
        return Err(ValidationError::new(format!(
            "{field} must not have leading or trailing whitespace"
        )));
    }
    if path.chars().any(requires_visible_escape) {
        return Err(ValidationError::new(format!(
            "{field} must not contain control or bidirectional-format characters"
        )));
    }
    if path.chars().any(is_default_ignorable_scalar) {
        return Err(ValidationError::new(format!(
            "{field} must not contain default-ignorable formatting or tag characters"
        )));
    }
    if !has_visible_diagnostic_scalar(path) {
        return Err(ValidationError::new(format!(
            "{field} must contain a visible non-whitespace scalar"
        )));
    }
    if path == ROOT_DIAGNOSTIC_PATH {
        return Ok(());
    }
    if path.contains('\\') || path.starts_with('/') || path.ends_with('/') {
        return Err(ValidationError::new(format!(
            "{field} must be a normalized source-relative path"
        )));
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain empty, '.' or '..' segments"
        )));
    }
    Ok(())
}

pub(super) fn validate_diagnostic_reason(reason: &str, usage: &mut ResourceUsage) -> Result<()> {
    usage.take_string("diagnostic reason", reason)?;
    if reason.chars().any(requires_visible_escape) {
        return Err(ValidationError::new(
            "diagnostic reason must not contain control or bidirectional-format characters",
        ));
    }
    if !has_visible_diagnostic_scalar(reason) {
        return Err(ValidationError::new(
            "diagnostic reason must contain a visible non-whitespace, non-default-ignorable scalar",
        ));
    }
    Ok(())
}

pub(super) fn has_visible_diagnostic_scalar(value: &str) -> bool {
    value
        .chars()
        .any(|character| !character.is_whitespace() && !is_default_ignorable_scalar(character))
}

#[derive(Default)]
pub(super) struct ResourceUsage {
    string_bytes: usize,
    records: usize,
    collection_entries: usize,
    diagnostics: usize,
}

impl ResourceUsage {
    pub(super) fn take_string(&mut self, field: &str, value: &str) -> Result<()> {
        if value.len() > MAX_STRING_BYTES {
            return Err(ValidationError::new(format!(
                "{field} exceeds MAX_STRING_BYTES ({MAX_STRING_BYTES} bytes)"
            )));
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(value.len())
            .ok_or_else(|| ValidationError::new("decoded string-byte counter overflow"))?;
        if self.string_bytes > MAX_TOTAL_DECODED_STRING_BYTES {
            return Err(ValidationError::new(format!(
                "decoded payload strings exceed MAX_TOTAL_DECODED_STRING_BYTES ({MAX_TOTAL_DECODED_STRING_BYTES} bytes)"
            )));
        }
        Ok(())
    }

    pub(super) fn take_record(&mut self, kind: &str) -> Result<()> {
        self.records = self
            .records
            .checked_add(1)
            .ok_or_else(|| ValidationError::new("decoded record counter overflow"))?;
        if self.records > MAX_TOTAL_DECODED_RECORDS {
            return Err(ValidationError::new(format!(
                "decoded {kind} records exceed MAX_TOTAL_DECODED_RECORDS ({MAX_TOTAL_DECODED_RECORDS})"
            )));
        }
        Ok(())
    }

    pub(super) fn take_collection_entry(&mut self, kind: &str) -> Result<()> {
        self.collection_entries = self
            .collection_entries
            .checked_add(1)
            .ok_or_else(|| ValidationError::new("decoded collection-entry counter overflow"))?;
        if self.collection_entries > MAX_TOTAL_DECODED_COLLECTION_ENTRIES {
            return Err(ValidationError::new(format!(
                "decoded {kind} collection entries exceed MAX_TOTAL_DECODED_COLLECTION_ENTRIES ({MAX_TOTAL_DECODED_COLLECTION_ENTRIES})"
            )));
        }
        Ok(())
    }

    pub(super) fn take_diagnostic(&mut self) -> Result<()> {
        self.diagnostics = self
            .diagnostics
            .checked_add(1)
            .ok_or_else(|| ValidationError::new("diagnostic counter overflow"))?;
        if self.diagnostics > MAX_DIAGNOSTICS {
            return Err(ValidationError::new(format!(
                "diagnostics exceed their limit of {MAX_DIAGNOSTICS} entries"
            )));
        }
        Ok(())
    }
}
