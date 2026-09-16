//! Local installed-tree acceptance only: no npm install or real Pi runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    extensions: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("package");
        fs::create_dir_all(source.join("node_modules/local-dependency")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::write(
            source.join("package.json"),
            br#"{
            "name":"local-pi-fixture", "version":"1.0.0", "type":"module",
            "packageManager":"npm@11.0.0",
            "pi":{"extensions":["index.mjs"]},
            "dependencies":{"local-dependency":"1.0.0"},
            "scripts":{"postinstall":"touch SCRIPT_RAN"}
        }"#,
        )
        .unwrap();
        fs::write(
            source.join("index.mjs"),
            b"throw new Error('installer must never import source');\n",
        )
        .unwrap();
        fs::write(
            source.join("node_modules/local-dependency/package.json"),
            br#"{"name":"local-dependency","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        fs::write(
            source.join("node_modules/local-dependency/index.js"),
            b"throw new Error('installer must never import dependency');\n",
        )
        .unwrap();
        Self {
            extensions: root.join("extensions"),
            _temp: temp,
            root,
            source,
        }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_octet"))
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LANG", "C.UTF-8")
            .args(arguments)
            .output()
            .unwrap()
    }

    fn install(&self, controls: &[&str]) -> Output {
        let pi = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extensions/octet-pi-compat/tests/fixtures/fake-pi")
            .canonicalize()
            .unwrap();
        let mut args = vec![
            "pi",
            "install",
            self.source.to_str().unwrap(),
            "--name",
            "pi-local-fixture",
            "--pi-package",
            pi.to_str().unwrap(),
            "--extension-root",
            self.extensions.to_str().unwrap(),
        ];
        args.extend_from_slice(controls);
        self.run(&args)
    }
}

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok()
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
fn installed_local_package_is_inert_and_rollback_preserves_source_and_dependencies() {
    if !node_available() {
        return;
    }
    let fixture = Fixture::new();
    let source_before = fs::read(fixture.source.join("index.mjs")).unwrap();
    let manifest_before = fs::read(fixture.source.join("package.json")).unwrap();
    let dependency = fixture
        .source
        .join("node_modules/local-dependency/index.js");
    let dependency_before = fs::read(&dependency).unwrap();
    successful(fixture.install(&[]));
    let link = fixture.extensions.join("pi-local-fixture");
    assert!(link.join("extension.toml").is_file());
    assert!(link.join("semantic_ui.mjs").is_file());
    assert!(link.join("editor_handoff.mjs").is_file());
    assert!(!fixture.source.join("SCRIPT_RAN").exists());
    assert!(!fixture.root.join("home/.octet/config.toml").exists());
    successful(fixture.run(&[
        "pi",
        "rollback",
        "pi-local-fixture",
        "--extension-root",
        fixture.extensions.to_str().unwrap(),
    ]));
    assert!(!link.exists());
    assert_eq!(
        fs::read(fixture.source.join("index.mjs")).unwrap(),
        source_before
    );
    assert_eq!(
        fs::read(fixture.source.join("package.json")).unwrap(),
        manifest_before
    );
    assert_eq!(fs::read(&dependency).unwrap(), dependency_before);
}

#[test]
fn missing_dependencies_and_unapproved_scripts_fail_before_publication() {
    if !node_available() {
        return;
    }
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.source.join("node_modules")).unwrap();
    let output = fixture.install(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing runtime dependencies"));
    let output = fixture.install(&["--allow-scripts"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--allow-scripts requires --allow-network"));
    assert!(!fixture.extensions.join("pi-local-fixture").exists());
    assert!(!fixture.source.join("node_modules").exists());
    assert!(!fixture.source.join("SCRIPT_RAN").exists());
}

#[test]
fn both_api_links_pin_the_host_version_without_rewriting_activation_or_trust() {
    if !node_available() {
        return;
    }
    for api in ["0.2", "0.3"] {
        let fixture = Fixture::new();
        let config = fixture.root.join("home/.octet/config.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let policy = b"enabled_extensions = [\"existing\"]\ntrusted_extensions = [\"existing\"]\n";
        fs::write(&config, policy).unwrap();
        successful(fixture.install(&["--api-version", api]));
        let manifest = octet_agent::ExtensionManifest::parse(
            &fs::read_to_string(fixture.extensions.join("pi-local-fixture/extension.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.api_version, api);
        assert_eq!(
            manifest.requires_octet.as_deref(),
            Some(format!("={}", env!("CARGO_PKG_VERSION")).as_str())
        );
        assert_eq!(fs::read(&config).unwrap(), policy);
        assert!(!fixture.source.join("SCRIPT_RAN").exists());
    }
}
