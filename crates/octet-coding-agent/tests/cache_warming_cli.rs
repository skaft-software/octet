//! Isolated process-boundary policy precedence and response-only local commands.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(user: &str, project: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(home.join(".octet")).unwrap();
        fs::create_dir_all(workspace.join(".octet")).unwrap();
        fs::write(home.join(".octet/config.toml"), user).unwrap();
        fs::write(workspace.join(".octet/config.toml"), project).unwrap();
        Self {
            root,
            home,
            workspace,
        }
    }

    fn invoke(&self, env: Option<&str>, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .env_clear()
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .env("ANTHROPIC_API_KEY", "isolated-local-test-key")
            .args([
                "--offline",
                "--no-context-files",
                "--no-tools",
                "--workspace-trusted",
                "--model",
                "claude-sonnet-4-6",
                "--workspace",
            ])
            .arg(&self.workspace)
            .arg("--session-dir")
            .arg(self.root.path().join("sessions"))
            .args(args);
        if let Some(mode) = env {
            command.env("OCTET_CACHE_WARMING", mode);
        }
        command.output().unwrap()
    }
}

#[test]
fn cache_warming_environment_cli_and_global_only_policy_are_resolved_at_process_boundary() {
    let fixture = Fixture::new(
        "cache_warming = 'off'\nshow_cache_miss_notices = false\n",
        "cache_warming = 'invalid-project-mode'\nshow_cache_miss_notices = true\n",
    );
    let output = fixture.invoke(Some("idle"), &["--print", "/cache-warming"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Mode: idle"));
    let output = fixture.invoke(
        Some("idle"),
        &["--cache-warming", "streaming", "--print", "/cache-warming"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Mode: streaming"));
    let output = fixture.invoke(None, &["--print", "/cache-warming"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Mode: off"));
    let output = fixture.invoke(Some("invalid-env-mode"), &["sessions", "list"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid-env-mode"));
}

#[test]
fn cache_warming_local_command_persists_only_user_config_and_never_emits_response_text() {
    let fixture = Fixture::new(
        "# owner preference\ncache_warming = 'off'\nshow_cache_miss_notices = false\n",
        "cache_warming = 'off'\n",
    );
    let output = fixture.invoke(None, &["--print", "/cache-warming idle"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let user = fs::read_to_string(fixture.home.join(".octet/config.toml")).unwrap();
    assert!(user.contains("# owner preference"));
    assert!(user.contains("cache_warming = \"idle\""));
    assert_eq!(
        fs::read_to_string(fixture.workspace.join(".octet/config.toml")).unwrap(),
        "cache_warming = 'off'\n"
    );
    let output = fixture.invoke(None, &["--print", "/cache-warming always"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
