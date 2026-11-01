use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn invalid_manifest_is_reported_without_an_unsettled_startup() {
    if std::env::var_os("OCTET_MANIFEST_DIAGNOSTIC_PROBE").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "extensions::lifecycle::manifest_diagnostic_tests::invalid_manifest_is_reported_without_an_unsettled_startup", "--nocapture"])
            .env("OCTET_MANIFEST_DIAGNOSTIC_PROBE", "1")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("invalid command identifier `bad/name`"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let extension_root = root.path().join("extensions");
    let directory = extension_root.join("invalid-probe");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(EXTENSION_MANIFEST_FILENAME),
        "name = \"invalid-probe\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"never-run\"\n[contributes]\ncommands = [\"bad/name\"]\n").unwrap();
    let config = super::super::tests::executable_extension_config(
        root.path(),
        &extension_root,
        "invalid-probe",
    );
    let session = Session::create(root.path().join("session.jsonl")).unwrap();
    let sessions = SessionStore::new(&config.session_dir, root.path());
    let mut host = ExtensionHost::new();
    let extensions = ExecutableExtensions::discover_and_start_for_selection(
        &config,
        &session,
        None,
        &sessions,
        &mut host,
        None,
        ExtensionProviderRuntime::default(),
        crate::app::resource_paths::ResourceConsumerCapability::Disabled,
        None,
        "new",
        ExtensionStartupTiming::AfterFirstFrame,
    );
    assert!(!extensions.startup_pending());
    assert!(extensions.processes.is_empty());
    assert!(extensions
        .diagnostics
        .iter()
        .any(|line| line.contains("invalid command identifier `bad/name`")));
}
