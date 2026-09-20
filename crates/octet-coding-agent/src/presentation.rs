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

#[cfg(test)]
pub use changed_files::{
    content_hash, project_changed_files, reported_output_hash, trusted_output_hash,
    validate_changed_file_evidence, WorkspaceSnapshot,
};
pub use formatting::{
    compact_context_limit, format_duration, format_token_rate, format_token_rate_value,
    PriceDisplay,
};
#[cfg(test)]
pub use model::resolve_model_display_name;
pub use model::{
    derive_model_display_name, model_display_name_variants, provider_lifecycle_label,
    provider_status_name, ModelDisplayMetadata,
};
#[cfg(test)]
pub use run::RunSummary;
pub use run::{RunId, RunOutcome, RunPhase, RunTracker};
pub use tool_display::{
    concise_line, is_hidden_tool_detail, summarize_tool, summarize_tool_with_workspace,
    tool_failure_reason, tool_result_is_failure, ToolDisplay,
};
