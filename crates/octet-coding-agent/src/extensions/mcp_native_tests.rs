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
struct LocalInference(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl HostStreamTransport for LocalInference {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        let content = if first {
            let tool = request
                .tools
                .iter()
                .find(|tool| tool.name.contains("fixture_unknown_effect"))
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

fn app(root: &Path, resident: bool) -> App {
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
export default pi => {{
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
        // An isolated absent config path avoids reading the developer's HOME.
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
                    root.join("absent-mcp.json").to_string_lossy().into_owned(),
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
    crate::app::bootstrap::build_app_with_resource_consumer(boot, launch, "MCP test".into())
        .unwrap()
}

fn bridge(app: &App) -> ExtensionProcess {
    app.executable_extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "octet-mcp")
        .expect("resident bridge must be admitted through normal App discovery")
        .clone()
}
async fn catalog(app: &mut App, expected: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            app.executable_extensions.drain_events();
            if bridge(app).tool_definitions().len() == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("resident catalog did not settle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_mcp_native_app_register_replace_call_remove_and_owner_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut app = app(&root, true);
    app.refresh_resource_paths_headless().await.unwrap();
    catalog(&mut app, 3).await;
    let resident = bridge(&app);
    let unknown = resident
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name.contains("fixture_unknown_effect"))
        .unwrap();
    // Ownerless calls cannot use even a valid transient catalog identity.
    let denied = resident
        .call_tool(&unknown.name, json!({}), resident.current_context())
        .await
        .unwrap();
    assert!(denied.is_error);
    assert!(!app.config.workspace.join("calls.jsonl").exists());
    let mut confirmations = NoConfirmations;
    let starts = std::fs::read_to_string(app.config.workspace.join("starts.jsonl")).unwrap();
    app.executable_extensions.effect_policy = octet_agent::EffectPolicy::Controlled;
    assert!(
        app.executable_extensions
            .execute_command_with_confirmation("replace-mcp", Vec::new(), &mut confirmations)
            .await
            .is_err(),
        "registration must not bypass the host process policy"
    );
    assert_eq!(
        std::fs::read_to_string(app.config.workspace.join("starts.jsonl")).unwrap(),
        starts
    );
    app.executable_extensions.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    app.executable_extensions
        .execute_command_with_confirmation("replace-mcp", Vec::new(), &mut confirmations)
        .await
        .unwrap();
    catalog(&mut app, 3).await;
    app.client.register_host_stream_transport(
        app.model.endpoint.id.clone(),
        Arc::new(LocalInference(std::sync::atomic::AtomicUsize::new(0))),
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
    assert!(
        std::fs::read_to_string(app.agent.session().path())
            .unwrap()
            .contains("should be policy gated"),
        "actual native tool result must be durable"
    );
    app.executable_extensions
        .execute_command_with_confirmation("remove-mcp", Vec::new(), &mut confirmations)
        .await
        .unwrap();
    catalog(&mut app, 0).await;
    app.executable_extensions
        .execute_command_with_confirmation("replace-mcp", Vec::new(), &mut confirmations)
        .await
        .unwrap();
    catalog(&mut app, 3).await;
    app.executable_extensions.release_binding().await;
    assert!(
        resident.tool_definitions().is_empty(),
        "owner retirement must remove the overlay"
    );
    assert!(
        !root.join("absent-mcp.json").exists(),
        "transient registration never persists MCP config"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_mcp_native_missing_resident_never_starts_or_persists_a_server() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut app = app(&root, false);
    let _ = app.refresh_resource_paths_headless().await;
    let mut confirmations = NoConfirmations;
    assert!(app
        .executable_extensions
        .execute_command_with_confirmation("replace-mcp", Vec::new(), &mut confirmations)
        .await
        .is_err());
    assert!(!app.config.workspace.join("calls.jsonl").exists());
    assert!(!app.config.workspace.join("starts.jsonl").exists());
    assert!(!root.join("absent-mcp.json").exists());
    app.executable_extensions.shutdown().await;
}
