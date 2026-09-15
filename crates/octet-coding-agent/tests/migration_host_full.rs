//! Black-box qualification coverage for the typed Pi import and host-owned
//! ingestion boundary.
//!
//! These tests deliberately invoke the real `octet` binary. They keep the source
//! tree and destination home in separate temporary directories, and the child
//! environment contains no credentials or inherited application state.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture {
    root: tempfile::TempDir,
    source: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("migration fixture tempdir");
        let source = root.path().join("pi");
        let home = root.path().join("home");
        fs::create_dir_all(source.join("skills/review")).unwrap();
        fs::create_dir_all(&home).unwrap();

        fs::write(
            source.join("settings.json"),
            br#"{
  "model": "openai/gpt-4o-mini",
  "mcpServers": {
    "docs": {
      "command": "docs-mcp",
      "args": ["--stdio"],
      "env": {"TOKEN": "MIGRATION_SECRET"},
      "headers": {"Authorization": "Bearer migration-header"},
      "cwd": "/private/source"
    }
  },
  "permissions": {"allow": ["read"]}
}
"#,
        )
        .unwrap();
        fs::write(
            source.join("skills/review/SKILL.md"),
            "Review the proposed change before approving it.\n",
        )
        .unwrap();

        Self { root, source, home }
    }

    fn run(&self, args: &[String]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
        command
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", &self.home)
            .env("LANG", "C.UTF-8")
            .env("PATH", "/usr/bin:/bin");
        #[cfg(windows)]
        command.env("USERPROFILE", &self.home);
        command
            .args(args)
            .output()
            .expect("run octet migration command")
    }

    fn import(&self, flags: &[&str]) -> Output {
        self.import_from(&self.source, flags)
    }

    fn import_from(&self, source: &Path, flags: &[&str]) -> Output {
        let mut args = vec![
            "--offline".to_owned(),
            "migrate".to_owned(),
            "import".to_owned(),
            "pi".to_owned(),
            "--source".to_owned(),
            source.display().to_string(),
        ];
        args.extend(flags.iter().map(|flag| (*flag).to_owned()));
        self.run(&args)
    }

    fn restore(&self, backup: &str, force: bool) -> Output {
        let mut args = vec![
            "--offline".to_owned(),
            "migrate".to_owned(),
            "restore".to_owned(),
            backup.to_owned(),
        ];
        if force {
            args.push("--yes".to_owned());
        }
        self.run(&args)
    }
}

