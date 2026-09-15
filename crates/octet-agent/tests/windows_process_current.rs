#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use octet_agent::{
    BashTool, CancellationToken, SandboxConfig, Tool, ToolContext, ToolProgressSink,
};
use serde_json::json;

fn bash_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OCTET_WINDOWS_BASH_PATH") {
        return Some(path.into());
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            let path = PathBuf::from(root).join("Git/bin/bash.exe");
            if path.is_file() && !is_legacy_wsl_bash_path(&path) {
                return Some(path);
            }
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("bash.exe"))
            .find(|path| path.is_file() && !is_legacy_wsl_bash_path(path))
    })
}

fn is_legacy_wsl_bash_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
    normalized.ends_with("\\windows\\system32\\bash.exe")
        || normalized.ends_with("\\windows\\sysnative\\bash.exe")
}

fn context<'a>(workspace: &'a Path, sandbox: &'a SandboxConfig) -> ToolContext<'a> {
    ToolContext {
        workspace,
        sandbox,
        execution_scope: "windows-process-current",
        resource_owner: "windows-process-current",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: CancellationToken::default(),
    }
}

#[tokio::test]
async fn explicit_windows_bash_preserves_bash_c_and_cwd_semantics() {
    let Some(shell) = bash_path() else {
        eprintln!("skipping Windows Bash qualification: Git Bash was not found");
        return;
    };
    let directory = tempfile::tempdir().expect("temporary workspace");
    let workspace = directory.path().canonicalize().expect("canonical workspace");
    std::fs::create_dir(workspace.join("subdir")).expect("subdirectory");

    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    sandbox.shell_path = Some(shell);
    sandbox.bash_timeout = Duration::from_secs(5);
    let output = BashTool
        .execute(
            json!({
                "command": "printf 'brace-%s\\n' brace-{one,two}; test -d .",
                "cwd": "subdir"
            }),
            &context(&workspace, &sandbox),
        )
        .await
        .expect("Bash command");

    assert!(output.text.starts_with("exit=0"), "{}", output.text);
    assert!(output.text.contains("brace-brace-one"), "{}", output.text);
    assert!(output.text.contains("brace-brace-two"), "{}", output.text);
}

#[tokio::test]
async fn windows_job_cleanup_bounds_an_infinite_bash_command() {
    let Some(shell) = bash_path() else {
        eprintln!("skipping Windows Job Object qualification: Git Bash was not found");
        return;
    };
    let directory = tempfile::tempdir().expect("temporary workspace");
    let workspace = directory.path().canonicalize().expect("canonical workspace");
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    sandbox.shell_path = Some(shell);
    sandbox.bash_timeout = Duration::from_secs(5);

    let started = Instant::now();
    let error = BashTool
        .execute(
            json!({
                "command": "while true; do :; done",
                "timeout_ms": 100
            }),
            &context(&workspace, &sandbox),
        )
        .await
        .expect_err("infinite Bash command must time out");

    assert!(error.message.contains("error timeout"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "Windows Job Object cleanup exceeded the bounded timeout: {error}"
    );
}
