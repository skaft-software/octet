//! Shared real-host fixture for the bounded Pi public API contract tests.
#![cfg(unix)]
use super::*;

pub(super) fn pi_app(factory: &str) -> (tempfile::TempDir, App) {
    pi_app_policy(factory, octet_agent::EffectPolicy::UnsafeHost)
}

pub(super) fn pi_app_policy(
    factory: &str,
    policy: octet_agent::EffectPolicy,
) -> (tempfile::TempDir, App) {
    pi_app_policy_with_frontend(factory, policy, None, false)
}

/// Construct the real terminal consumer before process negotiation and retain
/// that exact shell for command/input/frame acceptance. No synthetic UI peer.
pub(super) fn pi_ui_app(factory: &str) -> (tempfile::TempDir, App, InteractiveShell) {
    let shell = InteractiveShell::test_shell();
    let (directory, app) = pi_app_policy_with_frontend(
        factory,
        octet_agent::EffectPolicy::UnsafeHost,
        Some(&shell),
        false,
    );
    assert!(
        app.executable_extensions.remote_ui_wake().is_some(),
        "live shell consumer must bind before initialize"
    );
    (directory, app, shell)
}

/// Real controlled Agent with workspace-only reads, configured before the
/// rebuild constructs it. Other Pi fixture profiles keep their existing scope.
pub(super) fn pi_workspace_ui_app(factory: &str) -> (tempfile::TempDir, App, InteractiveShell) {
    let shell = InteractiveShell::test_shell();
    let (directory, app) = pi_app_policy_with_frontend(
        factory,
        octet_agent::EffectPolicy::Controlled,
        Some(&shell),
        true,
    );
    assert!(
        app.executable_extensions.remote_ui_wake().is_some(),
        "live shell consumer must bind before initialize"
    );
    (directory, app, shell)
}

/// The Pi fixture plus one enabled native executable extension. The popup's
/// single registry is only exercised when both extension kinds contribute.
pub(super) fn pi_ui_app_with_native_command(
    factory: &str,
    extension: &str,
    command: &str,
) -> (tempfile::TempDir, App, InteractiveShell) {
    let shell = InteractiveShell::test_shell();
    let (directory, app) = pi_app_policy_with_frontend_and_native(
        factory,
        octet_agent::EffectPolicy::UnsafeHost,
        Some(&shell),
        false,
        Some((extension, command)),
        false,
    );
    assert!(
        app.executable_extensions.remote_ui_wake().is_some(),
        "live shell consumer must bind before initialize"
    );
    (directory, app, shell)
}

fn pi_app_policy_with_frontend(
    factory: &str,
    policy: octet_agent::EffectPolicy,
    frontend: Option<&InteractiveShell>,
    workspace_only: bool,
) -> (tempfile::TempDir, App) {
    pi_app_policy_with_frontend_and_native(factory, policy, frontend, workspace_only, None, false)
}

/// Deliberately unconfigured runner: its actual captured hook catalog has no
/// resource consumer. Keep generic non-resource startup barrier/failure coverage
/// independent of the palette hook reserved by every configured bridge.
pub(super) fn pi_non_resource_app(factory: &str) -> (tempfile::TempDir, App) {
    pi_app_policy_with_frontend_and_native(
        factory,
        octet_agent::EffectPolicy::UnsafeHost,
        None,
        false,
        None,
        true,
    )
}

fn write_direct_pi_fixture(
    adapter: &std::path::Path,
    entry: &std::path::Path,
    output: &std::path::Path,
    workspace: &std::path::Path,
) {
    let runner = adapter.join("runner.mjs");
    let captured = std::process::Command::new("node")
        .arg(&runner)
        .arg("--inspect")
        .arg(entry)
        .current_dir(workspace)
        .env("PI_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        captured.status.success(),
        "{}",
        String::from_utf8_lossy(&captured.stderr)
    );
    let frame: serde_json::Value = serde_json::from_slice(&captured.stdout).unwrap();
    let metadata = &frame["result"];
    assert!(!metadata["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hook| hook == "resources_discover"));
    assert!(metadata["shortcuts"].as_array().unwrap().is_empty());
    assert!(metadata["flags"].as_array().unwrap().is_empty());
    let names = |field: &str| -> Vec<&str> {
        metadata[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect()
    };
    std::fs::create_dir_all(output).unwrap();
    std::fs::write(
        output.join("extension.toml"),
        format!(
            r#"name = "octet-pi-compat"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "node"
args = {args}
[capabilities]
filesystem = "unrestricted"
process = true
network = true
system_prompt = true
[contributes]
tools = {tools}
commands = {commands}
hooks = {hooks}
tool_renderers = {renderers}
notifications = true
confirmations = true
providers = true
"#,
            args = serde_json::json!([runner, entry]),
            tools = serde_json::json!(names("tools")),
            commands = serde_json::json!(names("commands")),
            hooks = metadata["hooks"],
            renderers = metadata["tool_renderers"],
        ),
    )
    .unwrap();
}

/// A minimal API 0.1 extension that answers `command/execute` with one bounded
/// text line. It is the native (non-Pi) half of the single command registry.
fn write_native_command_extension(root: &std::path::Path, extension: &str, command: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = root.join(extension);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("extension.toml"),
        format!(
            r#"name = {extension:?}
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "probe.sh"

[contributes]
commands = [{command:?}]
"#
        ),
    )
    .unwrap();
    let fixture = directory.join("probe.sh");
    std::fs::write(
        &fixture,
        format!(
            r#"#!/bin/sh
request_id() {{ sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }}
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"api_version":"0.1","tools":[],"commands":[{{"name":{command:?},"description":"Native fixture command"}}]}}}}\n' "$id"
while IFS= read -r request; do
  case "$request" in
    *'"method":"command/execute"'*)
      id=$(printf '%s' "$request" | request_id)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"text":"native fixture ran"}}}}\n' "$id"
      ;;
    *'"method":"shutdown"'*)
      id=$(printf '%s' "$request" | request_id)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"
      exit 0
      ;;
  esac
