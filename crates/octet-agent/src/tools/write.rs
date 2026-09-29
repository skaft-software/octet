//! Single-file create or full overwrite.

use octet_ai::ToolDef;
use serde::Deserialize;

use crate::effect::{ToolEffect, ToolPolicyDenialCode};
use crate::secure_fs::{PreparedMutation, SecureFileError};
use crate::tool::{content_hash, Tool, ToolContext, ToolError, ToolOutput};
use crate::tools::{
    format_unified_creation_diff, format_unified_diff, parse_args, validate_effect_path,
    validate_expected_hash, MAX_FILE_BYTES,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    /// Optional hash from a prior `read`; rejects the write if the existing
    /// file content no longer matches.
    expected_hash: Option<String>,
}

/// The built-in `write` tool.
///
/// Creates a new file (and missing parent directories) or completely replaces
/// an existing file.  `expected_hash` from a prior `read` gates the overwrite
/// against that existing content; without it the caller accepts
/// last-write-wins.  Writes are atomic per-file (temp file + rename).
pub struct WriteTool;

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "write".to_string(),
            description: "Create or fully replace one file. Creates missing parent \
                          directories. Pass expected_hash from a prior read to reject \
                          stale writes; omitting it accepts last-write-wins. Prefer \
                          paths relative to the workspace."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path; relative to the workspace, or absolute/~/ when enabled."
                    },
                    "content": {
                        "type": "string",
                        "description": "The full file content to write."
                    },
                    "expected_hash": {
                        "type": "string",
                        "description": "Optional hash from read; rejects the write if the file changed."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Create or overwrite files")
    }

    fn prompt_guidelines(&self) -> &[&str] {
        &["Use write only for new files or complete rewrites."]
    }

    fn effect(
        &self,
        arguments: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        if !ctx.sandbox.allow_write {
            return Err(ToolError::policy_denied(
                ToolPolicyDenialCode::WriteDisabled,
                "error not_permitted\nwrite is disabled by sandbox policy (allow_write=false)",
            ));
        }
        let arguments = arguments
            .as_object()
            .ok_or_else(|| ToolError::new("invalid arguments: expected an object"))?;
        if arguments.len() > 3
            || arguments
                .keys()
                .any(|key| !matches!(key.as_str(), "path" | "content" | "expected_hash"))
        {
            return Err(ToolError::new("invalid arguments: unknown property"));
        }
        let path = arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("invalid arguments: `path` must be a string"))?;
        validate_effect_path(path, ctx.sandbox.allow_external_paths)?;
        let content = arguments
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("invalid arguments: `content` must be a string"))?;
        if content.len() > MAX_FILE_BYTES {
            return Err(ToolError::new(format!(
                "error too_large\n{path}: content is {} bytes (limit {MAX_FILE_BYTES})",
                content.len()
            )));
        }
        validate_expected_hash(arguments.get("expected_hash"))?;
        Ok(if ctx.sandbox.allow_external_paths {
            ToolEffect::HostMutation
        } else {
            ToolEffect::WorkspaceMutation
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.effect(&args, ctx)?;
        let args: WriteArgs = parse_args(args)?;
        let display_path = ctx.display_path(&args.path);
        let target = ctx.resolve_create(&args.path)?;
        let cancellation = ctx.cancellation.clone();
        tokio::task::spawn_blocking(move || {
            create_or_replace(
                &display_path,
                &target,
                &args.path,
                &args.content,
                args.expected_hash.as_deref(),
                &cancellation,
            )
        })
        .await
        .map_err(|error| ToolError::new(format!("error internal\nwrite worker failed: {error}")))?
    }
}

fn stale_error(path: &str, expected: &str, actual: &str) -> ToolError {
    ToolError::new(format!(
        "error stale_file\n{path}  expected hash={expected} actual={actual}\n\
         The file has changed since it was last read."
    ))
}

fn file_error(path_display: &str, error: SecureFileError) -> ToolError {
    match error {
        SecureFileError::NotRegular => ToolError::new(format!(
            "error is_directory\n{path_display}: target is not a regular file"
        )),
        SecureFileError::Changed => ToolError::new(format!(
            "error stale_file\n{path_display}: changed while the write was in progress; retry from a fresh read"
        )),
        SecureFileError::Cancelled => {
            ToolError::new(format!("error cancelled\n{path_display}: write cancelled"))
        }
        other => ToolError::new(format!("error io\n{path_display}: {other}")),
    }
}

fn create_or_replace(
    display_path: &str,
    target: &std::path::Path,
    path: &str,
    content: &str,
    expected_hash: Option<&str>,
    cancellation: &crate::tool::CancellationToken,
) -> Result<ToolOutput, ToolError> {
    if content.len() > MAX_FILE_BYTES {
        return Err(ToolError::new(format!(
            "error too_large\n{path}: content is {} bytes (limit {MAX_FILE_BYTES})",
            content.len()
        )));
    }
    let prepared = PreparedMutation::prepare(target, true, MAX_FILE_BYTES)
        .map_err(|error| file_error(display_path, error))?;
    let old_content = prepared.original().map(<[u8]>::to_vec);
    let exists = old_content.is_some();

    // Hash gate against existing content.
    if let Some(ref current) = old_content {
        if let Some(expected) = expected_hash {
            let actual = content_hash(current);
            if actual != expected {
                return Err(stale_error(display_path, expected, &actual));
            }
        }
    }

    // Generate a diff for every content-changing write. Creation previews are
    // bounded, but still carry a real hunk header so the TUI recognizes and
    // renders them through the same diff path as replacements.
    let detail = if let Some(ref current) = old_content {
        let old_text = String::from_utf8_lossy(current).into_owned();
        if old_text == content {
            String::from("(no change)")
        } else {
            format_unified_diff(path, &old_text, content, &old_text)
        }
    } else {
        format_unified_creation_diff(path, content)
    };

    prepared
        .commit_if(content.as_bytes(), || cancellation.is_cancelled())
        .map_err(|error| file_error(display_path, error))?;
    let verb = if exists { "replaced" } else { "created" };
    let hash = content_hash(content.as_bytes());
    Ok(ToolOutput::new(format!(
        "ok\n{display_path}  {verb} hash={hash}\n{detail}"
    )))
}

#[cfg(test)]
mod tests;