fn json_report(output: Output) -> Value {
    assert!(
        output.status.success(),
        "migration command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("migration JSON report")
}

#[test]
fn typed_import_preview_apply_and_idempotent_rerun_are_host_owned() {
    let fixture = Fixture::new();
    let source_before = fs::read(fixture.source.join("settings.json")).unwrap();

    let preview = json_report(fixture.import(&["--dry-run", "--json"]));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["models_updated"], 1);
    assert_eq!(preview["skills_disabled"], 1);
    assert_eq!(preview["mcp_servers_disabled"], 1);
    assert!(preview["diagnostics"].as_u64().unwrap() >= 1);
    assert!(!fixture.home.join(".octet").exists());

    let applied = json_report(fixture.import(&["--yes", "--json"]));
    assert_eq!(applied["dry_run"], false);
    assert_eq!(applied["models_updated"], 1);
    assert_eq!(applied["skills_disabled"], 1);
    assert_eq!(applied["mcp_servers_disabled"], 1);
    let backup = applied["backup"]
        .as_str()
        .expect("mutating import reports its backup")
        .to_owned();

    let config = fs::read_to_string(fixture.home.join(".octet/config.toml")).unwrap();
    assert!(config.contains("model = \"gpt-4o-mini\""));
    let skill = fs::read_to_string(fixture.home.join(".octet/skills/review/SKILL.md")).unwrap();
    assert!(skill.contains("disable-model-invocation: true"));
    let mcp: Value =
        serde_json::from_slice(&fs::read(fixture.home.join(".octet/mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["servers"]["docs"]["enabled"], false);
    assert_eq!(mcp["servers"]["docs"]["required"], false);
    assert!(mcp["servers"]["docs"].get("env").is_none());
    assert!(mcp["servers"]["docs"].get("headers").is_none());

    for target in [
        fixture.home.join(".octet/config.toml"),
        fixture.home.join(".octet/mcp.json"),
        fixture.home.join(".octet/skills/review/SKILL.md"),
        fixture.home.join(".octet/migrations/pi-state.json"),
    ] {
        let bytes = fs::read(target).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("MIGRATION_SECRET"));
        assert!(!String::from_utf8_lossy(&bytes).contains("migration-header"));
    }
    assert_eq!(
        source_before,
        fs::read(fixture.source.join("settings.json")).unwrap()
    );

    let rerun = json_report(fixture.import(&["--yes", "--json"]));
    assert_eq!(rerun["dry_run"], false);
    assert_eq!(rerun["models_updated"], 0);
    assert_eq!(rerun["skills_disabled"], 0);
    assert_eq!(rerun["mcp_servers_disabled"], 0);
    assert!(rerun["unchanged"].as_u64().unwrap() >= 3);
    assert!(rerun["backup"].is_null());

    let restored = fixture.restore(&backup, false);
    assert!(
        restored.status.success(),
        "restore failed: stdout={} stderr={}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
    assert!(!fixture.home.join(".octet/config.toml").exists());
    assert!(!fixture.home.join(".octet/mcp.json").exists());
    assert!(!fixture.home.join(".octet/skills/review/SKILL.md").exists());
    assert!(!fixture
        .home
        .join(".octet/migrations/pi-state.json")
        .exists());
}

#[test]
fn changed_imported_data_requires_explicit_noninteractive_confirmation() {
    let fixture = Fixture::new();
    let first = json_report(fixture.import(&["--yes", "--json"]));
    let backup = first["backup"].as_str().unwrap().to_owned();
    let skill_path = fixture.home.join(".octet/skills/review/SKILL.md");
    fs::write(&skill_path, "User-authored review policy.\n").unwrap();

    let cancelled = fixture.import(&["--json"]);
    assert!(!cancelled.status.success());
    let error = String::from_utf8_lossy(&cancelled.stderr);
    assert!(
        error.contains("rerun with --yes"),
        "unexpected error: {error}"
    );
    assert_eq!(
        fs::read_to_string(&skill_path).unwrap(),
        "User-authored review policy.\n"
    );

    let refused_restore = fixture.restore(&backup, false);
    assert!(!refused_restore.status.success());
    assert!(String::from_utf8_lossy(&refused_restore.stderr).contains("changed after import"));
    assert_eq!(
        fs::read_to_string(&skill_path).unwrap(),
        "User-authored review policy.\n"
    );

    let forced_restore = fixture.restore(&backup, true);
    assert!(
        forced_restore.status.success(),
        "forced restore failed: stdout={} stderr={}",
        String::from_utf8_lossy(&forced_restore.stdout),
        String::from_utf8_lossy(&forced_restore.stderr)
    );
    assert!(!skill_path.exists());
}

#[test]
fn no_match_is_reported_without_destination_artifacts() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.source.join("settings.json")).unwrap();
    fs::remove_dir_all(fixture.source.join("skills")).unwrap();

    let report = json_report(fixture.import(&["--dry-run", "--json"]));
    assert_eq!(report["detected"], false);
    assert_eq!(report["changed"], false);
    assert!(!fixture.home.join(".octet").exists());
}

#[cfg(unix)]
#[test]
fn adapter_rejection_is_explicit_and_leaves_destination_untouched() {
    let fixture = Fixture::new();
    let linked_source = fixture.root.path().join("pi-link");
    std::os::unix::fs::symlink(&fixture.source, &linked_source).unwrap();

    let output = fixture.import_from(&linked_source, &["--dry-run", "--json"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("Pi migration adapter rejected migration/detect"),
        "unexpected adapter error: {error}"
    );
    assert!(!fixture.home.join(".octet").exists());
}