done
"#
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();
}

fn pi_app_policy_with_frontend_and_native(
    factory: &str,
    policy: octet_agent::EffectPolicy,
    frontend: Option<&InteractiveShell>,
    workspace_only: bool,
    native: Option<(&str, &str)>,
    direct_runner: bool,
) -> (tempfile::TempDir, App) {
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    if workspace_only {
        // Agent construction freezes the broker and sandbox. Mutating only
        // app.config after rebuild would leave all reads classified HostRead.
        app.config.effect_policy = policy;
        app.config.sandbox.allow_external_paths = false;
    }
    // Production session naming and lookup use the workspace SessionStore.
    // The estimate fixture's transcript deliberately lives outside that store.
    app.sessions.write_workspace_marker().unwrap();
    let session_path = app.sessions.new_path("pi-contract");
    app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::CreateNew(session_path)),
    )
    .unwrap();
    let entry = directory.path().join("probe.ts");
    let trace = serde_json::to_string(&directory.path().join("trace.jsonl")).unwrap();
    std::fs::write(&entry, factory.replace("TRACE", &trace)).unwrap();
    let extension_root = directory.path().join("extensions");
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    if direct_runner {
        write_direct_pi_fixture(
            &adapter,
            &entry,
            &extension_root.join("octet-pi-compat"),
            &app.config.workspace,
        );
    } else {
        let configured = std::process::Command::new("node")
            .arg(adapter.join("configure.mjs"))
            .arg("--reviewed")
            .arg("--output")
            .arg(extension_root.join("octet-pi-compat"))
            .arg(&entry)
            .current_dir(&app.config.workspace)
            .env("PI_OFFLINE", "1")
            .env("OCTET_PI_AGENT_DIR", directory.path().join("pi-agent"))
            .output()
            .expect("existing Node and adapter dependencies are required");
        assert!(
            configured.status.success(),
            "{}",
            String::from_utf8_lossy(&configured.stderr)
        );
    }
    if let Some((extension, command)) = native {
        write_native_command_extension(&extension_root, extension, command);
    }
    app.config.extension_paths = vec![extension_root];
    app.config.enabled_extensions = vec!["octet-pi-compat".into()];
    app.config.invocation_trusted_extensions = vec!["octet-pi-compat".into()];
    if let Some((extension, _)) = native {
        app.config.enabled_extensions.push(extension.into());
        app.config
            .invocation_trusted_extensions
            .push(extension.into());
    }
    app.config.workspace_trusted = true;
    app.config.mode = crate::config::Mode::Interactive;
    app.config.effect_policy = policy;
    app.config.sandbox.allow_process = true;
    app.config.sandbox.allow_shell = true;
    let mut host = octet_agent::ExtensionHost::new();
    let mut extensions =
        crate::extensions::ExecutableExtensions::discover_and_start_with_provider_runtime(
            &app.config,
            app.agent.session(),
            &app.model,
            &app.reasoning,
            &app.sessions,
            &mut host,
            None,
            crate::extensions::ExtensionProviderRuntime::default(),
            crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
            frontend,
        );
    assert!(
        extensions
            .summaries()
            .iter()
            .any(|s| s.name == "octet-pi-compat" && s.running),
        "{}",
        extensions.inspect_text()
    );
    if let Some((extension, _)) = native {
        assert!(
            extensions
                .summaries()
                .iter()
                .any(|s| s.name == extension && s.running),
            "{}",
            extensions.inspect_text()
        );
    }
    host.finalize_tool_surface();
    // Preserve the real App frontend capability through rebuild_app. Resource
    // loading is enabled for every configured bridge, including an empty inventory.
    app.resource_paths = crate::app::resource_paths::ResourcePathConsumer::new(
        &app.config,
        &app.skills,
        &app.prompts,
        &extensions,
        &mut host,
        crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
    );
    app.executable_extensions = extensions;
    app.executable_extensions
        .activate_session_lifecycle_driver();
    (directory, app)
}

pub(super) async fn command(
    app: &mut App,
    shell: &mut InteractiveShell,
    name: &str,
) -> anyhow::Result<String> {
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(Duration::from_secs(20), async {
        // Match the product's pre-command idle boundary. Discovery leaves Pi
        // session_start deferred; starting a command first can race that hook
        // (and its reload guard) with synchronous tool catalog publication.
        resource_paths::refresh_resource_paths(app, shell, &mut input).await?;
        let dialogs = app.executable_extensions.lifecycle_snapshot();
        let mut frontend = InteractiveExtensionConfirmations {
            shell,
            input: &mut input,
            dialogs: &dialogs,
            input_retired: false,
        };
        run_interactive_extension_command(
            app,
            &mut frontend,
            Some("octet-pi-compat"),
            name,
            Vec::new(),
            false,
            0,
        )
        .await
    })
    .await
    .expect("Pi command timed out")
    .map(|output| output.expect("Pi command is registered"))
}
