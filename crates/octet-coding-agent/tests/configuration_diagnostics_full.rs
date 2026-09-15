//! Credential-free process-boundary coverage for configuration diagnostics.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(config: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(home.join(".octet")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(home.join(".octet/config.toml"), config).unwrap();
    (root, home, workspace)
}

fn invoke(root: &Path, home: &Path, workspace: &Path, strict: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
    command
        .current_dir(workspace)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("TERM", "dumb")
        .args(["--offline", "--no-context-files", "--workspace"])
        .arg(workspace)
        .args(["--session-dir"])
        .arg(root.join("sessions"))
        .args(["--color", "never", "sessions", "list"]);
    if strict {
        command.env("OCTET_STRICT_CONFIG", "true");
    }
    command.output().expect("run isolated octet subprocess")
}

#[test]
fn default_configuration_diagnostics_are_emitted_on_stderr() {
    let (root, home, workspace) = fixture("modle = 'ignored'\n");
    let output = invoke(root.path(), &home, &workspace, false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!(
        r#"warning: global config {}:1:1: unknown configuration key "modle"; did you mean "model"?"#,
        home.join(".octet/config.toml").display()
    );

    assert!(
        output.status.success(),
        "credential-free command failed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stderr.contains(&expected), "missing diagnostic: {stderr}");
    assert!(!stdout.contains(&expected), "diagnostic leaked to stdout: {stdout}");
    assert!(stdout.contains("No matching sessions"));
}

#[test]
fn octet_strict_config_environment_rejects_unknown_keys() {
    let (root, home, workspace) = fixture("modle = 'ignored'\n");
    let output = invoke(root.path(), &home, &workspace, true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!(
        r#"global config {}:1:1: unknown configuration key "modle"; did you mean "model"?"#,
        home.join(".octet/config.toml").display()
    );

    assert!(!output.status.success(), "strict mode unexpectedly succeeded");
    assert!(stderr.contains("strict configuration rejected unknown keys"));
    assert!(stderr.contains(&expected), "missing strict diagnostic: {stderr}");
    assert!(!stderr.contains("warning:"), "strict mode emitted a warning: {stderr}");
}
