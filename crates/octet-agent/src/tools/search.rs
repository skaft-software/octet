//! Bounded repository content search backed by `rg --json`.

use std::process::Stdio;
use std::time::Duration;

use octet_ai::ToolDef;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::effect::{ToolEffect, ToolPolicyDenialCode};
use crate::tool::{ReplaySafety, Tool, ToolConcurrency, ToolContext, ToolError, ToolOutput};
use crate::tools::{clip_line, parse_args, validate_effect_path};

/// Display cap for a single match line.
const MAX_LINE_CHARS: usize = 300;
/// Default result cap when `max_results` is omitted.
const DEFAULT_MAX_RESULTS: usize = 50;
/// Hard cap for retained fields of one structured `rg --json` record.
/// Unused per-occurrence submatch arrays are discarded while framing.
const MAX_RG_EVENT_BYTES: usize = 256 * 1024;
const RG_MAX_COLUMNS: &str = "1024";
const RG_MAX_FILESIZE: &str = "32M";
const MAX_SEARCH_PATTERN_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    mode: SearchMode,
    #[serde(alias = "limit")]
    max_results: Option<usize>,
    #[serde(default, rename = "ignoreCase")]
    ignore_case: bool,
    #[serde(default)]
    context: usize,
    #[serde(default = "default_hidden")]
    hidden: bool,
}

