#![deny(missing_docs)]

//! Versioned, bounded wire schemas for migrated setups and paired comparisons.
//!
//! The migration schema records both successful mappings and unmapped source
//! items. The comparison schema is JSON-first: Markdown is rendered only from a
//! validated JSON report rather than maintained as a second report format.
//!
//! # Validated decoding boundary
//!
//! [`MigratedSetup`], [`CompareReportHeader`], and [`CompareReport`] deliberately
//! do **not** implement [`serde::Deserialize`]. A `serde_json::Value` has already
//! collapsed duplicate object names, so `serde_json::from_value` cannot prove
//! that a wire document was duplicate-free. Decode wire artifacts only through
//! their [`from_json`](MigratedSetup::from_json) methods, which inspect the raw
//! JSON token stream with bounded visitors before strict schema decoding.
//!
//! Checked typed constructors are the other way to make a validated value. All
//! fields are private, and constructors, mutators, canonicalization, and
//! rendering re-apply the documented bounds. To round-trip a serialized value,
//! call the matching `from_json` method on its canonical JSON; do not use a
//! generic Serde deserializer or a `serde_json::Value` conversion.
//!
//! ```compile_fail
//! use octet_migrate_types::CompareReport;
//!
//! let value = serde_json::json!({"header": {}, "tasks": []});
//! let _: CompareReport = serde_json::from_value(value).unwrap();
//! ```
//!
//! ```compile_fail
//! use octet_migrate_types::MigratedSetup;
//!
//! let value = serde_json::json!({});
//! let _: MigratedSetup = serde_json::from_value(value).unwrap();
//! ```
//!
//! ```compile_fail
//! use octet_migrate_types::CompareReportHeader;
//!
//! let value = serde_json::json!({});
//! let _: CompareReportHeader = serde_json::from_value(value).unwrap();
//! ```
//!
//! # Schema versions
//!
//! V1 readers reject every unsupported schema version. Readers first perform a
//! bounded raw-token version probe, so a version mismatch wins over additive or
//! wrong-typed sibling fields in an otherwise bounded JSON document. Readers do
//! not guess an upgrade or downgrade; callers must explicitly migrate a newer
//! or older document before consuming it.

// --- Module map -----------------------------------------------------------
// Every public name below is re-exported here, so this file is the crate's
// API surface and nothing else in it is part of it.
//
//   limits      every bound a v1 reader enforces, named once
//   error       the single error type, the version mismatch inside it, Result
//   diagnostic  a migration diagnostic (content, not failure)
//   migrated    the checked migrated-setup types
//   compare     the checked comparison-report types and its Markdown boundary
//   validate    the shared bound predicates and the per-document budget
//   json_bounds the bounded raw-JSON preflight (bytes, depth, duplicate names)
//   wire        the raw wire structs and their one-way conversions
//   version     the minimal raw-token schema-version probes
//   markdown    Markdown cell escaping for the report
//
// The seams follow the order a document travels: probe its version, bound its
// raw bytes, decode its wire structs, apply the shared predicates against one
// aggregate budget, and only then hand the caller a checked value. A reader
// that skipped a step would be a reader that trusted a bound it had not
// measured, so the steps are modules rather than a call order in one file.

mod compare;
mod diagnostic;
mod error;
mod json_bounds;
mod limits;
mod markdown;
mod migrated;
mod validate;
mod version;
mod wire;

pub use compare::{
    render_compare_report_markdown, CompareReport, CompareReportHeader, CompareTaskRow,
};
pub use diagnostic::{Diagnostic, DiagnosticSeverity};
pub use error::{Result, SchemaVersionMismatch, ValidationError};
pub use limits::{
    COMPARE_REPORT_SCHEMA_VERSION, MAX_DIAGNOSTICS, MAX_JSON_INPUT_BYTES, MAX_JSON_NESTING,
    MAX_LIST_ENTRIES, MAX_MAP_ENTRIES, MAX_MCP_ARGUMENTS, MAX_MCP_SERVERS, MAX_MODELS,
    MAX_PERMISSIONS, MAX_PORTABLE_JSON_INTEGER, MAX_RENDERED_MARKDOWN_BYTES, MAX_SKILLS,
    MAX_STRING_BYTES, MAX_TASKS, MAX_TOTAL_DECODED_COLLECTION_ENTRIES, MAX_TOTAL_DECODED_RECORDS,
    MAX_TOTAL_DECODED_STRING_BYTES, MAX_TOTAL_JSON_ENTRIES, MAX_TOTAL_JSON_STRING_BYTES,
    MIGRATED_SETUP_SCHEMA_VERSION, ROOT_DIAGNOSTIC_PATH,
};
pub use migrated::{
    McpServer, McpTransport, McpTransportKind, MigratedSetup, MigrationOutcome, Model, Permission,
    PermissionDecision, Skill,
};
