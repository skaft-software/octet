//! Real-process regressions for terminal cleanup; no model calls or user state.
use super::*;
use std::os::unix::fs::PermissionsExt as _;

const PROBE: &str = r#"#!/usr/bin/env python3
import json, os, sys

mode, log_path = sys.argv[1:]

def record(value):
    with open(log_path, "a") as log:
        log.write(json.dumps(value, separators=(",", ":")) + "\n")

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)

def reply(request_id, result):
    send({"jsonrpc":"2.0", "id":request_id, "result":result})

record({"launched":os.getpid()})
end_id = None
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    if method == "initialize":
        protocol = params["protocol"]
        assert "remote_ui" in protocol["optional_features"]
        if mode == "retry-probe" and not (os.path.exists(log_path + ".ready") and os.path.exists(log_path + ".retry-ok")):
            send({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32000,"message":"fixture not ready"}})
            continue
        reply(request["id"], {"api_version":"0.4", "tools":[
            {"name":"probe", "description":"probe", "parameters":{"type":"object"}}],
            "commands":[], "protocol":{"version":"0.4",
                "features":protocol["required_features"] + ["remote_ui"],
                "limits":{"max_concurrent_requests":2}}})
    elif method == "tool/call":
        reply(request["id"], {"content":[{"type":"text", "text":"callable"}],
            "is_error":False, "metadata":{}})
    elif method == "hook/run":
        record({"hook":params["hook"], "payload":params["payload"]})
        if params["hook"] == "session_start":
            reply(request["id"], {})
        else:
            end_id = request["id"]
            send({"jsonrpc":"2.0", "method":"notification", "params":{"message":"end-entered"}})
            if mode == "ui-cleanup":
                send({"jsonrpc":"2.0", "id":"end-cleanup", "method":"ui/chrome", "params":{
                    "parent_request_id":end_id, "resource_owner":params["context"]["resource_owner"],
                    "chrome":{"kind":"working_message", "message":None}}})
    elif request.get("id") == "end-cleanup" and ("result" in request or "error" in request):
        record({"end_ui_response":request})
        reply(end_id, {})
        end_id = None
    elif method == "$/cancelRequest":
        record({"cancelled":params["id"]})
        if params["id"] == end_id:
            send({"jsonrpc":"2.0", "id":end_id, "error":{"code":-32800,"message":"cancelled"}})
            end_id = None
    elif method == "shutdown":
        record({"shutdown":True})
        reply(request["id"], {})
        break
"#;

struct Fixture {
    _root: tempfile::TempDir,
    extensions: ExecutableExtensions,
    process: ExtensionProcess,
    manager: ExtensionRuntimeManager,
    log: PathBuf,
}

async fn fixture(mode: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("lifecycle-exit-probe");
    std::fs::create_dir(&directory).unwrap();
    let script = directory.join("probe.py");
    std::fs::write(&script, PROBE).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let log = root.path().join("wire.jsonl");
    if mode == "retry-probe" {
        // `.ready` models the published private runtime; `.retry-ok` models a
        // failure of the first activation attempt after setup succeeded.
        std::fs::write(format!("{}.ready", log.display()), b"ready").unwrap();
        std::fs::write(format!("{}.retry-ok", log.display()), b"ready").unwrap();
    }
    let manifest_text = format!(
        r#"name = "lifecycle-exit-probe"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "probe.py"
args = [{mode}, {log}]
[contributes]
tools = ["probe"]
hooks = ["session_start", "session_end"]
notifications = true
[runtime]
lifecycle = "pi_aggregate"
sharing = "workspace"
"#,
        mode = serde_json::to_string(mode).unwrap(),
        log = serde_json::to_string(&log).unwrap(),
    );
    let manifest_path = directory.join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(&manifest_path, &manifest_text).unwrap();
    let descriptor = DiscoveredExtension {
        manifest: ExtensionManifest::parse(&manifest_text).unwrap(),
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: octet_agent::extension_process::ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(root.path()).unwrap());
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([descriptor]))
        .await;
    let owner = "session-exit-probe";
    let binding = manager.bind_session(owner).unwrap();
    let wake = Arc::new(tokio::sync::Notify::new());
    let mut runtime = ExtensionRuntimeConfig::new(root.path());
    runtime.request_timeout = Duration::from_secs(2);
    runtime.shutdown_timeout = Duration::from_millis(100);
    runtime.remote_ui = Some(wake.clone());
    let process = binding
        .activate("lifecycle-exit-probe", runtime)
        .await
        .unwrap()
        .process()
        .clone();
    let mut host_state = process.current_context().host;
    host_state.has_ui = Some(true);
    host_state.mode = Some("tui".into());
    process.set_host_state(host_state);
    process.start_session_hook_binding(owner).await.unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    extensions.runtime_manager = Some(manager.clone());
    extensions.runtime_binding = Some(binding);
    extensions.resource_owner = Some(owner.into());
    extensions.remote_ui_wake = Some(wake);
    extensions.session_lifecycle_started = true;
    Fixture {
        _root: root,
        extensions,
        process,
        manager,
        log,
    }
}

