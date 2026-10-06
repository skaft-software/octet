//! Real App -> reviewed Pi factory -> resident BridgeManager -> local stdio MCP.
//! Only inference is scripted. No network, alternate MCP manager or protocol peer.
use super::*;
use crate::app::App;
use octet_ai::{
    AiError, Cost, Diagnostic, HostStreamModel, HostStreamTransport, Request, Response,
    ResponseStream, StopReason, StreamEvent, ToolCall, Usage,
};
use serde_json::json;

struct NoConfirmations;
impl ExtensionConfirmationHandler for NoConfirmations {
    fn confirm<'a>(
        &'a mut self,
        _: &'a str,
        _: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        Box::pin(std::future::ready(Ok(false)))
    }
}
struct LocalInference {
    calls: std::sync::atomic::AtomicUsize,
    tool_name: String,
}
#[async_trait::async_trait]
impl HostStreamTransport for LocalInference {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let content = if first {
            let tool = request
                .tools
                .iter()
                .find(|tool| tool.name == self.tool_name)
                .expect("resident dynamic tool must reach the actual provider catalog");
            vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId("pi-mcp-native-call".into()),
                name: tool.name.clone(),
                arguments_json: "{}".into(),
                argument_error: None,
                async_execution: false,
            })]
        } else {
            vec![AssistantPart::Text("MCP completed".into())]
        };
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(Response {
                message: AssistantMessage {
                    model: model.id,
                    protocol: model.protocol,
                    content,
                },
                stop_reason: if first {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
                usage: Usage::default(),
                cost: Some(Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

const EMPTY_MCP_CONFIG: &str = "{\"version\":1,\"servers\":{}}\n";

// App discovery reads process-global HOME. Re-exec just this test rather than
// racing other test threads with set_var, or bypassing real App discovery.
fn run_in_disposable_home(name: &str) -> bool {
    const CHILD: &str = "OCTET_TEST_PI_MCP_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap();
    let temporary = home.join("tmp");
    std::fs::create_dir(&temporary).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("extensions::mcp_native_tests::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env(CHILD, name)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .env("LANG", "C")
        .env("PI_OFFLINE", "1")
        .env("JITI_FS_CACHE", "false")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(&home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !timed_out && output.status.success() && stdout.contains("1 passed"),
        "isolated {name} failed (timed_out={timed_out})\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.join(".octet/mcp.json").exists());
    true
}

fn assert_config_unchanged(root: &Path) {
    assert_eq!(
        std::fs::read_to_string(root.join("mcp.json")).unwrap(),
        EMPTY_MCP_CONFIG,
        "transient registration never persists into MCP config"
    );
}

fn app(root: &Path, resident: bool) -> App {
    use std::os::unix::fs::PermissionsExt as _;

    let mcp_config = root.join("mcp.json");
    std::fs::write(&mcp_config, EMPTY_MCP_CONFIG).unwrap();
    std::fs::set_permissions(&mcp_config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let adapter = source.join("extensions/octet-pi-compat");
    let package = source.join("extensions/octet-mcp");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let server = root.join("server.py");
    // Instrument the package's existing conforming stdio fixture, not the host.
    let code = std::fs::read_to_string(package.join("fixtures/real_mcp_server.py")).unwrap()
        .replace("        name = params.get(\"name\")", "        with open('calls.jsonl', 'a') as log:\n            log.write(json.dumps(params) + '\\n')\n        name = params.get(\"name\")");
    let code = code.replace("for line in sys.stdin:", "with open('starts.jsonl', 'a') as log:\n    log.write('started\\n')\n\nfor line in sys.stdin:");
    std::fs::write(&server, code).unwrap();
    let entry = root.join("factory.mjs");
    std::fs::write(&entry, format!(r#"
import {{ appendFileSync }} from 'node:fs';
export default pi => {{
  pi.on('session_start', () => appendFileSync('session-starts.jsonl', 'started\n'));
  const config = {{command: 'python3', args: [{}], exposure: 'direct'}};
  pi.registerMcpServer('native-proof', config);
  pi.registerCommand('replace-mcp', {{handler: () => pi.registerMcpServer('native-proof', {{...config, env: {{PROOF: 'literal'}}}})}});
  pi.registerCommand('remove-mcp', {{handler: () => pi.unregisterMcpServer('native-proof')}});
}};
"#, json!(server))).unwrap();
    let extensions = root.join("extensions");
    let capture = std::process::Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .args(["--reviewed", "--output"])
        .arg(extensions.join("octet-pi-compat"))
        .arg(entry)
        .current_dir(&workspace)
        .env("PI_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        capture.status.success(),
        "{}",
        String::from_utf8_lossy(&capture.stderr)
    );
    // Capture is inert with respect to MCP startup.
    assert!(!workspace.join("calls.jsonl").exists());
    assert!(!workspace.join("starts.jsonl").exists());
    if resident {
        let bridge = extensions.join("octet-mcp");
        std::fs::create_dir_all(&bridge).unwrap();
        let mut manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(package.join("extension.toml")).unwrap())
                .unwrap();
        // The resident package's own runtime and vendored SDK, loaded locally.
        // Explicit missing files are invalid (only the missing default is
        // empty). Use a real private empty config, never the developer's HOME.
        let launch = format!("import sys; sys.path[:0]=[{},{}]; from octet_mcp.runtime import main; raise SystemExit(main())",
            json!(package.join("vendor")), json!(package));
        let entrypoint = manifest["entrypoint"].as_table_mut().unwrap();
        entrypoint.insert("command".into(), toml::Value::String("python3".into()));
        // The shipped manifest has no `args`; IndexMut on a missing key panics.
        entrypoint.insert(
            "args".into(),
            toml::Value::Array(
                vec![
                    "-c".into(),
                    launch,
                    "--config".into(),
                    mcp_config.to_string_lossy().into_owned(),
                ]
                .into_iter()
                .map(toml::Value::String)
                .collect(),
            ),
        );
        std::fs::write(
            bridge.join("extension.toml"),
            toml::to_string(&manifest).unwrap(),
        )
        .unwrap();
    }
    let mut config = tests::executable_extension_config(&workspace, &extensions, "octet-pi-compat");
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.workspace_trusted = true;
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    config.max_turns = Some(4);
    if resident {
        config.enabled_extensions.push("octet-mcp".into());
    }
    let boot = crate::app::bootstrap::bootstrap(config).unwrap();
    let launch = crate::app::bootstrap::resolve_launch_print(&boot, "pi-mcp-native").unwrap();
    let app =
        crate::app::bootstrap::build_app_with_resource_consumer(boot, launch, "MCP test".into())
            .unwrap();
    assert!(
        app.executable_extensions.processes.iter().any(|process| {
            process.descriptor().manifest.name == "octet-pi-compat" && process.is_running()
        }),
        "reviewed Pi factory was not admitted; diagnostics {:?}",
        app.executable_extensions
            .diagnostics
            .iter()
            .collect::<Vec<_>>()
    );
    if resident {
        bridge(&app);
    }
    app
}

#[track_caller]
fn bridge(app: &App) -> ExtensionProcess {
    match app
        .executable_extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "octet-mcp")
    {
        Some(process) => process.clone(),
        None => panic!(
            "resident bridge missing; admitted processes {:?}; diagnostics {:?}",
            app.executable_extensions
                .processes
                .iter()
                .map(|process| (
                    &process.descriptor().manifest.name,
                    process.health_snapshot().state
                ))
                .collect::<Vec<_>>(),
            app.executable_extensions
                .diagnostics
                .iter()
                .collect::<Vec<_>>()
        ),
    }
}
async fn catalog(app: &mut App, expected: usize) {
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            app.executable_extensions.drain_events();
            app.executable_extensions.drain_background_updates();
            if bridge(app).tool_definitions().len() == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "resident catalog did not settle: expected {expected}, got {}; pending session starts {}; diagnostics {:?}",
        bridge(app).tool_definitions().len(),
        app.executable_extensions.pending_session_hook_starts.len(),
        app.executable_extensions
            .diagnostics
            .iter()
            .collect::<Vec<_>>()
    );
}

async fn mcp_command(app: &mut App, name: &str) -> anyhow::Result<Option<String>> {
    let mut confirmations = NoConfirmations;
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        app.executable_extensions.execute_command_with_confirmation(
            name,
            Vec::new(),
            &mut confirmations,
        ),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "native MCP command {name} timed out; diagnostics {:?}",
        app.executable_extensions
            .diagnostics
            .iter()
            .collect::<Vec<_>>()
    );
    outcome.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_mcp_native_headless_drain_starts_without_resource_discovery_once() {
    if run_in_disposable_home("pi_mcp_native_headless_drain_starts_without_resource_discovery_once")
    {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut app = app(&root, true);
    assert!(!app.executable_extensions.has_resource_consumer_processes());
    assert!(!app.resource_paths_pending());
    assert!(
        app.executable_extensions
            .pending_session_hook_starts
            .iter()
            .any(|(process, _)| process.descriptor().manifest.name == "octet-pi-compat"),
        "Pi session_start was not queued; diagnostics {:?}",
        app.executable_extensions
            .diagnostics
            .iter()
            .collect::<Vec<_>>()
    );
    assert!(!app.config.workspace.join("starts.jsonl").exists());
    assert!(!app.config.workspace.join("session-starts.jsonl").exists());

    // No resource discovery, shell pump, command, or explicit lifecycle call:
    // the ordinary headless event drain must dispatch the deferred hook and
    // service its reverse request through the resident BridgeManager.
    catalog(&mut app, 3).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            app.executable_extensions.drain_events();
            if app
                .executable_extensions
                .pending_session_hook_starts
                .is_empty()
                && app
                    .executable_extensions
                    .session_hook_start_tasks
                    .iter()
                    .all(JoinHandle::is_finished)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("headless session_start did not settle");
    for _ in 0..3 {
        app.executable_extensions.drain_events();
        tokio::task::yield_now().await;
    }
    assert_eq!(
        std::fs::read_to_string(app.config.workspace.join("session-starts.jsonl")).unwrap(),
        "started\n",
        "the Pi startup callback runs exactly once"
    );
    assert_eq!(
        std::fs::read_to_string(app.config.workspace.join("starts.jsonl")).unwrap(),
        "started\n",
        "the actual local stdio server starts exactly once"
    );
    assert!(!app.config.workspace.join("calls.jsonl").exists());
    // release_binding intentionally clears App's process list. Keep the real
    // admitted handle to inspect its catalog after owner cleanup.
    let resident = bridge(&app);
    app.executable_extensions.release_binding().await;
    assert!(app.executable_extensions.processes.is_empty());
    assert!(resident.tool_definitions().is_empty());
    assert_config_unchanged(&root);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_mcp_native_app_register_replace_call_remove_and_owner_cleanup() {
    if run_in_disposable_home("pi_mcp_native_app_register_replace_call_remove_and_owner_cleanup") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut app = app(&root, true);
    app.refresh_resource_paths_headless().await.unwrap();
    catalog(&mut app, 3).await;
    let resident = bridge(&app);
    let tools = resident.tool_definitions();
    // The resident adds a hashed Pi namespace and truncates the upstream name
    // to keep its published identity within 64 bytes. Locate this known local
    // fixture by its description, then require that exact identity at inference.
    let unknown = tools
        .iter()
        .find(|tool| {
            tool.description
                .ends_with("Exercise fail-closed approval when annotations are absent")
        })
        .unwrap_or_else(|| {
            panic!(
                "unknown-effect fixture missing from real resident catalog: {:?}",
                tools.iter().map(|tool| &tool.name).collect::<Vec<_>>()
            )
        });
    // Ownerless calls cannot use even a valid transient catalog identity.
    let denied = resident
        .call_tool(&unknown.name, json!({}), resident.current_context())
        .await
        .unwrap();
    assert!(denied.is_error);
    assert!(!app.config.workspace.join("calls.jsonl").exists());
    let starts = std::fs::read_to_string(app.config.workspace.join("starts.jsonl")).unwrap();
    app.executable_extensions.effect_policy = octet_agent::EffectPolicy::Controlled;
    let refused = mcp_command(&mut app, "replace-mcp").await.unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("transient MCP process startup requires unsafe_host"),
        "the real process-policy gate must refuse registration, not an unrelated API: {refused:#}"
    );
    assert_eq!(
        std::fs::read_to_string(app.config.workspace.join("starts.jsonl")).unwrap(),
        starts
    );
    app.executable_extensions.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    mcp_command(&mut app, "replace-mcp").await.unwrap();
    catalog(&mut app, 3).await;
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(LocalInference {
            calls: std::sync::atomic::AtomicUsize::new(0),
            tool_name: unknown.name.clone(),
        }),
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        app.agent.complete("call the MCP tool"),
    )
    .await
    .unwrap()
    .unwrap();
    let calls = std::fs::read_to_string(app.config.workspace.join("calls.jsonl")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "real Agent and policy must dispatch exactly once"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(calls.trim()).unwrap()["name"],
        "fixture_unknown_effect",
        "the published identity must dispatch to the intended upstream tool"
    );
    assert!(
        std::fs::read_to_string(app.agent.session().path())
            .unwrap()
            .contains("should be policy gated"),
        "actual native tool result must be durable"
    );
    mcp_command(&mut app, "remove-mcp").await.unwrap();
    catalog(&mut app, 0).await;
    mcp_command(&mut app, "replace-mcp").await.unwrap();
    catalog(&mut app, 3).await;
    app.executable_extensions.release_binding().await;
    assert!(
        resident.tool_definitions().is_empty(),
        "owner retirement must remove the overlay"
    );
    assert_config_unchanged(&root);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_mcp_native_missing_resident_never_starts_or_persists_a_server() {
    if run_in_disposable_home("pi_mcp_native_missing_resident_never_starts_or_persists_a_server") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut app = app(&root, false);
    let _ = app.refresh_resource_paths_headless().await;
    let refused = mcp_command(&mut app, "replace-mcp").await.unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("resident octet-mcp is not enabled"),
        "the missing resident must refuse registration, not an unrelated API: {refused:#}"
    );
    assert!(!app.config.workspace.join("calls.jsonl").exists());
    assert!(!app.config.workspace.join("starts.jsonl").exists());
    assert_config_unchanged(&root);
    app.executable_extensions.shutdown().await;
}
