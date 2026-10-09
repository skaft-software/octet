//! The built-in tools (`read`, `edit`, `write`, `bash`), the [`CoreTools`]
//! extension that registers them, and the tool-layer durability primitives they
//! need.
//!
//! Core tools implement the same [`Tool`](crate::Tool) trait as third-party
//! tools. [`CoreTools`] additionally marks their native identities so reviewed
//! builtin overrides cannot replace unrelated tools with familiar names.
//!
//! The model-visible core surface is `read`/`write`/`edit`/`bash`. Directory
//! listing, file discovery, and content search use shell commands through
//! `bash`; there are no separate `search`/`ls`/`find`/`grep` tools.
//!
//! Three modules here are not tools but the harness-side primitives Pi defines
//! next to them, landed in the tool layer because `session.rs`/`agent.rs` are
//! outside this change's scope: [`durability`] (durable invocation-scoped
//! partial-output checkpoints and replay memos), [`deferred`] (durable
//! suspend/resume with poll permits), and [`summarization`] (the shared
//! summarization retry policy and its typed outcomes). Each documents the
//! consumer that still has to be wired.

mod bash;
mod edit;
mod powershell;
mod read;
mod shell_environment;
mod write;

pub mod deferred;
pub mod durability;
pub mod summarization;

#[cfg(windows)]
pub use bash::resolve_windows_shell;
pub use bash::{
    BashCheckpointPublisher, BashCheckpointStats, BashTool, CheckpointedBashTool,
    BASH_CHECKPOINT_INTERVAL, BASH_CHECKPOINT_MAX_BYTES, MIN_BASH_CHECKPOINT_INTERVAL,
};
pub use deferred::{
    prepare_deferred_poll, suspend_deferred_response, DeferredHandle, DeferredHandleRejection,
    DeferredPhase, DeferredPollIntent, DeferredPollOutcome, DeferredPollPermit,
    DeferredPollPreparation, DeferredPollRefusal, DeferredPollRefusalKind,
    DeferredResponseDeclaration, DeferredResume, DeferredStopReason, DeferredSuspendDecision,
    DeferredSuspendFailure, DeferredSuspendFailureKind, DeferredSuspended, ModelIdentity,
    SuspendedRunObservation, UnknownPollReplacement, INVALID_DEFERRED_HANDLE_DIAGNOSTIC,
};
pub use durability::{
    DurableInvocationStore, InterruptedInvocation, InvocationError, InvocationHandle,
    InvocationOutcome, InvocationScope, InvocationState, MemoLookup, Settlement, StoreLimits,
    UnsafeRecovery, INTERRUPTED_OUTCOME_UNKNOWN_MARKER, MEMO_NAMESPACE, PARTIAL_OUTPUT_NAMESPACE,
};
pub use edit::EditTool;
pub use powershell::PowerShellTool;
pub use read::ReadTool;
pub use shell_environment::{SessionShellTool, ShellSessionEnvironment};
pub use summarization::{
    run_summarization_with_retry, CompactionFailure, CompactionFailureKind, CompactionStepOutcome,
    SummarizationAttempt, SummarizationDiagnostic, SummarizationFailure, SummarizationFailureKind,
    SummarizationOutcome, SummarizationRetryPolicy, SummarizationRetryScheduled, SummarizationRun,
};
pub use write::WriteTool;

use crate::effect::ToolPolicyDenialCode;
use crate::extension::{Extension, ExtensionHost};
use crate::tool::ToolError;

/// Hard cap for one file loaded by read/edit/write preview and conflict checks.
pub(crate) const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

/// Hard cap for one local path spelling accepted at the tool boundary.
pub(crate) const MAX_TOOL_PATH_BYTES: usize = 32 * 1024;