fn default_hidden() -> bool {
    true
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SearchMode {
    #[default]
    Literal,
    Regex,
}

/// The built-in `search` tool.
///
/// Read-only. Shells out to ripgrep with `--json` (structured output, no
/// shell interpolation of the query — every value is passed as its own
/// argument after `--`) and reformats matches into compact
/// `path:line  text` lines. Results are sorted by path for deterministic
/// ordering, capped by `max_results` and by the sandbox output-byte limit,
/// with explicit truncation metadata. "No matches" is a successful output,
/// not an error.
///
/// Cleanup note: unlike `bash`, ripgrep is cancelled via `kill_on_drop` on
/// the direct child only — `rg` spawns no subprocess tree, so no process-group
/// handling is needed.
pub struct SearchTool;

#[async_trait::async_trait]
impl Tool for SearchTool {
    fn composition_is_unmetered(&self) -> bool {
        true
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Search file contents with ripgrep (rg)")
    }

    fn definition(&self) -> ToolDef {
        ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "search".to_string(),
            description: "Search local file contents. Prefer paths relative to the workspace; \
                          trusted-local hosts also accept absolute and `~/` paths for intentional \
                          external searches. Matches are literal by default; set mode=regex for \
                          regular expressions. Returns `path:line  text` lines with a match count and \
                          truncation flag. For independent inspections, request all read/search calls \
                          together in one turn."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text (or regex when mode=regex) to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search; relative to workspace, or absolute/~/ when enabled (default: workspace root)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "File pattern filter, e.g. \"*.rs\"."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["literal", "regex"],
                        "description": "Matching mode (default literal)."
                    },
                    "ignoreCase": {"type":"boolean", "description":"Case-insensitive matching (default false)."},
                    "context": {"type":"integer", "minimum":0, "maximum":1000, "description":"Surrounding lines per match; not counted against limit."},
                    "hidden": {"type":"boolean", "description":"Include hidden files (default true); ignore rules still apply."},
                    "limit": {"type":"integer", "minimum":1, "description":"Alias for max_results; do not combine."},
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum matches to return (default 50)."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        }
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "matches": {
                    "type": "array",
                    "description": "Returned match and context lines, in ripgrep order, with the same line clipping as a direct search.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "line": {"type": "integer", "minimum": 1},
                            "text": {"type": "string"},
                            "is_context": {"type": "boolean"},
                            "text_truncated": {"type": "boolean"}
                        },
                        "required": ["path", "line", "text", "is_context", "text_truncated"],
                        "additionalProperties": false
                    }
                },
                "total": {"type": "integer", "minimum": 0, "description": "Returned match count, excluding context; a lower bound when truncated."},
                "truncated": {"type": "boolean"}
            },
            "required": ["matches", "total", "truncated"],
            "additionalProperties": false
        }))
    }

    fn effect(
        &self,
        arguments: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        if !ctx.sandbox.allow_process {
            return Err(ToolError::policy_denied(
                ToolPolicyDenialCode::ProcessDisabled,
                "error not_permitted\nsearch requires command execution \
                 (allow_process=true and allow_shell=true)",
            ));
        }
        if !ctx.sandbox.allow_shell {
            return Err(ToolError::policy_denied(
                ToolPolicyDenialCode::ShellDisabled,
                "error not_permitted\nsearch requires command execution \
                 (allow_process=true and allow_shell=true)",
            ));
        }
        let arguments = arguments
            .as_object()
            .ok_or_else(|| ToolError::new("invalid arguments: expected an object"))?;
        if arguments.len() > 9
            || arguments.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "query"
                        | "path"
                        | "glob"
                        | "mode"
                        | "max_results"
                        | "limit"
                        | "ignoreCase"
                        | "context"
                        | "hidden"
                )
            })
        {
            return Err(ToolError::new("invalid arguments: unknown property"));
        }
        let query = arguments
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("invalid arguments: `query` must be a string"))?;
        if query.is_empty() {
            return Err(ToolError::new("invalid arguments: query must be non-empty"));
        }
        if query.len() > MAX_SEARCH_PATTERN_BYTES {
            return Err(ToolError::new(format!(
                "invalid arguments: query is {} bytes (limit {MAX_SEARCH_PATTERN_BYTES})",
                query.len()
            )));
        }
        for name in ["path", "glob"] {
            if arguments.get(name).is_some_and(|value| !value.is_string()) {
                return Err(ToolError::new(format!(
                    "invalid arguments: `{name}` must be a string"
                )));
            }
        }
        if let Some(path) = arguments.get("path").and_then(serde_json::Value::as_str) {
            validate_effect_path(path, ctx.sandbox.allow_external_paths)?;
        }
        if arguments
            .get("glob")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|glob| glob.len() > MAX_SEARCH_PATTERN_BYTES)
        {
            return Err(ToolError::new(format!(
                "invalid arguments: `glob` exceeds {MAX_SEARCH_PATTERN_BYTES} bytes"
            )));
        }
        if arguments
            .get("mode")
            .is_some_and(|value| !matches!(value.as_str(), Some("literal" | "regex")))
        {
            return Err(ToolError::new(
                "invalid arguments: `mode` must be `literal` or `regex`",
            ));
        }
        if arguments.get("max_results").is_some_and(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .is_none_or(|value| value == 0)
        }) {
            return Err(ToolError::new(
                "invalid arguments: `max_results` must be a positive integer",
            ));
        }
        for name in ["ignoreCase", "hidden"] {
            if arguments.get(name).is_some_and(|v| !v.is_boolean()) {
                return Err(ToolError::new(format!(
                    "invalid arguments: {name} must be boolean"
                )));
            }
        }
        if arguments
            .get("context")
            .is_some_and(|v| v.as_u64().is_none_or(|n| n > 1000))
        {
            return Err(ToolError::new(
                "invalid arguments: context must be an integer from 0 to 1000",
            ));
        }
        if arguments.contains_key("limit") && arguments.contains_key("max_results") {
            return Err(ToolError::new(
                "invalid arguments: use limit or max_results, not both",
            ));
        }
        if arguments.get("limit").is_some_and(|v| {
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .is_none_or(|n| n == 0)
        }) {
            return Err(ToolError::new(
                "invalid arguments: limit must be a positive integer",
            ));
        }
        // Search currently executes `rg` from PATH as a native child. Treat it
        // as process authority even though its argument construction is fixed.
        Ok(ToolEffect::HostProcess)
    }

    fn replay_safety(&self) -> ReplaySafety {
        // `rg` is resolved afresh from ambient PATH. Recovery must not assume
        // the resulting native executable is idempotent.
        ReplaySafety::Unsafe
    }

    fn concurrency(&self) -> ToolConcurrency {
        // Native process effects remain ordered unless an isolated backend can
        // prove independence and enforce aggregate resource bounds.
        ToolConcurrency::Sequential
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.execute_with_program(args, ctx, std::path::Path::new("rg"))
            .await
    }
}

impl SearchTool {
    async fn execute_with_program(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
        program: &std::path::Path,
    ) -> Result<ToolOutput, ToolError> {
        self.effect(&args, ctx)?;
        let args: SearchArgs = parse_args(args)?;
        execute_search(args, ctx, program).await
    }
}

