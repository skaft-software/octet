//! Exact string replacement in existing files.

use octet_ai::ToolDef;
use serde::Deserialize;

use crate::effect::{ToolEffect, ToolPolicyDenialCode};
use crate::secure_fs::{PreparedMutation, SecureFileError};
use crate::tool::{content_hash, Tool, ToolContext, ToolError, ToolOutput};
use crate::tools::{
    clip_line, format_unified_diff, parse_args, validate_effect_path, validate_expected_hash,
    MAX_FILE_BYTES,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    edits: Vec<Replacement>,
    expected_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    #[serde(alias = "oldText")]
    old: String,
    #[serde(alias = "newText")]
    new: String,
}

fn normalize(mut args: serde_json::Value) -> Result<EditArgs, ToolError> {
    use serde_json::{json, Value};
    let object = args
        .as_object_mut()
        .ok_or_else(|| ToolError::new("invalid arguments: expected object"))?;
    let edits = match object.remove("edits") {
        Some(Value::String(s)) => serde_json::from_str(&s).map_err(|_| {
            ToolError::new("invalid arguments: edits must contain JSON replacements")
        })?,
        value => value.unwrap_or_else(|| json!([])),
    };
    let mut edits = match edits {
        Value::Array(edits) => edits,
        Value::Object(edit) => vec![Value::Object(edit)],
        _ => {
            return Err(ToolError::new(
                "invalid arguments: edits must be an array or a single replacement",
            ))
        }
    };
    for (old, new) in [("old", "new"), ("oldText", "newText")] {
        if object.contains_key(old) || object.contains_key(new) {
            let old = object
                .remove(old)
                .ok_or_else(|| ToolError::new("invalid arguments: missing old replacement text"))?;
            let new = object
                .remove(new)
                .ok_or_else(|| ToolError::new("invalid arguments: missing new replacement text"))?;
            edits.push(json!({"old":old,"new":new}));
        }
    }
    if edits.is_empty() || edits.len() > 1000 {
        return Err(ToolError::new(
            "invalid arguments: edits must contain 1 to 1000 replacements",
        ));
    }
    object.insert("edits".into(), Value::Array(edits));
    validate_expected_hash(object.get("expected_hash"))?;
    let args: EditArgs = parse_args(args)?;
    let mut bytes = 0usize;
    for edit in &args.edits {
        if edit.old.is_empty() {
            return Err(ToolError::new("invalid arguments: `old` must be non-empty"));
        }
        bytes = bytes
            .saturating_add(edit.old.len())
            .saturating_add(edit.new.len());
    }
    if bytes > MAX_FILE_BYTES {
        return Err(ToolError::new(
            "invalid arguments: replacement text exceeds file byte limit",
        ));
    }
    Ok(args)
}

/// The built-in `edit` tool: exact string replacement in an existing file.
///
/// `old` must be non-empty and occur exactly once in the file.  `new` may be
/// empty to delete the matched text.  `expected_hash` from a prior `read`
/// provides optimistic concurrency — the edit is rejected as stale when the
/// current content no longer matches.
///
/// Writes are atomic (temp file + rename in the target directory); a failed
/// edit never leaves partial content behind.
pub struct EditTool;

