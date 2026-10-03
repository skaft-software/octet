//! Public tool activity: safe targets, semantic summaries and test results.

use super::*;

pub(super) fn projection_actor_generation(run_id: &RunId) -> u64 {
    run_id
        .as_str()
        .split('-')
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}

pub(super) fn normalized_tool_name(name: &str) -> String {
    let normalized = name
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.trim_matches('_').is_empty() {
        "tool".into()
    } else {
        normalized
    }
}

pub(super) fn safe_relative_path(value: &str) -> Option<String> {
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    let mut components = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if components.is_empty() {
        Some(".".into())
    } else {
        Some(components.join("/"))
    }
}

pub(super) fn safe_public_target(workspace: &Path, value: &str) -> Option<String> {
    if value.contains("://") {
        let url = url::Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        let host = url.host_str()?;
        let port = url
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        return Some(bounded_text(
            &format!("{}://{host}{port}{path}", url.scheme()),
            1024,
        ));
    }
    let source = Path::new(value);
    if !source.is_absolute() {
        return safe_relative_path(value);
    }
    let workspace = workspace.canonicalize().ok()?;
    let candidate = source.canonicalize().ok()?;
    let relative = candidate.strip_prefix(workspace).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(".".into());
    }
    safe_relative_path(&relative.to_string_lossy())
}

pub(super) fn safe_workspace_path(workspace: &Path, value: &str) -> Option<String> {
    if value.contains("://") {
        return None;
    }
    safe_public_target(workspace, value)
}

pub(super) fn safe_public_query(workspace: &Path, value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("://") && url::Url::parse(&normalized).is_ok() {
        return safe_public_target(workspace, &normalized);
    }

    let lower = normalized.to_ascii_lowercase();
    let sensitive_assignments = [
        "api_key=",
        "api_key:",
        "api-key=",
        "api-key:",
        "apikey=",
        "apikey:",
        "access_token=",
        "access_token:",
        "access-token=",
        "access-token:",
        "auth_token=",
        "auth_token:",
        "authorization=",
        "authorization:",
        "bearer ",
        "basic ",
        "client_secret=",
        "client_secret:",
        "cookie=",
        "cookie:",
        "credential=",
        "credential:",
        "password=",
        "password:",
        "password ",
        "secret=",
        "secret:",
        "secret ",
        "session_token=",
        "session_token:",
        "token=",
        "token:",
    ];
    let known_token_prefixes = [
        "akia",
        "asia",
        "aiza",
        "dop_v1_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "hf_",
        "npm_",
        "pypi-",
        "sk-",
        "sk_live_",
        "rk_live_",
        "xoxb-",
        "xoxa-",
        "xoxp-",
        "xoxr-",
        "xoxs-",
        "xoxapp-",
        "ya29.",
    ];
    let contains_known_token = normalized.split_ascii_whitespace().any(|word| {
        let word = word.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.')
        });
        let word = word.to_ascii_lowercase();
        word.len() >= 12
            && known_token_prefixes
                .iter()
                .any(|prefix| word.starts_with(prefix))
    });
    if sensitive_assignments
        .iter()
        .any(|needle| lower.contains(needle))
        || contains_known_token
    {
        return Some("[redacted query]".into());
    }

    Some(bounded_text(&normalized, 512))
}

pub(super) fn search_target(query: Option<String>, path: Option<String>) -> Option<String> {
    match (query, path) {
        (Some(query), Some(path)) => Some(bounded_text(&format!("{query} in {path}"), 1024)),
        (Some(query), None) => Some(query),
        (None, Some(path)) => Some(path),
        (None, None) => None,
    }
}

