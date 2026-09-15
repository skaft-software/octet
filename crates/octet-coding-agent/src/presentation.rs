#![allow(missing_docs)]

//! Presentation-owned projections and compatibility reexports.
//!
//! Execution, persistence, and telemetry producers remain outside this facade.
//! The modules below own model identity, tool display, formatting, changed-file
//! evidence, request timing, and run lifecycle projection respectively.

pub mod changed_files;
pub mod formatting;
pub mod model;
pub mod request;
pub mod run;
pub mod tool_display;

#[cfg(test)]
mod ownership_tests;

pub use changed_files::{
    content_hash, is_valid_sha256_hex, output_contains_hash_token, project_changed_files,
    reported_output_hash, trusted_output_hash, validate_changed_file_evidence,
    ChangedFileCandidate, ChangedFileProjection, WorkspaceFileSnapshot, WorkspaceSnapshot,
};
pub use formatting::{
    compact_context_limit, format_duration, format_token_rate, format_token_rate_value,
    PriceDisplay,
};
pub use model::{
    derive_model_display_name, model_display_name_variants, provider_lifecycle_label,
    provider_status_name, resolve_model_display_name, ModelDisplayMetadata,
};
pub use request::{
    RequestThroughput, RequestThroughputTracker, RequestTiming, RequestTimingSample,
};
pub use run::{RunId, RunOutcome, RunPhase, RunPresentation, RunSummary, RunTracker, RunUpdate};
pub use tool_display::{
    compact_path, concise_line, display_path, is_hidden_tool_detail, normalize_shell_command,
    summarize_tool, summarize_tool_with_workspace, tool_failure_reason, tool_result_is_failure,
    ToolDisplay,
};
