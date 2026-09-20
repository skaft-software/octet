//! Black-box Pi import regression coverage.

use std::fs;
use std::process::Command;

#[test]
fn pi_adapter_source_package_manifest_validates_without_model_tools() {
    // Validate the actual source adapter without rewriting its host/API contract.
    let source = include_str!("../../../extensions/octet-import-pi/extension.toml");
    let manifest = octet_agent::ExtensionManifest::parse(source).unwrap();
    assert_eq!(manifest.name, "octet-import-pi");
    assert_eq!(manifest.api_version, "0.4");
    assert_eq!(
        manifest.requires_octet.as_deref(),
        Some(concat!("=", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(manifest.entrypoint.command, "extension.sh");
    assert!(manifest.entrypoint.args.is_empty());
    assert!(manifest.contributes.tools.is_empty());
    assert!(!manifest.capabilities.process);
    assert!(!manifest.capabilities.network);
}

#[test]
fn dry_run_import_maps_canonical_model_without_destination_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("pi");
    let home = temp.path().join("home");
    let xdg_config = temp.path().join("xdg-config");
    let xdg_cache = temp.path().join("xdg-cache");
    let xdg_data = temp.path().join("xdg-data");
    let xdg_runtime = temp.path().join("xdg-runtime");
    for directory in [
        &source,
        &home,
        &xdg_config,
        &xdg_cache,
        &xdg_data,
        &xdg_runtime,
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    let settings = b"{\"model\":\"openai/gpt-4o-mini\"}\n";
    fs::write(source.join("settings.json"), settings).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_octet"))
        .current_dir(temp.path())
        .env_clear()
        .env_remove("OCTET_SESSION_ID")
        .env_remove("OCTET_SESSION_DB")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_RUNTIME_DIR", &xdg_runtime)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .args(["--offline", "migrate", "import", "pi", "--source"])
        .arg(&source)
        .args(["--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "import failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["models_updated"], 1);
    assert_eq!(report["skipped"], 0);
    assert_eq!(report["diagnostics"], 0);
    assert!(report.get("model_diagnostics").is_none());
    assert_eq!(fs::read(source.join("settings.json")).unwrap(), settings);
    assert!(
        fs::read_dir(&home).unwrap().next().is_none(),
        "dry run created destination artifacts"
    );
}

#[test]
fn import_preserves_policy_and_source_while_disabling_imported_code() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("pi");
    let home = temp.path().join("home");
    fs::create_dir_all(source.join("skills/review")).unwrap();
    fs::create_dir_all(home.join(".octet")).unwrap();
    let policy = b"enabled_extensions = [\"existing\"]\ntrusted_extensions = [\"existing\"]\n";
    let config = home.join(".octet/config.toml");
    fs::write(&config, policy).unwrap();
    let settings = br#"{"mcpServers":{"review":{"command":"never-execute-pi-import-fixture","args":["--stdio"],"enabled":true,"required":true,"env":{"TOKEN":"PI_IMPORT_SECRET"},"headers":{"Authorization":"PI_IMPORT_SECRET"},"cwd":"/private/source"}},"trusted_extensions":["unreviewed"]}"#;
    let skill = b"---\nname: review\ndisable-model-invocation: false\n---\nReview carefully.\n";
    fs::write(source.join("settings.json"), settings).unwrap();
    fs::write(source.join("skills/review/SKILL.md"), skill).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_octet"))
            .current_dir(temp.path())
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .args(["--offline", "migrate", "import", "pi", "--source"])
            .arg(&source)
            .arg("--json")
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["skills_disabled"], 1);
    assert_eq!(report["mcp_servers_disabled"], 1);
    let mcp_bytes = fs::read(home.join(".octet/mcp.json")).unwrap();
    let mcp: serde_json::Value = serde_json::from_slice(&mcp_bytes).unwrap();
    let server = &mcp["servers"]["review"];
    assert_eq!(server["enabled"], false);
    assert_eq!(server["required"], false);
    for field in ["env", "headers", "cwd"] {
        assert!(server.get(field).is_none());
    }
    for bytes in [&output.stdout, &output.stderr, &mcp_bytes] {
        assert!(!String::from_utf8_lossy(bytes).contains("PI_IMPORT_SECRET"));
    }
    let imported_skill = fs::read_to_string(home.join(".octet/skills/review/SKILL.md")).unwrap();
    let frontmatter = imported_skill.split("---").nth(1).unwrap();
    assert!(frontmatter.contains("disable-model-invocation: true"));
    assert_eq!(fs::read(&config).unwrap(), policy);
    assert_eq!(fs::read(source.join("settings.json")).unwrap(), settings);
    assert_eq!(
        fs::read(source.join("skills/review/SKILL.md")).unwrap(),
        skill
    );
    let repeated = run();
    assert!(repeated.status.success());
    let report: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(report["skills_disabled"], 0);
    assert_eq!(report["mcp_servers_disabled"], 0);
    assert_eq!(fs::read(&config).unwrap(), policy);
    assert_eq!(fs::read(home.join(".octet/mcp.json")).unwrap(), mcp_bytes);
}