async fn execute_search(
    args: SearchArgs,
    ctx: &ToolContext<'_>,
    program: &std::path::Path,
) -> Result<ToolOutput, ToolError> {
    let max_results = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);

    // Resolve explicit paths through the host policy. `rg` keeps relative
    // display paths for workspace targets and receives an absolute target
    // for trusted-local paths outside the workspace.
    let search_path = args
        .path
        .as_deref()
        .map(|path| ctx.resolve_existing(path))
        .transpose()?;

    let mut command = tokio::process::Command::new(program);
    command.args([
        "--json",
        "--sort",
        "path",
        "--no-config",
        "--max-columns",
        RG_MAX_COLUMNS,
        "--max-columns-preview",
        "--max-filesize",
        RG_MAX_FILESIZE,
    ]);
    if args.mode == SearchMode::Literal {
        command.arg("--fixed-strings");
    }
    if args.ignore_case {
        command.arg("--ignore-case");
    }
    if args.hidden {
        command.arg("--hidden");
    }
    if args.context > 0 {
        command.arg("--context").arg(args.context.to_string());
    }
    if let Some(glob) = &args.glob {
        command.args(["--glob", glob]);
    }
    // `--` terminates flags: the model's query and path are data, never
    // options, and no shell is involved at any point.
    command.arg("--").arg(&args.query);
    if let Some(path) = &search_path {
        if let Ok(relative) = path.strip_prefix(ctx.workspace) {
            command.arg(if relative.as_os_str().is_empty() {
                std::path::Path::new(".")
            } else {
                relative
            });
        } else {
            command.arg(path);
        }
    }
    command
        .env_clear()
        .envs(crate::extension_process::sanitized_subprocess_environment())
        .current_dir(ctx.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Error text is not surfaced (it can contain host paths), so never
        // create an unread pipe that a child can fill and deadlock on.
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::new("search is unavailable: ripgrep (rg) was not found on PATH")
        } else {
            ToolError::new(format!("failed to start ripgrep: {e}"))
        }
    })?;

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            return Err(ToolError::new("failed to capture ripgrep output"));
        }
    };

    let byte_budget = ctx.sandbox.max_output_bytes.saturating_sub(128).max(1024);
    let deadline = tokio::time::Instant::now() + ctx.sandbox.bash_timeout;
    let collect = async {
        let (results, truncated, match_count) =
            collect_rg_stdout(stdout, max_results, byte_budget).await?;

        let status =
            if truncated {
                // Enough results — stop ripgrep instead of draining it.
                let _ = child.start_kill();
                child
                    .wait()
                    .await
                    .map_err(|error| ToolError::new(format!("failed to reap ripgrep: {error}")))?;
                None
            } else {
                Some(child.wait().await.map_err(|error| {
                    ToolError::new(format!("failed to wait for ripgrep: {error}"))
                })?)
            };
        Ok::<_, ToolError>((results, truncated, match_count, status))
    };
    let collected = tokio::select! {
        biased;
        _ = ctx.cancellation.cancelled() => Ok(Err(ToolError::new("search cancelled"))),
        result = tokio::time::timeout_at(deadline, collect) => result,
    };
    let (results, truncated, match_count, status) = match collected {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            return Err(error);
        }
        Err(_) => {
            let _ = child.start_kill();
            // Bound cleanup separately; `kill_on_drop` remains the final
            // backstop if an unusual platform does not reap promptly.
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            return Err(ToolError::new(format!(
                "search exceeded the {:.0}s execution limit",
                ctx.sandbox.bash_timeout.as_secs_f64()
            )));
        }
    };

    // rg exits 0 on matches, 1 on no matches, 2 on real errors.
    if status.is_some_and(|status| status.code() == Some(2)) && results.is_empty() {
        return Err(ToolError::new(
            "search failed: ripgrep reported an error (check the query/glob syntax)",
        ));
    }

    let count_line = if truncated {
        format!("{match_count}+ matches")
    } else if match_count == 1 {
        "1 match".to_string()
    } else {
        format!("{match_count} matches")
    };
    let output = if results.is_empty() {
        ToolOutput::new("no matches")
    } else {
        ToolOutput::new(format!(
            "{count_line}\n{}\ntruncated={truncated}",
            results
                .iter()
                .map(SearchLine::render)
                .collect::<Vec<_>>()
                .join("\n")
        ))
    };
    if ctx.progress.is_programmatic() {
        output
            .try_with_programmatic_content(serde_json::json!({
                "matches": results, "total": match_count, "truncated": truncated
            }))
            .map_err(|error| ToolError::new(error.to_string()))
    } else {
        Ok(output)
    }
}

