//! Product-level API 0.3 active-session reverse requests.
//!
//! The agent crate owns the typed transport. These tests drive the real
//! product boundary: discovery injects one session-lifecycle driver into an
//! isolated API 0.3 process, the product activates/deactivates it exactly as
//! the interactive frontend does, and only a negotiated peer can reach it.
#![cfg(unix)]
use super::*;
use octet_agent::extension_process::{
    ExtensionSessionLifecycleError, ExtensionSessionLifecycleOperation,
};
use serde_json::json;

const PROBE_EXTENSION: &str = "lifecycle-probe";

/// One reverse request per tool call. The fixture never invents authority: it
/// selects `session_lifecycle` only when the host offer contains it and echoes
/// whatever terminal response the host produced.
const SESSION_LIFECYCLE_PROBE: &str = r#"import json
import sys

mode = sys.argv[1]
sys.path.insert(0, sys.argv[2])
from octet_extension.api_v03 import canonical_json

pending_call = None
next_id = 0


def send(value):
    sys.stdout.write(canonical_json(value) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        offer = request["params"]["contract"]
        capabilities = offer["required_capabilities"][:]
        methods = offer["required_methods"][:]
        if mode == "select" and "session_lifecycle" in offer["optional_capabilities"]:
            capabilities.append("session_lifecycle")
            methods.extend(name for name in offer["optional_methods"] if name.startswith("session/"))
        send({"jsonrpc": "2.0", "id": request["id"], "result": {
            "api_version": "0.3",
            "contract": {"schema": offer["schema"], "encoding": offer["encoding"],
                          "capabilities": sorted(capabilities), "methods": sorted(methods),
                          "limits": offer["limits"]},
            "tools": [{"name": "probe", "description": "Session lifecycle probe",
                        "parameters": {"type": "object"}}]}})
    elif method == "tool/call":
        arguments = request["params"]["arguments"]
        next_id += 1
        pending_call = request["id"]
        send({"jsonrpc": "2.0", "id": "probe-" + str(next_id),
              "method": arguments["method"], "params": arguments["params"]})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": request["id"], "result": {"terminal": "shutdown"}})
        break
    elif pending_call is not None:
        send({"jsonrpc": "2.0", "id": pending_call, "result": {
            "content": [], "is_error": False, "metadata": None,
            "structured_content": {"response": request}}})
        pending_call = None
"#;

struct ProbeState {
    _root: tempfile::TempDir,
    extensions: ExecutableExtensions,
    process: ExtensionProcess,
}

async fn start_probe(mode: &str) -> ProbeState {
    let root = tempfile::tempdir().unwrap();
    let extension_root = root.path().join("extensions");
    let directory = extension_root.join(PROBE_EXTENSION);
    std::fs::create_dir_all(&directory).unwrap();
    let script = directory.join("probe.py");
    std::fs::write(&script, SESSION_LIFECYCLE_PROBE).unwrap();
    let sdk_path = format!("{}/../../sdk/python", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        format!(
            r#"name = "{PROBE_EXTENSION}"
version = "0.1.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = [{script}, {mode}, {sdk}]
[contributes]
tools = ["probe"]
"#,
            script = serde_json::to_string(&script).unwrap(),
            mode = serde_json::to_string(mode).unwrap(),
            sdk = serde_json::to_string(&sdk_path).unwrap(),
        ),
    )
    .unwrap();
    let mut config =
        super::tests::executable_extension_config(root.path(), &extension_root, PROBE_EXTENSION);
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    let session = Session::create(root.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = SessionStore::new(&config.session_dir, root.path());
    let mut host = ExtensionHost::new();
    let extensions = ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );
    assert_eq!(
        extensions.processes.len(),
        1,
        "{}\n{}",
        extensions.status_summary(),
        extensions
            .diagnostics
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let process = extensions.processes[0].clone();
    ProbeState {
        _root: root,
        extensions,
        process,
    }
}

async fn probe(
    process: &ExtensionProcess,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    process
        .call_tool(
            "probe",
            json!({"method": method, "params": params}),
            process.current_context(),
        )
        .await
        .unwrap()
        .structured_content
        .unwrap()
}

async fn next_driver_request(
    extensions: &mut ExecutableExtensions,
) -> ExtensionSessionLifecycleRequest {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(request) = extensions.next_session_lifecycle_request() {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("active-session request reaches the product driver")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unnegotiated_session_lifecycle_reverse_requests_fail_closed() {
    let mut state = start_probe("omit").await;
    assert!(!state
        .process
        .negotiated_features()
        .contains("session_lifecycle"));
    state.extensions.activate_session_lifecycle_driver();
    let evidence = probe(
        &state.process,
        "session/switch",
        json!({"session_id": "session-b"}),
    )
    .await;
    assert_eq!(evidence["response"]["error"]["code"], -32601, "{evidence}");
    assert!(state.process.is_running());
    assert!(state.extensions.next_session_lifecycle_request().is_none());
    state.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_driver_settles_every_negotiated_session_operation() {
    let mut state = start_probe("select").await;
    assert!(state
        .process
        .negotiated_features()
        .contains("session_lifecycle"));

    // Startup leaves the driver inactive: a valid reverse request is refused
    // canonically and never reaches the product queue.
    let evidence = probe(&state.process, "session/create", json!({})).await;
    assert_eq!(evidence["response"]["error"]["code"], -32603, "{evidence}");
    assert_eq!(
        evidence["response"]["error"]["data"]["reason"],
        "active-session lifecycle service is unavailable"
    );
    assert!(state.extensions.next_session_lifecycle_request().is_none());

    state.extensions.activate_session_lifecycle_driver();
    for (method, params, expected, result) in [
        (
            "session/create",
            json!({}),
            ExtensionSessionLifecycleOperation::Create,
            "created-session",
        ),
        (
            "session/fork",
            json!({}),
            ExtensionSessionLifecycleOperation::Fork,
            "forked-session",
        ),
        (
            "session/reload",
            json!({}),
            ExtensionSessionLifecycleOperation::Reload,
            "reloaded-session",
        ),
        (
            "session/switch",
            json!({"session_id": "session-b"}),
            ExtensionSessionLifecycleOperation::Switch {
                session_id: "session-b".into(),
            },
            "session-b",
        ),
    ] {
        let (evidence, ()) = tokio::join!(probe(&state.process, method, params), async {
            let request = next_driver_request(&mut state.extensions).await;
            assert_eq!(request.operation(), &expected);
            assert!(!request.is_cancelled());
            request.respond(Ok(result.to_owned()));
        });
        assert_eq!(
            evidence["response"]["result"]["session_id"], result,
            "{evidence}"
        );
    }

    // Deactivation fences future work: the peer is refused again and no
    // unbound request is left for a replacement application binding.
    state.extensions.deactivate_session_lifecycle_driver();
    let evidence = probe(&state.process, "session/reload", json!({})).await;
    assert_eq!(evidence["response"]["error"]["code"], -32603, "{evidence}");
    assert!(state.extensions.next_session_lifecycle_request().is_none());
    assert!(state.process.is_running());

    // A driver failure is terminal for that request only; the peer survives.
    state.extensions.activate_session_lifecycle_driver();
    let (evidence, ()) = tokio::join!(probe(&state.process, "session/fork", json!({})), async {
        let request = next_driver_request(&mut state.extensions).await;
        request.respond(Err(ExtensionSessionLifecycleError::Failed));
    });
    assert_eq!(evidence["response"]["error"]["code"], -32603, "{evidence}");
    assert_eq!(
        evidence["response"]["error"]["data"]["reason"],
        "active-session lifecycle operation failed"
    );
    assert!(state.process.is_running());
    state.extensions.shutdown().await;
}