pub(super) fn command_activity_details(
    name: &str,
    arguments: &serde_json::Value,
    workspace: &Path,
) -> (Option<String>, bool) {
    if !matches!(name, "bash" | "exec") {
        return (None, false);
    }
    let raw_command = arguments
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if raw_command.is_empty() {
        return (None, false);
    }

    // Keep the complete command visible. The command is already bounded and
    // control-safe at the public boundary; only credential-like values are
    // collapsed so observability does not become an accidental secret leak.
    let command_preview =
        if safe_public_query(workspace, raw_command).as_deref() == Some("[redacted query]") {
            let context = raw_command
                .split_ascii_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            if context.is_empty() {
                "[redacted command]".into()
            } else {
                format!("{context} [redacted arguments]")
            }
        } else {
            raw_command.to_owned()
        };

    // Verification classification remains intentionally conservative and is
    // independent from command visibility. A compound or quoted shell command
    // is still shown in full, but is not promoted to a verified-test phase
    // unless its shape can be classified deterministically.
    let normalized =
        crate::presentation::summarize_tool_with_workspace(name, arguments, Some(workspace))
            .shell_command
            .unwrap_or_default();
    let simple_command = !normalized.chars().any(|character| {
        matches!(
            character,
            '\n' | '\r' | ';' | '|' | '&' | '>' | '<' | '`' | '$' | '\'' | '"'
        )
    });
    let words = normalized.split_ascii_whitespace().collect::<Vec<_>>();
    let program = words
        .first()
        .and_then(|word| Path::new(word).file_name())
        .and_then(|word| word.to_str())
        .unwrap_or_default();
    let subcommand = words.get(1).copied().unwrap_or_default();
    let verification = simple_command
        && matches!(
            (program, subcommand, words.get(2).copied()),
            (
                "cargo",
                "test" | "check" | "clippy" | "fmt" | "build" | "doc",
                _
            ) | (
                "npm" | "pnpm" | "yarn" | "bun",
                "test" | "build" | "lint" | "check",
                _
            ) | (
                "npm" | "pnpm" | "yarn" | "bun",
                "run",
                Some("test" | "build" | "lint" | "check" | "typecheck")
            ) | ("pytest", _, _)
                | ("python" | "python3", "-m", Some("pytest" | "unittest"))
                | ("go", "test" | "vet" | "build", _)
                | ("rustc", _, _)
        );
    (Some(bounded_text(&command_preview, 1024)), verification)
}

pub(super) fn semantic_tool_activity(
    name: &str,
    arguments: &serde_json::Value,
    workspace: &Path,
    started_at_ms: u64,
) -> ToolActivity {
    let path = arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_public_target(workspace, value));
    let resource_path = arguments
        .get("resource_path")
        .and_then(serde_json::Value::as_str)
        .and_then(safe_relative_path);
    let query = arguments
        .get("query")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_public_query(workspace, value));
    let url = arguments
        .get("url")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_public_target(workspace, value));
    let cwd = arguments
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_workspace_path(workspace, value));
    let (command_preview, verification) = command_activity_details(name, arguments, workspace);
    let remote_read = name == "read"
        && arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.starts_with("http://") || value.starts_with("https://"));
    let (kind, phase, title, target) = match name {
        "read" if remote_read => (
            ToolKind::Web,
            ActivityPhase::Investigated,
            path.as_ref()
                .map(|target| format!("Read {target}"))
                .unwrap_or_else(|| "Read remote resource".into()),
            path,
        ),
        "read" => (
            ToolKind::Read,
            ActivityPhase::Investigated,
            path.as_ref()
                .map(|target| format!("Read {target}"))
                .unwrap_or_else(|| "Read file".into()),
            path,
        ),
        "search" => {
            let target = search_target(query, path);
            (
                ToolKind::Search,
                ActivityPhase::Investigated,
                target
                    .as_ref()
                    .map(|target| format!("Search {target}"))
                    .unwrap_or_else(|| "Search workspace".into()),
                target,
            )
        }
        "edit" => (
            ToolKind::Edit,
            ActivityPhase::Changed,
            path.as_ref()
                .map(|target| format!("Update {target}"))
                .unwrap_or_else(|| "Update file".into()),
            path,
        ),
        "write" => (
            ToolKind::Write,
            ActivityPhase::Changed,
            path.as_ref()
                .map(|target| format!("Write {target}"))
                .unwrap_or_else(|| "Write file".into()),
            path,
        ),
        "bash" | "exec" => (
            ToolKind::Command,
            if verification {
                ActivityPhase::Verified
            } else {
                ActivityPhase::Other
            },
            command_preview
                .as_ref()
                .map(|command| {
                    let single_line = command.replace(['\r', '\n', '\t'], " ");
                    format!("Run {single_line}")
                })
                .unwrap_or_else(|| "Run command".into()),
            None,
        ),
        "read_skill_resource" => (
            ToolKind::Skill,
            ActivityPhase::Investigated,
            resource_path
                .as_ref()
                .map(|target| format!("Read skill resource {target}"))
                .unwrap_or_else(|| "Read skill resource".into()),
            resource_path,
        ),
        "web_search" => {
            let target = url.or(query);
            (
                ToolKind::Web,
                ActivityPhase::Investigated,
                target
                    .as_ref()
                    .map(|target| format!("Search the web for {target}"))
                    .unwrap_or_else(|| "Search the web".into()),
                target,
            )
        }
        _ => (
            ToolKind::Other,
            ActivityPhase::Other,
            format!(
                "Run {}",
                normalized_tool_name(name).replace(['_', '-'], " ")
            ),
            None,
        ),
    };
    ToolActivity {
        raw_tool_name: normalized_tool_name(name),
        kind,
        phase,
        status: ToolActivityStatus::Running,
        title: bounded_single_line_text(&title, 512),
        summary: Some("Running".into()),
        target,
        cwd,
        command_preview,
        exit_code: None,
        signal: None,
        started_at_ms: started_at_ms.max(1),
        completed_at_ms: None,
        duration_ms: None,
        output_summary: None,
        output_handle: None,
        observed_output_bytes: 0,
        dropped_output_bytes: 0,
        changed_paths: Vec::new(),
        source_ids: Vec::new(),
        artifact_ids: Vec::new(),
    }
}

