#![allow(missing_docs)]

//! Deterministic human-facing model/tool activity summaries.
//!
//! Raw tool arguments remain outside this module's display contract: helpers
//! only derive labels and values for transcript/status surfaces.

use std::path::Path;

use octet_agent::{ToolError, ToolOutput, ToolOutputMediaKind};

/// First-party orchestration calls are projected through the worker roster,
/// never through ordinary transcript tool cards.
pub(crate) const SUBAGENT_TOOL_NAMES: [&str; 6] = [
    "subagent_spawn",
    "subagent_status",
    "subagent_wait",
    "subagent_stop",
    "subagent_continue",
    "subagent_models",
];

pub(crate) fn is_subagent_tool(name: &str) -> bool {
    SUBAGENT_TOOL_NAMES.contains(&name)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDisplay {
    pub active: String,
    pub success: String,
    pub failure: String,
    pub compact_active: String,
    pub compact_success: String,
    pub compact_failure: String,
    pub plain_tag: &'static str,
    /// Stable user-facing label, independent of the protocol tool identifier.
    pub label: String,
    /// Presentation-only command string for the `bash` tool.
    pub shell_command: Option<String>,
    pub changed_path: Option<String>,
    /// Salient argument shown after the tool's label in the transcript, the
    /// way `Read` shows a path and `Bash` shows a command. `None` keeps the
    /// legacy sentence summaries with their redundant lead stripped.
    pub value: Option<String>,
}

impl ToolDisplay {
    /// Marks a completed read with payload-free vision/audio metadata.
    ///
    /// Only successful summaries receive the symbols; active and failed reads
    /// must never imply that the model ingested media.
    pub fn mark_media_read(&mut self, kinds: &[ToolOutputMediaKind]) {
        if self.plain_tag != "read" {
            return;
        }
        let mut suffix = String::new();
        if kinds.contains(&ToolOutputMediaKind::Image) {
            suffix.push_str("  ◉");
        }
        if kinds.contains(&ToolOutputMediaKind::Audio) {
            suffix.push_str("  ♪");
        }
        self.success.push_str(&suffix);
        self.compact_success.push_str(&suffix);
    }

    /// Restores media-read metadata from the durable, payload-free result
    /// summary. Omission markers win, so a locally loaded but unsupported
    /// media item never gains a successful ingestion symbol after resume.
    pub fn mark_media_read_from_result(&mut self, result: &str) {
        let lines = result.lines().collect::<Vec<_>>();
        let image_accepted = lines.contains(&"read=vision")
            && !lines.iter().any(|line| line.contains("image omitted:"));
        let audio_accepted = lines.contains(&"read=audio")
            && !lines.iter().any(|line| line.contains("audio omitted:"));
        let mut kinds = Vec::with_capacity(2);
        if image_accepted {
            kinds.push(ToolOutputMediaKind::Image);
        }
        if audio_accepted {
            kinds.push(ToolOutputMediaKind::Audio);
        }
        self.mark_media_read(&kinds);
    }
}

pub fn summarize_tool(name: &str, args: &serde_json::Value) -> ToolDisplay {
    summarize_tool_with_workspace(name, args, None)
}

/// Build a tool summary while rendering paths inside `workspace` relatively.
/// The raw arguments remain authoritative for execution and audit history.
pub fn summarize_tool_with_workspace(
    name: &str,
    args: &serde_json::Value,
    workspace: Option<&Path>,
) -> ToolDisplay {
    match name {
        "read" => summarize_read(args, workspace),
        "search" => {
            let path = display_path(string_arg(args, "path").unwrap_or("workspace"), workspace);
            let query = string_arg(args, "query").unwrap_or("pattern");
            let full = format!("searching {path} for {query}");
            let compact_path = compact_path(&path);
            let compact = format!("searching {compact_path}");
            ToolDisplay {
                active: full.clone(),
                success: format!("searched {path} for {query}"),
                failure: full,
                compact_active: compact.clone(),
                compact_success: format!("searched {compact_path}"),
                compact_failure: compact,
                plain_tag: "search",
                label: "search".to_owned(),
                shell_command: None,
                changed_path: None,
                value: None,
            }
        }
        "edit" => path_tool(args, "updating", "updated", "updating", "edit", workspace),
        "write" => path_tool(args, "writing", "wrote", "writing", "write", workspace),
        // `exec` is retained only as a renderer for pre-rename sessions.
        "bash" | "exec" => summarize_bash(args, workspace),
        name if is_subagent_tool(name) => ToolDisplay {
            active: "delegating to workers".to_owned(),
            success: "delegation updated".to_owned(),
            failure: "delegation failed".to_owned(),
            compact_active: "delegating".to_owned(),
            compact_success: "delegated".to_owned(),
            compact_failure: "delegation failed".to_owned(),
            plain_tag: "delegation",
            label: "delegation".to_owned(),
            shell_command: None,
            changed_path: None,
            value: None,
        },
        other => {
            let readable = other.replace(['_', '-'], " ");
            let detail = primary_arg_detail(args);
            let (active, success, failure) = match detail.as_deref() {
                Some(detail) => (
                    format!("running {readable}: {detail}"),
                    format!("finished {readable}: {detail}"),
                    format!("{readable} failed: {detail}"),
                ),
                None => (
                    format!("running {readable}"),
                    format!("finished {readable}"),
                    format!("{readable} failed"),
                ),
            };
            ToolDisplay {
                active: active.clone(),
                success: success.clone(),
                failure: failure.clone(),
                compact_active: active,
                compact_success: success,
                compact_failure: failure,
                plain_tag: "tool",
                label: readable,
                shell_command: None,
                changed_path: None,
                value: detail,
            }
        }
    }
}

fn path_tool(
    args: &serde_json::Value,
    active_verb: &str,
    success_verb: &str,
    failure_verb: &str,
    tag: &'static str,
    workspace: Option<&Path>,
) -> ToolDisplay {
    let path = display_path(string_arg(args, "path").unwrap_or("file"), workspace);
    let compact = compact_path(&path);
    ToolDisplay {
        active: format!("{active_verb} {path}"),
        success: format!("{success_verb} {path}"),
        failure: format!("{failure_verb} {path}"),
        compact_active: format!("{active_verb} {compact}"),
        compact_success: format!("{success_verb} {compact}"),
        compact_failure: format!("{failure_verb} {compact}"),
        plain_tag: tag,
        label: tag.to_owned(),
        shell_command: None,
        // Both mutation tools carry a changed-file candidate. The candidate
        // becomes evidence only after changed-file validation in `changed_files`.
        changed_path: matches!(tag, "edit" | "write").then_some(path),
        value: None,
    }
}

fn summarize_read(args: &serde_json::Value, workspace: Option<&Path>) -> ToolDisplay {
    let path = display_path(string_arg(args, "path").unwrap_or("file"), workspace);
    let offset = args.get("offset").and_then(|v| v.as_u64());
    let limit = args.get("limit").and_then(|v| v.as_u64());
    let range = match (offset, limit) {
        (Some(start), Some(count)) => {
            let end = start.saturating_add(count.saturating_sub(1));
            format!("{path}:{start}-{end}")
        }
        (Some(start), None) => format!("{path}:{start}+"),
        _ => path.clone(),
    };
    let compact = compact_path(&path);
    ToolDisplay {
        active: format!("reading {range}"),
        success: format!("read {range}"),
        failure: format!("reading {range}"),
        compact_active: format!("reading {compact}"),
        compact_success: format!("read {compact}"),
        compact_failure: format!("reading {compact}"),
        plain_tag: "read",
        label: "read".to_owned(),
        shell_command: None,
        changed_path: None,
        value: None,
    }
}

fn summarize_bash(args: &serde_json::Value, workspace: Option<&Path>) -> ToolDisplay {
    let command =
        normalize_shell_command(string_arg(args, "command").unwrap_or("command"), workspace);
    let program = command
        .split_whitespace()
        .next()
        .and_then(|value| std::path::Path::new(value).file_name())
        .and_then(|value| value.to_str())
        .unwrap_or("command");

    ToolDisplay {
        active: format!("running {command}"),
        success: format!("ran {command}"),
        failure: format!("failed: {command}"),
        compact_active: format!("running {program}"),
        compact_success: format!("ran {program}"),
        compact_failure: format!("{program} failed"),
        plain_tag: "bash",
        label: "bash".to_owned(),
        shell_command: Some(command),
        changed_path: None,
        value: None,
    }
}

/// Pick the most informative top-level argument of an extension tool call so
/// its transcript header can read `• Web search  <query>` the same way core
/// tools read `• Read  <file>`. Well-known argument names win; otherwise the
/// first string value is used. Long values collapse to one line and truncate.
fn primary_arg_detail(args: &serde_json::Value) -> Option<String> {
    const PREFERRED_KEYS: [&str; 12] = [
        "command", "url", "path", "file", "query", "pattern", "target", "name", "task", "title",
        "key", "message",
    ];
    const MAX_DETAIL_CHARS: usize = 72;

    let object = args.as_object()?;
    let mut candidate = PREFERRED_KEYS.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    });
    if candidate.is_none() {
        candidate = object
            .values()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .find(|value| !value.is_empty());
    }
    let candidate = candidate?;
    let detail: String = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    if detail.chars().count() <= MAX_DETAIL_CHARS {
        return Some(detail);
    }
    let truncated: String = detail.chars().take(MAX_DETAIL_CHARS).collect();
    Some(format!("{truncated}…"))
}

