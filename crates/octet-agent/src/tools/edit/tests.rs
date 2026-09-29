//! Unit tests for the `edit` tool's exact-match and patch handling.
//!
//! Separate from `edit.rs` so the tool's matching rules stay legible as a
//! single narrative rather than source interleaved with cases.
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