pub(super) fn parse_process_metadata(text: &str) -> (Option<i32>, Option<i32>, Option<u64>) {
    let mut exit_code = None;
    let mut signal = None;
    let mut duration_ms = None;
    for token in text.lines().take(4).flat_map(str::split_ascii_whitespace) {
        if let Some(value) = token.strip_prefix("exit=") {
            if let Some(value) = value.strip_prefix("signal:") {
                signal = value.parse::<i32>().ok();
            } else if value != "unknown" {
                exit_code = value.parse::<i32>().ok();
            }
        } else if let Some(value) = token
            .strip_prefix("duration=")
            .and_then(|value| value.strip_suffix('s'))
        {
            duration_ms = value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|seconds| (seconds * 1_000.0).round().min(u64::MAX as f64) as u64);
        }
    }
    (exit_code, signal, duration_ms)
}

pub(super) fn complete_tool_activity(
    mut activity: ToolActivity,
    name: &str,
    result: &Result<ToolOutput, ToolError>,
    completed_at_ms: u64,
    progress: ProjectedToolProgress,
) -> (ToolActivity, ToolResultSummary) {
    let raw_result = match result {
        Ok(output) => output.text.as_str(),
        Err(error) => error.message.as_str(),
    };
    let (exit_code, signal, parsed_duration_ms) = if matches!(name, "bash" | "exec") {
        parse_process_metadata(raw_result)
    } else {
        (None, None, None)
    };
    let failed = crate::presentation::tool_result_is_failure(name, result);
    let status = if failed {
        ToolActivityStatus::Failed
    } else {
        ToolActivityStatus::Succeeded
    };
    let duration_ms = parsed_duration_ms
        .unwrap_or_else(|| completed_at_ms.saturating_sub(activity.started_at_ms));
    let output_summary = match status {
        ToolActivityStatus::Succeeded if activity.phase == ActivityPhase::Verified => {
            Some("Verification completed".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Read => {
            Some("Read completed".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Search => {
            Some("Search completed".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Edit => {
            Some("File updated".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Write => {
            Some("File written".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Web => {
            Some("Remote lookup completed".into())
        }
        ToolActivityStatus::Succeeded => Some("Tool completed".into()),
        ToolActivityStatus::Failed if activity.phase == ActivityPhase::Verified => {
            Some("Verification failed".into())
        }
        ToolActivityStatus::Failed if exit_code.is_some() => {
            Some(format!("Command exited {}", exit_code.unwrap_or_default()))
        }
        ToolActivityStatus::Failed if signal.is_some() => Some(format!(
            "Command stopped by signal {}",
            signal.unwrap_or_default()
        )),
        ToolActivityStatus::Failed => Some("Tool failed".into()),
        ToolActivityStatus::Running | ToolActivityStatus::Stopped => None,
    };
    let final_bytes = raw_result.len().min(u64::MAX as usize) as u64;
    activity.status = status;
    activity.summary = Some(match status {
        ToolActivityStatus::Succeeded => "Completed".into(),
        ToolActivityStatus::Failed => "Failed".into(),
        ToolActivityStatus::Stopped => "Stopped".into(),
        ToolActivityStatus::Running => "Running".into(),
    });
    activity.exit_code = exit_code;
    activity.signal = signal;
    activity.completed_at_ms = Some(completed_at_ms.max(activity.started_at_ms));
    activity.duration_ms = Some(duration_ms);
    activity.output_summary = output_summary.clone();
    activity.observed_output_bytes = progress.observed_output_bytes.max(final_bytes);
    activity.dropped_output_bytes = progress.dropped_output_bytes;
    let summary = activity
        .summary
        .clone()
        .unwrap_or_else(|| "Completed".into());
    let result = ToolResultSummary {
        tool_call_item_id: ItemId::new("placeholder").expect("static item ID is valid"),
        status,
        summary,
        output_summary,
        output_handle: activity.output_handle.clone(),
        exit_code,
        signal,
        completed_at_ms: activity.completed_at_ms.unwrap_or(completed_at_ms),
        duration_ms,
        observed_output_bytes: activity.observed_output_bytes,
        dropped_output_bytes: activity.dropped_output_bytes,
    };
    (activity, result)
}

pub(super) fn test_framework_hint(activity: &ToolActivity) -> Option<TestFramework> {
    let command = activity.command_preview.as_deref()?;
    let words = command.split_ascii_whitespace().collect::<Vec<_>>();
    let program = words
        .first()
        .and_then(|word| Path::new(word).file_name())
        .and_then(|word| word.to_str())
        .unwrap_or_default();
    match (program, words.get(1).copied(), words.get(2).copied()) {
        ("cargo", Some("test"), _) => Some(TestFramework::CargoLibtest),
        ("pytest", _, _) | ("python" | "python3", Some("-m"), Some("pytest" | "unittest")) => {
            Some(TestFramework::Pytest)
        }
        ("go", Some("test"), _) => Some(TestFramework::GoTest),
        // Package runners may dispatch either Vitest or Jest, so their
        // deterministic reporter markers select the parser.
        _ => None,
    }
}

pub(super) fn project_test_results(
    item_id: &ItemId,
    activity: &ToolActivity,
    output: &ToolOutput,
) -> Option<StructuredTestResults> {
    if activity.kind != ToolKind::Command || activity.phase != ActivityPhase::Verified {
        return None;
    }
    let status = match activity.status {
        ToolActivityStatus::Succeeded => TestCommandStatus::Succeeded,
        ToolActivityStatus::Failed => TestCommandStatus::Failed,
        ToolActivityStatus::Stopped => TestCommandStatus::Stopped,
        ToolActivityStatus::Running => return None,
    };
    let bytes = output.text.as_bytes();
    let retained_len = bytes.len().min(MAX_TEST_OUTPUT_BYTES);
    parse_test_output(TestOutputInput {
        origin_item_id: item_id.clone(),
        output: &bytes[..retained_len],
        input_truncated: bytes.len() > retained_len || activity.dropped_output_bytes > 0,
        command: TestCommandOutcome {
            status,
            exit_code: activity.exit_code,
            signal: activity.signal,
        },
        framework_hint: test_framework_hint(activity),
    })
    .ok()
}