/// Validate only lexical path properties before effect admission. This must not
/// query filesystem state: classification must remain deterministic and must
/// not leak host-path metadata before policy authorization.
pub(crate) fn validate_effect_path(
    path: &str,
    allow_external_paths: bool,
) -> Result<(), ToolError> {
    if path.is_empty() {
        return Err(ToolError::new(
            "invalid arguments: `path` must be non-empty",
        ));
    }
    if path.len() > MAX_TOOL_PATH_BYTES {
        return Err(ToolError::new(format!(
            "invalid arguments: `path` is {} bytes (limit {MAX_TOOL_PATH_BYTES})",
            path.len()
        )));
    }
    if path.as_bytes().contains(&0) {
        return Err(ToolError::new(
            "invalid arguments: `path` must not contain NUL",
        ));
    }
    if allow_external_paths {
        return Ok(());
    }

    let path_value = std::path::Path::new(path);
    if path_value.is_absolute()
        || path_value.components().any(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ToolError::policy_denied(
            ToolPolicyDenialCode::WorkspaceConfinement,
            format!("invalid arguments: absolute paths are not allowed: `{path}`"),
        ));
    }
    if path_value
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(ToolError::policy_denied(
            ToolPolicyDenialCode::WorkspaceConfinement,
            format!("invalid arguments: parent (`..`) path components are not allowed: `{path}`"),
        ));
    }
    if matches!(path, "~") || path.starts_with("~/") || path.starts_with("~\\") {
        return Err(ToolError::policy_denied(
            ToolPolicyDenialCode::WorkspaceConfinement,
            format!("invalid arguments: home-relative paths are not allowed: `{path}`"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_expected_hash(value: Option<&serde_json::Value>) -> Result<(), ToolError> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(value) = value.as_str() else {
        return Err(ToolError::new(
            "invalid arguments: `expected_hash` must be a string",
        ));
    };
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ToolError::new(
            "invalid arguments: `expected_hash` must be 64 lowercase hexadecimal characters",
        ));
    }
    Ok(())
}

/// Builtin identities eligible for an explicitly reviewed API 0.4 override.
/// Publication still requires the actual CoreTools registration, not just a name.
pub(crate) const BUILTIN_TOOL_NAMES: &[&str] = &["read", "edit", "write", "bash", "powershell"];

/// Extension registering the built-in tools through the native registry.
pub struct CoreTools;

impl Extension for CoreTools {
    fn register(&self, host: &mut ExtensionHost) {
        host.builtin_tool(ReadTool);
        host.builtin_tool(EditTool);
        host.builtin_tool(WriteTool);
        host.builtin_tool(BashTool);
        // Optional at the product allowlist boundary; never a bash fallback.
        #[cfg(windows)]
        host.builtin_tool(PowerShellTool::default());
    }
}

/// Deserializes model-provided arguments into a typed argument struct,
/// converting schema mismatches into a clear tool error for the model.
pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(
    args: serde_json::Value,
) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::new(format!("invalid arguments: {e}")))
}

const MAX_UNIFIED_DIFF_BYTES: usize = 16 * 1024;
const UNIFIED_DIFF_TRUNCATION_MARKER: &str =
    "\n... unified diff truncated; remaining content omitted ...\n";

struct UnifiedDiffWriter {
    output: String,
    truncated: bool,
}

impl UnifiedDiffWriter {
    fn new() -> Self {
        Self {
            output: String::with_capacity(MAX_UNIFIED_DIFF_BYTES),
            truncated: false,
        }
    }

    fn push_bounded(&mut self, value: &str) {
        if self.truncated {
            return;
        }
        if value.len() <= MAX_UNIFIED_DIFF_BYTES.saturating_sub(self.output.len()) {
            self.output.push_str(value);
            return;
        }

        let content_limit = MAX_UNIFIED_DIFF_BYTES - UNIFIED_DIFF_TRUNCATION_MARKER.len();
        truncate_utf8(&mut self.output, content_limit);
        let mut keep = content_limit
            .saturating_sub(self.output.len())
            .min(value.len());
        while keep > 0 && !value.is_char_boundary(keep) {
            keep -= 1;
        }
        self.output.push_str(&value[..keep]);
        self.output.push_str(UNIFIED_DIFF_TRUNCATION_MARKER);
        self.truncated = true;
    }

    fn push_line(&mut self, prefix: &str, line: &str) -> bool {
        self.push_bounded(prefix);
        self.push_bounded(line);
        self.push_bounded("\n");
        !self.truncated
    }

    fn finish(self) -> String {
        self.output
    }
}

impl std::fmt::Write for UnifiedDiffWriter {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.push_bounded(value);
        Ok(())
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    let mut keep = value.len().min(max_bytes);
    while keep > 0 && !value.is_char_boundary(keep) {
        keep -= 1;
    }
    value.truncate(keep);
}

