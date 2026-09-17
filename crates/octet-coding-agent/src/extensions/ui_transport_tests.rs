//! Product-level API 0.2 UI observation transport.
//!
//! Editor snapshots, terminal resize observations and autocomplete chains are
//! retained API 0.2 surfaces. These tests drive the product boundary: only a
//! peer that negotiated the exact feature receives an observation, the editor
//! cursor stays host-local, an extension-initiated editor lease is applied by
//! the interactive shell, and autocomplete requires an interactive boundary
//! to admit the registration before any host-mediated query.
#![cfg(unix)]
use super::*;
use serde_json::json;

const PROBE_EXTENSION: &str = "ui-probe";

/// Records every UI surface the host delivers and answers one autocomplete
/// query. It never initiates host work on its own.
const UI_TRANSPORT_PROBE: &str = r#"import json
import sys

mode = sys.argv[1]
state = {"editor": None, "resize": None, "ack": None, "complete": None,
         "leases": 0, "responses": {}}


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        features = ["request_cancellation", "content_parts"]
        if mode == "ui":
            features += ["editor_handoff", "terminal_input", "autocomplete"]
        send({"jsonrpc": "2.0", "id": request["id"], "result": {
            "api_version": "0.2",
            "tools": [{"name": "probe", "description": "UI transport probe",
                        "parameters": {"type": "object"}}],
            "protocol": {"version": "0.2", "features": features,
                          "limits": {"max_concurrent_requests": 1}}}})
        if mode == "ui":
            send({"jsonrpc": "2.0", "id": "register-1",
                  "method": "ui/autocomplete/register", "params": {"revision": 1}})
    elif method == "ui/editor-state":
        state["editor"] = request["params"]
    elif method == "ui/resize":
        state["resize"] = request["params"]
    elif method == "ui/autocomplete/complete":
        state["complete"] = request["params"]
        send({"jsonrpc": "2.0", "id": request["id"], "result": {
            "prefix": "fi",
            "items": [{"value": "file.rs", "label": "file.rs", "description": "source"}]}})
    elif method == "tool/call":
        arguments = request["params"]["arguments"]
        if "operation" in arguments:
            # One extension-initiated editor lease. The tool call is answered
            # immediately; the host response is recorded when it arrives.
            state["leases"] += 1
            lease = "editor-" + str(state["leases"])
            params = {"operation": arguments["operation"]}
            if "text" in arguments:
                params["text"] = arguments["text"]
            send({"jsonrpc": "2.0", "id": lease, "method": "ui/editor", "params": params})
            send({"jsonrpc": "2.0", "id": request["id"], "result": {
                "content": [{"type": "text", "text": json.dumps({"sent": lease})}],
                "is_error": False, "metadata": {}}})
        else:
            send({"jsonrpc": "2.0", "id": request["id"], "result": {
                "content": [{"type": "text", "text": json.dumps(state)}],
                "is_error": False, "metadata": {}}})
    elif isinstance(request.get("id"), str) and request["id"].startswith("editor-"):
        state["responses"][request["id"]] = request
    elif request.get("id") == "register-1":
        state["ack"] = request
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": request["id"], "result": {}})
        break
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
    std::fs::write(&script, UI_TRANSPORT_PROBE).unwrap();
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        format!(
            r#"name = "{PROBE_EXTENSION}"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "python3"
args = [{script}, {mode}]
[contributes]
tools = ["probe"]
"#,
            script = serde_json::to_string(&script).unwrap(),
            mode = serde_json::to_string(mode).unwrap(),
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

fn snapshot(text: &str, cursor: usize, revision: u64) -> ShellEditorSnapshot {
    ShellEditorSnapshot {
        text: text.to_owned(),
        cursor,
        revision,
        focused: true,
    }
}

/// Returns the fixture's recorded observations as one JSON document.
async fn recorded(process: &ExtensionProcess) -> serde_json::Value {
    let output = process
        .call_tool("probe", json!({}), process.current_context())
        .await
        .unwrap();
    serde_json::from_str(&output.content).unwrap()
}

/// Sends one probe tool call with arguments and returns its JSON body.
async fn probe(process: &ExtensionProcess, arguments: serde_json::Value) -> serde_json::Value {
    let output = process
        .call_tool("probe", arguments, process.current_context())
        .await
        .unwrap();
    serde_json::from_str(&output.content).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negotiated_ui_surfaces_deliver_editor_resize_and_autocomplete() {
    let mut state = start_probe("ui").await;
    let features = state.process.negotiated_features();
    for feature in ["editor_handoff", "terminal_input", "autocomplete"] {
        assert!(features.contains(feature), "{features:?}");
    }
    state
        .extensions
        .sync_editor_state(snapshot("draft text", 5, 4));
    state.extensions.observe_terminal_resize(120, 40);

    // The registration arrives right after initialization. Only the
    // interactive boundary admits it, and the host answers it explicitly.
    let mut shell = InteractiveShell::test_shell();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            state.extensions.drain_events_for_shell(&mut shell);
            if !state.extensions.autocomplete_registrations.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("autocomplete registration admitted");
    assert!(state
        .extensions
        .request_editor_autocomplete(snapshot("@fi", 3, 7)));
    let update = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = state.extensions.drain_background_updates();
            if let Some(update) = updates.autocomplete.into_iter().next() {
                return update;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("autocomplete update");
    assert_eq!(update.snapshot.revision, 7);
    assert_eq!(update.snapshot.cursor, 3);
    assert_eq!(update.prefix, "fi");
    assert_eq!(update.items.len(), 1);
    assert_eq!(update.items[0].value, "file.rs");
    assert_eq!(update.items[0].label, "file.rs");
    assert_eq!(update.items[0].description.as_deref(), Some("source"));

    let recorded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = recorded(&state.process).await;
            if !value["ack"].is_null() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("autocomplete registration acknowledgement");
    assert_eq!(
        recorded["editor"],
        json!({"text": "draft text", "revision": 4, "focused": true})
    );
    assert!(
        recorded["editor"].get("cursor").is_none(),
        "the host-owned editor cursor must never leave the frontend: {recorded}"
    );
    assert_eq!(recorded["resize"], json!({"columns": 120, "rows": 40}));
    assert_eq!(
        recorded["complete"],
        json!({"text": "@fi", "cursor": 3, "revision": 7})
    );
    assert_eq!(recorded["ack"]["result"], json!({"accepted": true}));
    state.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unnegotiated_ui_features_receive_nothing_and_own_no_autocomplete_chain() {
    let mut state = start_probe("none").await;
    let features = state.process.negotiated_features();
    assert!(!features.contains("editor_handoff"), "{features:?}");
    assert!(!features.contains("terminal_input"), "{features:?}");
    assert!(!features.contains("autocomplete"), "{features:?}");
    state
        .extensions
        .sync_editor_state(snapshot("draft text", 5, 4));
    state.extensions.observe_terminal_resize(80, 24);
    assert!(!state
        .extensions
        .request_editor_autocomplete(snapshot("@fi", 3, 7)));

    let recorded = recorded(&state.process).await;
    assert!(recorded["editor"].is_null(), "{recorded}");
    assert!(recorded["resize"].is_null(), "{recorded}");
    assert!(recorded["complete"].is_null(), "{recorded}");
    assert!(recorded["ack"].is_null(), "{recorded}");
    assert!(state.process.is_running());
    state.extensions.shutdown().await;
}

/// The extension-initiated half of editor handoff: a peer that negotiated
/// `editor_handoff` can ask the real interactive shell to apply an operation,
/// and receives only the shared snapshot — never the host-local cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negotiated_editor_lease_applies_host_operations_without_exposing_the_cursor() {
    let mut state = start_probe("ui").await;
    assert!(state
        .process
        .negotiated_features()
        .contains("editor_handoff"));
    let mut shell = InteractiveShell::test_shell();
    let seeded = shell.extension_set_editor("seed".to_owned());
    assert!(seeded.focused, "the host editor must own the input surface");

    let sent = probe(
        &state.process,
        json!({"operation": "set", "text": "handoff text"}),
    )
    .await;
    assert_eq!(sent["sent"], "editor-1");

    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            state.extensions.drain_events_for_shell(&mut shell);
            let value = recorded(&state.process).await;
            if !value["responses"].as_object().unwrap().is_empty() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("host editor lease response");

    let response = &observed["responses"]["editor-1"];
    assert_eq!(response["result"]["text"], "handoff text");
    assert!(
        response["result"].get("cursor").is_none(),
        "the host-owned editor cursor must never leave the frontend: {response}"
    );
    // The host actually moved its own editor, not merely answered.
    let applied = shell.extension_editor_snapshot();
    assert_eq!(applied.text, "handoff text");
    assert_eq!(response["result"]["revision"], applied.revision);
    assert_eq!(response["result"]["focused"], applied.focused);

    // A later read is served from that same host snapshot, still without the
    // cursor, and the peer gains no second input owner.
    let sent = probe(&state.process, json!({"operation": "get"})).await;
    assert_eq!(sent["sent"], "editor-2");
    let observed_read = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            state.extensions.drain_events_for_shell(&mut shell);
            let value = recorded(&state.process).await;
            if !value["responses"]["editor-2"].is_null() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("host editor read response");
    let read = &observed_read["responses"]["editor-2"];
    assert_eq!(read["result"]["text"], "handoff text");
    assert!(read["result"].get("cursor").is_none(), "{read}");
    assert_eq!(
        read["result"]["revision"],
        shell.extension_editor_snapshot().revision
    );
    assert!(state.process.is_running());
    state.extensions.shutdown().await;
}
