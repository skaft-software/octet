//! Removed Pi bridge CLI regression; portable migration remains available.

use std::fs;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("home/.octet")).unwrap();
        // Early CLI exits and read-only migration must not need valid product
        // configuration, provider credentials, or a session.
        fs::write(root.path().join("home/.octet/config.toml"), "invalid = [").unwrap();
        fs::create_dir_all(root.path().join("pi-home")).unwrap();
        fs::write(root.path().join("pi-home/settings.json"), "{}").unwrap();
        Self { root }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_octet"))
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("LANG", "C.UTF-8")
            .args(arguments)
            .output()
            .unwrap()
    }

    fn assert_unchanged(&self) {
        assert_eq!(
            fs::read_to_string(self.root.path().join("home/.octet/config.toml")).unwrap(),
            "invalid = ["
        );
        assert_eq!(
            fs::read_to_string(self.root.path().join("pi-home/settings.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            fs::read_dir(self.root.path().join("home/.octet"))
                .unwrap()
                .count(),
            1
        );
    }
}

fn successful(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn help_exposes_migration_but_not_the_removed_pi_command() {
    let fixture = Fixture::new();
    let help = successful(fixture.run(&["--help"]));
    let commands = help
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect::<Vec<_>>();
    assert!(commands.contains(&"migrate"));
    assert!(!commands.contains(&"pi"));
    let help = fixture.run(&["help", "pi"]);
    assert!(!help.status.success());
    fixture.assert_unchanged();
}

#[test]
fn removed_bridge_options_are_rejected_before_startup() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["pi", "list", "--extension-root", "./extensions"],
        vec!["pi", "install", "./extension.ts", "--pi-package", "./pi"],
        vec!["pi", "plan", "./extension.ts", "--pi-package", "./pi"],
        vec!["pi", "preflight", "--plan", "./plan.json"],
        vec!["pi", "publish", "--plan", "./plan.json"],
        vec![
            "pi",
            "rollback",
            "old-link",
            "--extension-root",
            "./extensions",
        ],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("unexpected argument"), "{error}");
        assert!(output.stdout.is_empty());
    }
    fixture.assert_unchanged();
}

#[test]
fn pi_migration_still_runs_read_only_without_the_bridge() {
    let fixture = Fixture::new();
    let output = successful(fixture.run(&[
        "migrate",
        "pi",
        "--dry-run",
        "--json",
        "--pi-home",
        "pi-home",
        "--project",
        ".",
    ]));
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["source"], "pi");
    assert_eq!(report["mode"], "dry_run");
    assert_eq!(report["model_usage"], "disabled");
    assert_eq!(report["package_code_executed"], false);
    successful(fixture.run(&["migrate", "import", "pi", "--help"]));
    fixture.assert_unchanged();
}