/// Keep presentation diffs within the serialized metadata budget, including
/// JSON escaping. Raw UTF-8 size alone does not bound control-character diffs.
pub(crate) fn bounded_diff_metadata(mut diff: String) -> serde_json::Value {
    let escaped_len = |ch: char| match ch {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
        ch if ch <= '\u{001f}' => 6,
        ch => ch.len_utf8(),
    };
    let envelope = r#"{"diff":""}"#.len();
    let budget = crate::tool::MAX_TOOL_METADATA_BYTES - envelope;
    if diff.chars().map(escaped_len).sum::<usize>() > budget {
        let content_budget = budget
            - UNIFIED_DIFF_TRUNCATION_MARKER
                .chars()
                .map(escaped_len)
                .sum::<usize>();
        let mut used = 0;
        let keep = diff
            .char_indices()
            .find_map(|(offset, ch)| {
                used += escaped_len(ch);
                (used > content_budget).then_some(offset)
            })
            .expect("oversized diff exceeds the smaller content budget");
        diff.truncate(keep);
        diff.push_str(UNIFIED_DIFF_TRUNCATION_MARKER);
    }
    serde_json::json!({ "diff": diff })
}

/// Build a minimal, bounded unified diff showing the replacement with
/// surrounding context lines so the rendered output is scannable at a glance.
/// Hunk counts always describe the complete replacement, even when the body is
/// truncated to the normal tool-output budget.
pub(crate) fn format_unified_diff(path: &str, old: &str, new: &str, full_text: &str) -> String {
    use std::fmt::Write as _;

    let match_offset = full_text.find(old).unwrap_or(0);
    let change_line = full_text[..match_offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    // Do not collect every line here. A valid 32 MiB file made entirely of
    // newlines would otherwise allocate hundreds of MiB just to retain
    // borrowed line slices while constructing a three-line context window.
    let full_line_count = full_text.lines().count();
    let old_count = old.lines().count();
    let new_count = new.lines().count();
    let context_start = change_line.saturating_sub(3);
    let context_before_count = change_line
        .min(full_line_count)
        .saturating_sub(context_start);
    let after_start = change_line.saturating_add(old_count).min(full_line_count);
    let after_end = after_start.saturating_add(3).min(full_line_count);
    let context_after_count = after_end.saturating_sub(after_start);
    let hunk_start = context_start + 1;
    let old_hunk_count = context_before_count + old_count + context_after_count;
    let new_hunk_count = context_before_count + new_count + context_after_count;

    let mut diff = UnifiedDiffWriter::new();
    write!(
        diff,
        "--- a/{path}\n+++ b/{path}\n@@ -{hunk_start},{old_hunk_count} +{hunk_start},{new_hunk_count} @@\n"
    )
    .expect("bounded unified-diff formatting cannot fail");
    if diff.truncated {
        return diff.finish();
    }

    for line in full_text
        .lines()
        .skip(context_start)
        .take(context_before_count)
    {
        if !diff.push_line(" ", line) {
            return diff.finish();
        }
    }
    for line in old.lines() {
        if !diff.push_line("-", line) {
            return diff.finish();
        }
    }
    for line in new.lines() {
        if !diff.push_line("+", line) {
            return diff.finish();
        }
    }
    for line in full_text
        .lines()
        .skip(after_start)
        .take(context_after_count)
    {
        if !diff.push_line(" ", line) {
            return diff.finish();
        }
    }
    diff.finish()
}

/// Build the bounded creation form of a unified diff. At most ten content
/// lines are previewed, matching the historical output without ever copying a
/// payload-sized line into an intermediate or final string.
pub(crate) fn format_unified_creation_diff(path: &str, content: &str) -> String {
    use std::fmt::Write as _;

    let total = content.lines().count();
    let mut diff = UnifiedDiffWriter::new();
    write!(diff, "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{total} @@\n")
        .expect("bounded unified-diff formatting cannot fail");
    if diff.truncated {
        return diff.finish();
    }

    let mut shown = 0usize;
    for line in content.lines().take(10) {
        shown += 1;
        if !diff.push_line("+", line) {
            return diff.finish();
        }
    }
    if total > shown {
        writeln!(
            diff,
            "… {} more line{}",
            total - shown,
            if total - shown == 1 { "" } else { "s" }
        )
        .expect("bounded unified-diff formatting cannot fail");
    }
    diff.finish()
}

/// Truncates a display line to `max` characters, appending an ellipsis when cut.
pub(crate) fn clip_line(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let clipped: String = line.chars().take(max).collect();
        format!("{clipped}…")
    }
}

#[cfg(test)]
mod unified_diff_tests;