/// Normalize a path only for presentation. Execution retains the raw model
/// argument, while paths inside the configured workspace lose the host-specific
/// absolute prefix.
pub fn display_path(path: &str, workspace: Option<&Path>) -> String {
    let Some(workspace) = workspace else {
        return path.to_owned();
    };
    if path == "~" || path.starts_with("~/") {
        return path.to_owned();
    }
    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let source = Path::new(path);
    let candidate = if source.is_absolute() {
        source.to_path_buf()
    } else {
        workspace_root.join(source)
    };
    let candidate = candidate.canonicalize().unwrap_or(candidate);
    match candidate.strip_prefix(&workspace_root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.to_owned(),
    }
}

/// Remove a redundant shell `cd` to the workspace from the human-facing
/// summary. The command sent to the child process is never rewritten.
pub fn normalize_shell_command(command: &str, workspace: Option<&Path>) -> String {
    let Some(workspace) = workspace else {
        return command.to_owned();
    };
    let root = workspace.to_string_lossy();
    for prefix in [
        format!("cd {root} &&"),
        format!("cd '{root}' &&"),
        format!("cd \"{root}\" &&"),
    ] {
        if let Some(rest) = command.strip_prefix(&prefix) {
            return rest.trim_start().to_owned();
        }
    }
    command.to_owned()
}