#[async_trait::async_trait]
impl Tool for EditTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            constrained_sampling: None,
            name: "edit".to_string(),
            description: "Replace unique, non-overlapping regions in one file. Every edits[].oldText \
                          matches the original file, never earlier replacements. Merge overlapping edits. \
                          Legacy old/new remains accepted. expected_hash rejects stale reads."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path; relative to the workspace, or absolute/~/ when enabled."
                    },
                    "edits": {"description":"Replacements against the original file; prefer an array of oldText/newText objects.", "anyOf":[{"type":"array","minItems":1,"maxItems":1000,"items":{"type":"object","properties":{"oldText":{"type":"string"},"newText":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"additionalProperties":false}},{"type":"object"},{"type":"string"}]},
                    "oldText": {"type":"string"},
                    "newText": {"type":"string"},
                    "old": {
                        "type": "string",
                        "description": "Exact existing text to replace; must occur exactly once."
                    },
                    "new": {
                        "type": "string",
                        "description": "Replacement text. May be empty."
                    },
                    "expected_hash": {
                        "type": "string",
                        "description": "Optional hash from read; rejects the edit if the file changed."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call")
    }

    fn prompt_guidelines(&self) -> &[&str] {
        &[
            "Use edit for precise changes (edits[].oldText must match exactly)",
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
        ]
    }

    fn effect(
        &self,
        arguments: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        if !ctx.sandbox.allow_edit {
            return Err(ToolError::policy_denied(
                ToolPolicyDenialCode::EditDisabled,
                "error not_permitted\nedit is disabled by sandbox policy (allow_edit=false)",
            ));
        }
        let arguments = normalize(arguments.clone())?;
        validate_effect_path(&arguments.path, ctx.sandbox.allow_external_paths)?;
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
        let args = normalize(args)?;
        let display_path = ctx.display_path(&args.path);
        let target = ctx.resolve_existing(&args.path)?;
        let cancellation = ctx.cancellation.clone();
        tokio::task::spawn_blocking(move || {
            replace(
                &display_path,
                &target,
                &args.edits,
                args.expected_hash.as_deref(),
                &cancellation,
            )
        })
        .await
        .map_err(|error| ToolError::new(format!("error internal\nedit worker failed: {error}")))?
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
            "error stale_file\n{path_display}: changed while the edit was in progress; retry from a fresh read"
        )),
        SecureFileError::Cancelled => {
            ToolError::new(format!("error cancelled\n{path_display}: edit cancelled"))
        }
        other => ToolError::new(format!("error io\n{path_display}: {other}")),
    }
}

fn replace(
    display_path: &str,
    target: &std::path::Path,
    edits: &[Replacement],
    expected_hash: Option<&str>,
    cancellation: &crate::tool::CancellationToken,
) -> Result<ToolOutput, ToolError> {
    let prepared = PreparedMutation::prepare(target, false, MAX_FILE_BYTES)
        .map_err(|error| file_error(display_path, error))?;
    let current = prepared
        .original()
        .expect("resolve_existing target was present")
        .to_vec();
    let actual = content_hash(&current);
    if let Some(expected) = expected_hash {
        if actual != expected {
            return Err(stale_error(display_path, expected, &actual));
        }
    }

    let text = std::str::from_utf8(&current).map_err(|_| {
        ToolError::new(format!(
            "error invalid_utf8\n{display_path}: replace only supports UTF-8 text; file was not modified"
        ))
    })?;
    let mut regions = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = text
            .find(&edit.old)
            .ok_or_else(|| no_match_error(display_path, &edit.old, text))?;
        let next = start
            + text[start..]
                .chars()
                .next()
                .expect("nonempty match")
                .len_utf8();
        if text[next..].contains(&edit.old) {
            let count = 1 + text[next..].matches(&edit.old).count();
            return Err(ToolError::new(format!("error ambiguous\n{display_path}\n\"{}\" matches {count} locations. Include more surrounding context to make it unique.", clip_line(&edit.old,80))));
        }
        regions.push((start, start + edit.old.len(), edit));
    }
    regions.sort_unstable_by_key(|(start, _, _)| *start);
    if regions.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(ToolError::new("error overlapping_edits\nReplacements overlap in the original file; merge them into one edit."));
    }
    let mut size = text.len();
    for (_, _, edit) in &regions {
        size = size - edit.old.len() + edit.new.len();
    }
    if size > MAX_FILE_BYTES {
        return Err(ToolError::new(format!("error too_large\n{display_path}: edited content is {size} bytes (limit {MAX_FILE_BYTES})")));
    }
    let mut updated = String::with_capacity(size);
    let mut cursor = 0;
    let mut diff = String::new();
    let mut added = 0;
    let mut removed = 0;
    for (start, end, edit) in regions {
        updated.push_str(&text[cursor..start]);
        updated.push_str(&edit.new);
        cursor = end;
        added += edit.new.lines().count();
        removed += edit.old.lines().count();
        if diff.len() < super::MAX_UNIFIED_DIFF_BYTES {
            diff.push_str(&format_unified_diff(
                display_path,
                &edit.old,
                &edit.new,
                text,
            ));
            super::truncate_utf8(&mut diff, super::MAX_UNIFIED_DIFF_BYTES);
        }
    }
    updated.push_str(&text[cursor..]);
    prepared
        .commit_if(updated.as_bytes(), || cancellation.is_cancelled())
        .map_err(|error| file_error(display_path, error))?;
    let hash = content_hash(updated.as_bytes());
    Ok(ToolOutput::new(format!(
        "ok modified=1\n{display_path}  +{added} -{removed} hash={hash}\n{diff}"
    )))
}

