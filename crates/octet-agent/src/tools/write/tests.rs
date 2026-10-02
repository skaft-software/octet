//! Unit tests for the `write` tool.
//!
//! Separate from `write.rs` so the tool's small surface and its cases do not
//! occupy the same file.
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
    sandbox.allow_write = true;
    Fixture {
        _dir: dir,
        workspace,
        sandbox,
    }
}

fn diff_metadata(output: &ToolOutput) -> String {
    output
        .details()
        .and_then(|details| details.metadata())
        .and_then(|metadata| metadata.get("diff"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

impl Fixture {
    fn ctx(&self) -> ToolContext<'_> {
        ToolContext {
            workspace: &self.workspace,
            sandbox: &self.sandbox,
            execution_scope: "write-test",
            resource_owner: "write-test",
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
        WriteTool
            .effect(
                &json!({"path": "missing.txt", "content": "content"}),
                &fixture.ctx(),
            )
            .unwrap(),
        ToolEffect::WorkspaceMutation
    );
    fixture.sandbox.allow_external_paths = true;
    for path in ["missing.txt", "/definitely/not/a/real/octet-effect-path"] {
        assert_eq!(
            WriteTool
                .effect(&json!({"path": path, "content": "content"}), &fixture.ctx(),)
                .unwrap(),
            ToolEffect::HostMutation
        );
    }

    fixture.sandbox.allow_write = false;
    assert!(WriteTool
        .effect(
            &json!({"path": "missing.txt", "content": "content"}),
            &fixture.ctx(),
        )
        .unwrap_err()
        .to_string()
        .contains("allow_write=false"));
}

#[tokio::test]
async fn creates_file_and_parent_dirs() {
    let f = fixture();
    let out = WriteTool
        .execute(
            json!({"path": "src/new/mod.rs", "content": "pub fn x() {}\n"}),
            &f.ctx(),
        )
        .await
        .unwrap();
    let expected_hash = content_hash(b"pub fn x() {}\n");
    assert!(
        out.text
            .starts_with(&format!("ok\nsrc/new/mod.rs  created hash={expected_hash}")),
        "{}",
        out.text
    );
    let diff = diff_metadata(&out);
    assert!(
        diff.contains("--- /dev/null"),
        "missing diff header: {}",
        out.text
    );
    assert!(
        diff.contains("@@ -0,0 +1,1 @@"),
        "missing diff hunk: {}",
        out.text
    );
    assert!(
        diff.contains("+pub fn x() {}"),
        "missing preview line: {}",
        out.text
    );
    assert_eq!(
        std::fs::read_to_string(f.workspace.join("src/new/mod.rs")).unwrap(),
        "pub fn x() {}\n"
    );
}

#[tokio::test]
async fn overwrite_existing_gated_by_expected_hash() {
    let f = fixture();
    std::fs::write(f.workspace.join("a.txt"), "old content").unwrap();
    let good = content_hash(b"old content");

    // Wrong hash: rejected, file preserved.
    let err = WriteTool
        .execute(
            json!({"path": "a.txt", "content": "new", "expected_hash": "0".repeat(64)}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("stale_file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(f.workspace.join("a.txt")).unwrap(),
        "old content"
    );

    // Matching hash: replacement proceeds.
    let out = WriteTool
        .execute(
            json!({"path": "a.txt", "content": "new", "expected_hash": good}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(out.text.contains("replaced"), "{}", out.text);

    // No hash at all: last-write-wins overwrite.
    let out = WriteTool
        .execute(json!({"path": "a.txt", "content": "newest"}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.contains("replaced"), "{}", out.text);
    assert_eq!(
        std::fs::read_to_string(f.workspace.join("a.txt")).unwrap(),
        "newest"
    );
}

#[tokio::test]
async fn empty_content_creates_empty_file_not_deletes() {
    let f = fixture();
    let out = WriteTool
        .execute(json!({"path": "empty.txt", "content": ""}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.contains("created"), "{}", out.text);
    assert!(f.workspace.join("empty.txt").exists());
    assert_eq!(
        std::fs::read_to_string(f.workspace.join("empty.txt")).unwrap(),
        ""
    );
}

#[tokio::test]
async fn requires_allow_write() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.allow_write = false;
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "write-test",
        resource_owner: "write-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
    };
    let err = WriteTool
        .execute(json!({"path": "x.txt", "content": "x"}), &ctx)
        .await
        .unwrap_err();
    assert!(err.message.contains("not_permitted"), "{err}");
    assert_eq!(
        err.policy_denial_code(),
        Some(ToolPolicyDenialCode::WriteDisabled)
    );
    assert!(!f.workspace.join("x.txt").exists());
}

#[tokio::test]
async fn cancellation_prevents_the_rename_commit() {
    let f = fixture();
    let cancellation = crate::CancellationToken::default();
    cancellation.cancel();
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &f.sandbox,
        execution_scope: "write-cancel-test",
        resource_owner: "write-cancel-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation,
    };
    let error = WriteTool
        .execute(
            json!({"path": "cancelled.txt", "content": "must not commit"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("cancelled"), "{error}");
    assert!(!f.workspace.join("cancelled.txt").exists());
}

#[tokio::test]
async fn rejects_directory_and_escaping_paths() {
    let f = fixture();
    std::fs::create_dir(f.workspace.join("sub")).unwrap();

    let err = WriteTool
        .execute(json!({"path": "sub", "content": "x"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains("is_directory"), "{err}");

    let err = WriteTool
        .execute(json!({"path": "../evil.txt", "content": "x"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains(".."), "{err}");
}

#[tokio::test]
async fn escaped_diff_metadata_cannot_fail_after_a_successful_mutation() {
    let f = fixture();
    let content = "\u{0001}".repeat(20_000) + "λ";
    // Exercise both creation and replacement with a diff that expands 6x
    // during JSON serialization.
    for initial in [None, Some("old")] {
        if let Some(initial) = initial {
            std::fs::write(f.workspace.join("escaped.txt"), initial).unwrap();
        }
        let out = WriteTool
            .execute(json!({"path": "escaped.txt", "content": content}), &f.ctx())
            .await
            .unwrap();
        assert!(out.text.starts_with("ok"));
        assert_eq!(
            std::fs::read_to_string(f.workspace.join("escaped.txt")).unwrap(),
            content
        );
        let metadata = out.details().unwrap().metadata().unwrap();
        assert!(
            serde_json::to_vec(metadata).unwrap().len() <= crate::tool::MAX_TOOL_METADATA_BYTES
        );
        assert!(diff_metadata(&out).contains("unified diff truncated"));
    }
}
