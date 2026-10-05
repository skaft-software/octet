//! Shared real-host fixture for the bounded Pi public API contract tests.
#![cfg(unix)]
use super::*;

pub(super) fn pi_app(factory: &str) -> (tempfile::TempDir, App) {
    pi_app_policy(factory, octet_agent::EffectPolicy::UnsafeHost)
}

pub(super) fn pi_app_policy(factory: &str, policy: octet_agent::EffectPolicy) -> (tempfile::TempDir, App) {
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let entry = directory.path().join("probe.ts");
    let trace = serde_json::to_string(&directory.path().join("trace.jsonl")).unwrap();
    std::fs::write(&entry, factory.replace("TRACE", &trace)).unwrap();
    let extension_root = directory.path().join("extensions");
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize().unwrap();
    let configured = std::process::Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed").arg("--output")
        .arg(extension_root.join("octet-pi-compat")).arg(&entry)
        .current_dir(&app.config.workspace).env("PI_OFFLINE", "1")
        .output().expect("existing Node and adapter dependencies are required");
    assert!(configured.status.success(), "{}", String::from_utf8_lossy(&configured.stderr));
    app.config.extension_paths = vec![extension_root];
    app.config.enabled_extensions = vec!["octet-pi-compat".into()];
    app.config.invocation_trusted_extensions = vec!["octet-pi-compat".into()];
    app.config.workspace_trusted = true;
    app.config.mode = crate::config::Mode::Interactive;
    app.config.effect_policy = policy;
    app.config.sandbox.allow_process = true;
    app.config.sandbox.allow_shell = true;
    let mut host = octet_agent::ExtensionHost::new();
    let mut extensions = crate::extensions::ExecutableExtensions::discover_and_start_with_provider_runtime(
        &app.config, app.agent.session(), &app.model, &app.reasoning, &app.sessions,
        &mut host, None, crate::extensions::ExtensionProviderRuntime::default(),
        crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
    );
    assert!(extensions.summaries().iter().any(|s| s.name == "octet-pi-compat" && s.running),
        "{}", extensions.inspect_text());
    host.finalize_tool_surface();
    // Preserve the real App frontend consumer through rebuild_app. The adapter
    // reserves resources_discover even for factories without resource callbacks.
    app.resource_paths = crate::app::resource_paths::ResourcePathConsumer::new(
        &app.config, &app.skills, &app.prompts, &extensions, &mut host,
        crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
    );
    app.executable_extensions = extensions;
    app.executable_extensions.activate_session_lifecycle_driver();
    (directory, app)
}

pub(super) async fn command(app: &mut App, shell: &mut InteractiveShell, name: &str) -> anyhow::Result<String> {
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let mut frontend = InteractiveExtensionConfirmations { shell, input: &mut input, dialogs: &dialogs };
    tokio::time::timeout(Duration::from_secs(20), run_interactive_extension_command(
        app, &mut frontend, Some("octet-pi-compat"), name, Vec::new(), false, 0,
    )).await.expect("Pi command timed out")
        .map(|output| output.expect("Pi command is registered"))
}