fn records(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Models a Python extension that failed its cold start: the runtime catalogue
/// and session binding exist, but no process is live in the host.
async fn activation_fixture() -> Fixture {
    let mut f = fixture("retry-probe").await;
    f.extensions.processes.clear();
    f.extensions.workspace = f._root.path().to_path_buf();
    f.extensions.tool_host = Some(ExtensionHost::new());
    f.extensions.session_id = Some("session-exit-probe".to_owned());
    f.extensions.resource_owner = Some("session-exit-probe".to_owned());
    f
}

fn local_python_runtime(f: &Fixture) -> octet_agent::extension_process::PythonRuntimeConfig {
    octet_agent::extension_process::PythonRuntimeConfig {
        root: f._root.path().join("local-python-fixture"),
        setup: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_runtime_setup_activates_extension_and_registers_its_tool() {
    let mut f = activation_fixture().await;
    let python_runtime = local_python_runtime(&f);
    // Cold start: the local fixture runtime has not been provisioned yet.
    std::fs::remove_file(format!("{}.ready", f.log.display())).unwrap();
    assert!(f
        .extensions
        .activate_python_extension_with_runtime("lifecycle-exit-probe", python_runtime.clone())
        .await
        .is_err());
    // `/extensions setup <name>` published the verified runtime. Activation in
    // the same session must register the tool instead of requiring a restart.
    std::fs::write(format!("{}.ready", f.log.display()), b"ready").unwrap();
    f.extensions
        .activate_python_extension_with_runtime("lifecycle-exit-probe", python_runtime)
        .await
        .unwrap();

    assert!(f
        .extensions
        .tool_host
        .as_ref()
        .unwrap()
        .tool_definitions()
        .iter()
        .any(|definition| definition.name == "probe"));
    let process = f.extensions.processes.last().unwrap().clone();
    assert!(process.is_running());
    let result = process
        .call_tool("probe", serde_json::json!({}), process.current_context())
        .await
        .unwrap();
    assert!(result.content.contains("callable"));
    process.shutdown().await;
    f.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_failure_after_setup_can_be_retried_in_the_current_host() {
    let mut f = activation_fixture().await;
    let python_runtime = local_python_runtime(&f);
    // Runtime setup is published, but the first activation attempt fails; the
    // caller must receive the error and be able to retry without a restart.
    std::fs::remove_file(format!("{}.retry-ok", f.log.display())).unwrap();
    let error = f
        .extensions
        .activate_python_extension_with_runtime("lifecycle-exit-probe", python_runtime.clone())
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());
    assert!(f.extensions.processes.is_empty());
    std::fs::write(format!("{}.retry-ok", f.log.display()), b"ready").unwrap();
    f.extensions
        .activate_python_extension_with_runtime("lifecycle-exit-probe", python_runtime)
        .await
        .unwrap();

    let process = f.extensions.processes.last().unwrap().clone();
    assert!(process.is_running());
    let result = process
        .call_tool("probe", serde_json::json!({}), process.current_context())
        .await
        .unwrap();
    assert!(result.content.contains("callable"));
    // Setup for an already-live extension is a no-op, not a second launch.
    f.extensions
        .activate_python_extension_with_runtime("lifecycle-exit-probe", local_python_runtime(&f))
        .await
        .unwrap();
    assert_eq!(f.extensions.processes.len(), 1);
    process.shutdown().await;
    f.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_transmits_explicit_host_transition_reason() {
    let mut f = fixture("ui-cleanup").await;
    for reason in ["new", "resume", "fork"] {
        f.process
            .start_session_hook_binding_with_reason(format!("owner-{reason}"), reason)
            .await
            .unwrap();
    }
    let reasons = records(&f.log)
        .into_iter()
        .filter(|entry| entry["hook"] == "session_start")
        .map(|entry| entry["payload"]["reason"].clone())
        .collect::<Vec<_>>();
    assert_eq!(reasons, vec!["startup", "new", "resume", "fork"]);
    f.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_fleet_admission_before_awaiting_session_end() {
    let mut f = fixture("hold-end").await;
    let mut events = f.process.subscribe();
    let shutdown = f.extensions.shutdown();
    tokio::pin!(shutdown);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! {
                biased;
                event = events.recv() => {
                    if let ExtensionEvent::Notification { notification } = event.unwrap() {
                        if notification.message == "end-entered" { break; }
                    }
                }
                () = &mut shutdown => panic!("shutdown finished before the held end callback"),
            }
        }
    })
    .await
    .unwrap();
    // The same manager fence rejects foreground activation and stops its crash
    // monitor. It must be installed before a callback can time out or exit.
    let admission = f.manager.bind_session("must-not-start-during-shutdown");
    tokio::time::timeout(Duration::from_secs(3), &mut shutdown)
        .await
        .unwrap();
    assert!(matches!(
        admission,
        Err(octet_agent::extension_runtime::ExtensionRuntimeManagerError::ManagerClosed)
    ));
    assert_eq!(
        records(&f.log)
            .iter()
            .filter(|entry| entry.get("launched").is_some())
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_end_reverse_ui_receives_a_terminal_reply_before_shutdown() {
    let mut f = fixture("ui-cleanup").await;
    tokio::time::timeout(Duration::from_secs(3), f.extensions.shutdown())
        .await
        .unwrap();
    let wire = records(&f.log);
    let reply = wire
        .iter()
        .position(|entry| entry.get("end_ui_response").is_some());
    let shutdown = wire
        .iter()
        .position(|entry| entry.get("shutdown").is_some());
    assert!(
        reply
            .zip(shutdown)
            .is_some_and(|(reply, shutdown)| reply < shutdown),
        "UI cleanup must receive success or a truthful typed refusal, not be left awaiting an unpumped frontend: {wire:?}",
    );
    let response = &wire[reply.unwrap()]["end_ui_response"];
    assert_eq!(
        response["error"]["code"],
        ExtensionRequestFailure::NotForegroundOwner.code()
    );
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not applied"));
    assert!(!wire.iter().any(|entry| entry.get("cancelled").is_some()));
}