/// Builds the `no_match` error, suggesting nearby lines that resemble the
/// first line of the missing `old` text.
fn no_match_error(path: &str, old: &str, text: &str) -> ToolError {
    let mut message = format!(
        "error no_match\n{path}\n\"{}\" not found in file.",
        clip_line(old, 80)
    );
    let needle = old.lines().find(|l| !l.trim().is_empty()).map(str::trim);
    if let Some(needle) = needle {
        let suggestions: Vec<String> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(needle) || line.trim() == needle)
            .take(3)
            .map(|(i, line)| format!("    {}: {}", i + 1, clip_line(line.trim_end(), 120)))
            .collect();
        if !suggestions.is_empty() {
            message.push_str(" Did you mean:\n");
            message.push_str(&suggestions.join("\n"));
        }
    }
    ToolError::new(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxConfig;
    use crate::ToolProgressSink;
    use serde_json::json;
    use std::path::PathBuf;

    struct Fixture {
        _dir: tempfile::TempDir,
        workspace: PathBuf,
        sandbox: SandboxConfig,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let mut sandbox = SandboxConfig::new(&workspace);
        sandbox.allow_edit = true;
        Fixture {
            _dir: dir,
            workspace,
            sandbox,
        }
    }

    impl Fixture {
        fn ctx(&self) -> ToolContext<'_> {
            ToolContext {
                workspace: &self.workspace,
                sandbox: &self.sandbox,
                execution_scope: "edit-test",
                resource_owner: "edit-test",
                active_skills: &[],
                registered_tools: &[],
                progress: ToolProgressSink::null(),
                cancellation: Default::default(),
            }
        }
    }

    #[test]
    fn effect_uses_ambient_path_authority_without_resolving_the_target() {
        let mut fixture = fixture();
        assert_eq!(
            EditTool
                .effect(
                    &json!({"path": "missing.txt", "old": "old", "new": "new"}),
                    &fixture.ctx(),
                )
                .unwrap(),
            ToolEffect::WorkspaceMutation
        );
        fixture.sandbox.allow_external_paths = true;
        for path in ["missing.txt", "/definitely/not/a/real/octet-effect-path"] {
            assert_eq!(
                EditTool
                    .effect(
                        &json!({"path": path, "old": "old", "new": "new"}),
                        &fixture.ctx(),
                    )
                    .unwrap(),
                ToolEffect::HostMutation
            );
        }

        fixture.sandbox.allow_edit = false;
        assert!(EditTool
            .effect(
                &json!({"path": "missing.txt", "old": "old", "new": "new"}),
                &fixture.ctx(),
            )
            .unwrap_err()
            .to_string()
            .contains("allow_edit=false"));
    }

    #[tokio::test]
    async fn replace_exact_unique_match() {
        let f = fixture();
        std::fs::write(f.workspace.join("m.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let out = EditTool
            .execute(
                json!({"path": "m.rs", "old": "fn b() {}", "new": "fn b() -> u8 { 1 }"}),
                &f.ctx(),
            )
            .await
            .unwrap();
        assert!(out.text.starts_with("ok modified=1\nm.rs  +1 -1 hash="));
        assert_eq!(
            std::fs::read_to_string(f.workspace.join("m.rs")).unwrap(),
            "fn a() {}\nfn b() -> u8 { 1 }\n"
        );
    }

    #[test]
    fn unified_diff_uses_the_exact_match_and_counts_context_rows() {
        let full = "needle\nwrong\na\nb\nc\nneedle\nsecond\nafter-1\nafter-2\nafter-3\n";
        let diff = format_unified_diff("m.rs", "needle\nsecond", "replacement", full);
        assert!(diff.contains("@@ -3,8 +3,7 @@"), "{diff}");
        assert!(diff.contains(" c\n-needle\n-second\n+replacement\n after-1"));
    }

    #[tokio::test]
    async fn replace_rejects_invalid_utf8_without_corrupting_the_file() {
        let f = fixture();
        let path = f.workspace.join("binary.dat");
        let original = b"prefix\xffneedle\x80suffix";
        std::fs::write(&path, original).unwrap();

        let error = EditTool
            .execute(
                json!({
                    "path": "binary.dat",
                    "old": "needle",
                    "new": "changed"
                }),
                &f.ctx(),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("invalid_utf8"), "{error}");
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replace_preserves_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let f = fixture();
        let path = f.workspace.join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        EditTool
            .execute(
                json!({
                    "path": "script.sh",
                    "old": "echo old",
                    "new": "echo new"
                }),
                &f.ctx(),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[tokio::test]
    async fn replace_rejects_stale_missing_and_ambiguous() {
        let f = fixture();
        let original = "let x = 1;\nlet x = 1;\nlet y = 2;\n";
        std::fs::write(f.workspace.join("m.rs"), original).unwrap();

        let err = EditTool
            .execute(
                json!({"path": "m.rs", "old": "let y = 2;", "new": "z", "expected_hash": "0".repeat(64)}),
                &f.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("stale_file"), "{err}");

        let err = EditTool
            .execute(
                json!({"path": "m.rs", "old": "let q = 9;", "new": "z"}),
                &f.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("no_match"), "{err}");

        let err = EditTool
            .execute(
                json!({"path": "m.rs", "old": "let x = 1;", "new": "z"}),
                &f.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("ambiguous"), "{err}");
        assert!(err.message.contains("2 locations"), "{err}");

        // Every failure preserved the original content (atomicity).
        assert_eq!(
            std::fs::read_to_string(f.workspace.join("m.rs")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn no_match_suggests_similar_lines() {
        let f = fixture();
        std::fs::write(f.workspace.join("m.rs"), "    let value = compute();\n").unwrap();
        let err = EditTool
            .execute(
                json!({"path": "m.rs", "old": "let value = compute();\nreturn value;", "new": "z"}),
                &f.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("Did you mean"), "{err}");
        assert!(err.message.contains("1: "), "{err}");
    }

    #[tokio::test]
    async fn empty_old_is_rejected() {
        let f = fixture();
        std::fs::write(f.workspace.join("m.rs"), "content").unwrap();
        let err = EditTool
            .execute(json!({"path": "m.rs", "old": "", "new": "x"}), &f.ctx())
            .await
            .unwrap_err();
        assert!(err.message.contains("non-empty"), "{err}");
    }

    #[tokio::test]
    async fn empty_new_deletes_matched_text() {
        let f = fixture();
        std::fs::write(f.workspace.join("m.rs"), "keep\nremove me\nkeep\n").unwrap();
        let out = EditTool
            .execute(
                json!({"path": "m.rs", "old": "remove me\n", "new": ""}),
                &f.ctx(),
            )
            .await
            .unwrap();
        assert!(out.text.starts_with("ok modified=1\nm.rs  +0 -1 hash="));
        assert_eq!(
            std::fs::read_to_string(f.workspace.join("m.rs")).unwrap(),
            "keep\nkeep\n"
        );
    }

    #[tokio::test]
    async fn edit_requires_allow_edit() {
        let f = fixture();
        let mut sandbox = f.sandbox.clone();
        sandbox.allow_edit = false;
        let ctx = ToolContext {
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "edit-test",
            resource_owner: "edit-test",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        };
        let err = EditTool
            .execute(json!({"path": "x.txt", "old": "a", "new": "b"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.message.contains("not_permitted"), "{err}");
        assert_eq!(
            err.policy_denial_code(),
            Some(ToolPolicyDenialCode::EditDisabled)
        );
    }

    #[tokio::test]
    async fn trusted_local_mode_edits_an_absolute_path() {
        let f = fixture();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "old").unwrap();
        let mut sandbox = f.sandbox.clone();
        sandbox.allow_external_paths = true;
        let ctx = ToolContext {
            workspace: &f.workspace,
            sandbox: &sandbox,
            execution_scope: "edit-test",
            resource_owner: "edit-test",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        };

        EditTool
            .execute(
                json!({
                    "path": outside.path().to_string_lossy(),
                    "old": "old",
                    "new": "new"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(outside.path()).unwrap(), "new");
    }

    #[tokio::test]
    async fn edit_rejects_escaping_paths() {
        let f = fixture();
        for op in [
            json!({"path": "../evil.txt", "old": "a", "new": "b"}),
            json!({"path": "/etc/hosts", "old": "a", "new": "b"}),
        ] {
            let err = EditTool.execute(op, &f.ctx()).await.unwrap_err();
            assert!(
                err.message.contains("..") || err.message.contains("absolute"),
                "{err}"
            );
        }
    }
}