fn string_arg<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|value| value.as_str())
}

pub fn compact_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
        .to_owned()
}

pub fn tool_result_is_failure(name: &str, result: &Result<ToolOutput, ToolError>) -> bool {
    match result {
        Err(_) => true,
        Ok(output) if output.is_error() => true,
        Ok(output) if matches!(name, "bash" | "exec") => bash_exit_reason(&output.text).is_some(),
        Ok(_) => false,
    }
}

pub fn tool_failure_reason(name: &str, result: &Result<ToolOutput, ToolError>) -> Option<String> {
    match result {
        Err(error) => Some(error_reason(&error.message)),
        Ok(output) if output.is_error() => Some(error_reason(&output.text)),
        Ok(output) if matches!(name, "bash" | "exec") => bash_exit_reason(&output.text),
        Ok(_) => None,
    }
}

fn bash_exit_reason(output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    let exit = first
        .split_whitespace()
        .find_map(|part| part.strip_prefix("exit="))?;
    match exit {
        "0" => None,
        value if value.starts_with("signal:") => {
            Some(format!("command stopped by {}", value.replace(':', " ")))
        }
        "unknown" => Some("command exit status unknown".into()),
        code => Some(format!("command exited {code}")),
    }
}

fn error_reason(message: &str) -> String {
    let mut lines = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("tool failed");
    if let Some(code) = first.strip_prefix("error ") {
        lines
            .find(|line| !is_hidden_tool_detail(line))
            .map(concise_line)
            .unwrap_or_else(|| code.replace('_', " "))
    } else if is_hidden_tool_detail(first) {
        "tool failed".into()
    } else {
        concise_line(first)
    }
}

/// Protocol/concurrency details remain available in verbose mode but are not
/// part of the default intent-level transcript.
pub fn is_hidden_tool_detail(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("hash=")
        || line.contains("expected_hash")
        || line.contains("actual_hash")
        || line.contains("tool_call_id")
}

pub fn concise_line(value: &str) -> String {
    const LIMIT: usize = 160;
    let line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("unknown error");
    let line = line.trim();
    if line.chars().count() <= LIMIT {
        return line.to_owned();
    }
    let mut result = line
        .chars()
        .take(LIMIT.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}
