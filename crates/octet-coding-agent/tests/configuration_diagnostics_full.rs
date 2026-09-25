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

fn invoke(
    root: &Path,
    home: &Path,
    workspace: &Path,
    strict: Option<&str>,
    extra_args: &[&str],
) -> Output {
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
        .args(extra_args)
        .args(["--color", "never", "sessions", "list"]);
    if let Some(strict) = strict {
        command.env("OCTET_STRICT_CONFIG", strict);
    }
    command.output().expect("run isolated octet subprocess")
}

#[test]
fn default_configuration_diagnostics_are_emitted_on_stderr() {
    let (root, home, workspace) = fixture("modle = 'ignored'\n");
    let output = invoke(root.path(), &home, &workspace, None, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!(
        "warning: global config {}:1:1: unknown configuration key \"modle\"; did you mean \"model\"?\n",
        home.join(".octet/config.toml").display()
    );

    assert!(
        output.status.success(),
        "credential-free command failed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stderr, expected);
    assert!(
        !stdout.contains("unknown configuration key"),
        "diagnostic leaked to stdout: {stdout}"
    );
    assert!(stdout.contains("No matching sessions"));
}

#[test]
fn octet_strict_config_environment_rejects_unknown_keys() {
    let (root, home, workspace) = fixture("modle = 'ignored'\n");
    let output = invoke(root.path(), &home, &workspace, Some("true"), &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!(
        "Error: strict configuration rejected unknown keys:<U+000A>  - global config {}:1:1: unknown configuration key \"modle\"; did you mean \"model\"?\n",
        home.join(".octet/config.toml").display()
    );

    assert!(
        !output.status.success(),
        "strict mode unexpectedly succeeded"
    );
    assert_eq!(stderr, expected);
    assert!(
        !stderr.contains("warning:"),
        "strict mode emitted a warning: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "strict mode ran the sessions command"
    );
}

#[test]
fn trusted_layers_preserve_diagnostic_order_and_overridden_typos() {
    let (root, home, workspace) =
        fixture("themee = 'ignored'\nmodle = 'ignored'\n[compaction]\nkeep_recent_turn = 2\n");
    fs::create_dir_all(workspace.join(".octet")).unwrap();
    fs::write(
        workspace.join(".octet/config.toml"),
        "modle = 'also ignored'\nthemee = 'also ignored'\n",
    )
    .unwrap();
    let global = home.join(".octet/config.toml");
    let project = workspace.canonicalize().unwrap().join(".octet/config.toml");
    let diagnostics = [
        format!("global config {}:4:1: unknown configuration key \"compaction.keep_recent_turn\"; did you mean \"compaction.keep_recent_turns\"?", global.display()),
        format!("global config {}:2:1: unknown configuration key \"modle\"; did you mean \"model\"?", global.display()),
        format!("global config {}:1:1: unknown configuration key \"themee\"; did you mean \"theme\"?", global.display()),
        format!("project config {}:1:1: unknown configuration key \"modle\"; did you mean \"model\"?", project.display()),
        format!("project config {}:2:1: unknown configuration key \"themee\"; did you mean \"theme\"?", project.display()),
    ];

    let warning = invoke(
        root.path(),
        &home,
        &workspace,
        None,
        &["--workspace-trusted"],
    );
    let stderr = String::from_utf8_lossy(&warning.stderr);
    assert!(warning.status.success(), "{stderr}");
    let expected = diagnostics
        .iter()
        .map(|line| format!("warning: {line}\n"))
        .collect::<String>();
    assert_eq!(stderr, expected);

    let strict = invoke(
        root.path(),
        &home,
        &workspace,
        Some("true"),
        &["--workspace-trusted"],
    );
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(!strict.status.success());
    let expected = format!(
        "Error: strict configuration rejected unknown keys:<U+000A>{}\n",
        diagnostics
            .iter()
            .map(|line| format!("  - {line}"))
            .collect::<Vec<_>>()
            .join("<U+000A>")
    );
    assert_eq!(stderr, expected);
    assert!(!stderr.contains("warning:"), "{stderr}");
    assert!(strict.stdout.is_empty());
}

#[test]
fn untrusted_project_diagnostics_are_not_loaded_even_in_strict_mode() {
    let (root, home, workspace) = fixture("");
    fs::create_dir_all(workspace.join(".octet")).unwrap();
    fs::write(workspace.join(".octet/config.toml"), "modle = 'ignored'\n").unwrap();

    let output = invoke(root.path(), &home, &workspace, Some("true"), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("No matching sessions"));
}

#[test]
fn strict_environment_overrides_toml_but_not_explicit_cli_strictness() {
    let (root, home, workspace) = fixture("strict_config = true\nmodle = 'ignored'\n");
    let warning = invoke(root.path(), &home, &workspace, Some("false"), &[]);
    let stderr = String::from_utf8_lossy(&warning.stderr);
    assert!(warning.status.success(), "{stderr}");
    assert!(stderr.starts_with("warning: global config "), "{stderr}");
    assert!(!stderr.contains("strict configuration rejected"));

    let strict = invoke(
        root.path(),
        &home,
        &workspace,
        Some("false"),
        &["--strict-config"],
    );
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(!strict.status.success());
    assert!(
        stderr.contains("strict configuration rejected unknown keys"),
        "{stderr}"
    );
    assert!(!stderr.contains("warning:"), "{stderr}");
    assert!(strict.stdout.is_empty());
}

#[test]
fn strict_environment_accepts_aliases_and_ignored_legacy_keys() {
    let (root, home, workspace) =
        fixture("show_turn_cost = true\nexec_timeout_secs = 30\n[compaction]\npolicy = 'local'\n");
    let output = invoke(root.path(), &home, &workspace, Some("true"), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("No matching sessions"));
}