async fn collect_rg_stdout<R: tokio::io::AsyncRead + Unpin>(
    mut stdout: R,
    max_results: usize,
    byte_budget: usize,
) -> Result<(Vec<SearchLine>, bool, usize), ToolError> {
    let mut results = Vec::new();
    let mut match_count = 0usize;
    let mut body_bytes = 0usize;
    let mut event = RgEventBuffer::default();
    let mut chunk = [0u8; 8 * 1024];

    loop {
        let read = stdout
            .read(&mut chunk)
            .await
            .map_err(|error| ToolError::new(format!("failed to read ripgrep output: {error}")))?;
        if read == 0 {
            if !event.bytes.is_empty()
                && record_rg_event(
                    &event.bytes,
                    &mut results,
                    &mut body_bytes,
                    &mut match_count,
                    max_results,
                    byte_budget,
                )
            {
                return Ok((results, true, match_count));
            }
            return Ok((results, false, match_count));
        }

        let mut cursor = 0;
        while cursor < read {
            let remainder = &chunk[cursor..read];
            let newline = remainder.iter().position(|byte| *byte == b'\n');
            let end = newline.map_or(read, |offset| cursor + offset);
            let segment = &chunk[cursor..end];
            for &byte in segment {
                event.push(byte)?;
            }
            let Some(_) = newline else {
                break;
            };
            if record_rg_event(
                &event.bytes,
                &mut results,
                &mut body_bytes,
                &mut match_count,
                max_results,
                byte_budget,
            ) {
                return Ok((results, true, match_count));
            }
            event = RgEventBuffer::default();
            cursor = end + 1;
        }
    }
}

/// Ripgrep emits compact JSON, with an array of offsets and matched text
/// for *every occurrence*, even with --max-columns-preview. Our result is
/// line-oriented and never consumes that array. Replace it with [] while
/// draining the record, keeping memory bounded independently of match density.
/// String/escape/depth tracking prevents file contents from impersonating keys.
#[derive(Default)]
struct RgEventBuffer {
    bytes: Vec<u8>,
    depth: usize,
    in_string: bool,
    escaped: bool,
    skip_depth: Option<usize>,
}

impl RgEventBuffer {
    fn push(&mut self, byte: u8) -> Result<(), ToolError> {
        let starts_submatches = !self.in_string
            && self.depth == 2
            && byte == b'['
            && self.bytes.ends_with(b"\"submatches\":");
        if starts_submatches {
            self.bytes.extend_from_slice(b"[]");
            self.skip_depth = Some(self.depth);
        } else if self.skip_depth.is_none() {
            self.bytes.push(byte);
        }
        if self.bytes.len() > MAX_RG_EVENT_BYTES {
            return Err(ToolError::new(format!(
                "search output record exceeded the {MAX_RG_EVENT_BYTES}-byte limit"
            )));
        }
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.in_string = false;
            }
        } else {
            match byte {
                b'"' => self.in_string = true,
                b'{' | b'[' => self.depth = self.depth.saturating_add(1),
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.skip_depth == Some(self.depth) {
                        self.skip_depth = None;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn record_rg_event(
    event: &[u8],
    results: &mut Vec<SearchLine>,
    body_bytes: &mut usize,
    match_count: &mut usize,
    max_results: usize,
    byte_budget: usize,
) -> bool {
    let Ok(event) = std::str::from_utf8(event) else {
        return false;
    };
    let Some((result, is_match)) = render_match(event) else {
        return false;
    };
    let rendered = result.render();
    if (is_match && *match_count == max_results)
        || body_bytes.saturating_add(rendered.len() + usize::from(!results.is_empty()))
            > byte_budget
    {
        return true;
    }
    *body_bytes += rendered.len() + usize::from(!results.is_empty());
    if is_match {
        *match_count += 1;
    }
    results.push(result);
    false
}

#[derive(Debug, Serialize)]
struct SearchLine {
    path: String,
    line: u64,
    text: String,
    is_context: bool,
    text_truncated: bool,
}

impl SearchLine {
    fn render(&self) -> String {
        let separator = if self.is_context { "-" } else { ":" };
        format!("{}{separator}{}  {}", self.path, self.line, self.text)
    }
}

/// Decodes fields before rendering; never parses the human-facing output to
/// recover paths or text (which themselves may contain separators/newlines).
fn render_match(json_line: &str) -> Option<(SearchLine, bool)> {
    let event: serde_json::Value = serde_json::from_str(json_line).ok()?;
    let kind = event.get("type")?.as_str()?;
    if kind != "match" && kind != "context" {
        return None;
    }
    let data = event.get("data")?;
    let path = data.get("path")?.get("text")?.as_str()?;
    let line_number = data.get("line_number")?.as_u64()?;
    let text = data
        .get("lines")
        .and_then(|l| l.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim_end();
    let clipped = clip_line(text, MAX_LINE_CHARS);
    Some((
        SearchLine {
            path: path.to_owned(),
            line: line_number,
            text_truncated: clipped != text,
            text: clipped,
            is_context: kind == "context",
        },
        kind == "match",
    ))
}

#[cfg(test)]
mod tests;
