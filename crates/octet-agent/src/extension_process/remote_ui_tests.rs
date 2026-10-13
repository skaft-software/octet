//! Real-process remote UI lifecycle regressions.

use super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use tempfile::TempDir;

#[cfg(unix)]
const REMOTE_UI_PROCESS_SCRIPT: &str = r#"#!/usr/bin/env python3
import json, os, sys

def receive():
    return json.loads(sys.stdin.readline())
def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)
def notice(message):
    send({"jsonrpc":"2.0","method":"notification","params":{"message":message}})
def snapshot(revision, columns=80, rows=24):
    send({"jsonrpc":"2.0","method":"ui/frame","params":{
        "resource_owner":owner,"surface_id":surface,"revision":revision,
        "columns":columns,"rows":rows,"lines":["\x1b[38;2;255;128;0mframe " + str(revision) + "\x1b[0m"]}})
init = receive()
protocol = init["params"]["protocol"]
assert init["params"]["api_version"] == "0.4"
assert "remote_ui" in protocol["optional_features"]
send({"jsonrpc":"2.0","id":init["id"],"result":{
    "api_version":"0.4","tools":[],"commands":[{"name":name,"description":name} for name in ["open","close","cancel","crash"]],
    "protocol":{"version":"0.4","features":protocol["required_features"] + ["remote_ui"],"limits":{"max_concurrent_requests":4}}}})
owner = None
parent = None
surface = "demo"
serial = 0
cancel_id = None
while True:
    message = receive()
    method = message.get("method")
    params = message.get("params", {})
    if method == "command/execute":
        name = params["name"]
        if name == "crash":
            os._exit(0)
        if name in ("open", "cancel"):
            owner = params["context"]["resource_owner"]
            parent = message["id"]
            surface = "cancelled" if name == "cancel" else "demo"
            serial += 1
            child_id = "open-" + str(serial)
            send({"jsonrpc":"2.0","id":child_id,"method":"ui/open","params":{
                "parent_request_id":parent,"surface_id":surface,"title":"Demo","mouse_capture":True}})
            answer = receive()
            assert answer["id"] == child_id and answer["result"] == {"columns":80,"rows":24}, answer
            for revision in range(64):
                snapshot(revision)
            notice("cancel-ready" if name == "cancel" else "frames-ready")
            if name == "cancel":
                cancel_id = message["id"]
            else:
                send({"jsonrpc":"2.0","id":message["id"],"result":{"text":"opened"}})
        elif name == "close":
            serial += 1
            child_id = "close-" + str(serial)
            # Deliberately echo the original, normally-settled command parent.
            send({"jsonrpc":"2.0","id":child_id,"method":"ui/close","params":{
                "parent_request_id":parent,"resource_owner":owner,"surface_id":surface}})
            answer = receive()
            assert answer["id"] == child_id and answer["result"] == {}, answer
            send({"jsonrpc":"2.0","id":message["id"],"result":{"text":"closed"}})
    elif method == "ui/key":
        assert params == {"surface_id":"demo","key":"Enter","kind":"press","modifiers":[]}, params
        snapshot(64)
        notice("key-ready")
    elif method == "ui/mouse":
        assert params["surface_id"] == "demo" and params["kind"] == "press" and params["button"] == "left"
        assert params["x"] == 79 and params["y"] == 23 and params["wheel_delta"] == 0
        snapshot(65)
        notice("mouse-ready")
    elif method == "ui/resize":
        assert params == {"surface_id":"demo","columns":70,"rows":20}, params
        snapshot(66)  # stale geometry must not become the current view
        snapshot(67, 70, 20)
        notice("resize-ready")
    elif method == "context/updated":
        assert params["resource_owner"] == owner and params["host"]["model"] == "updated-model", params
        notice("context-ready")
    elif method == "ui/closed":
        assert params["surface_id"] == "demo" and params["reason"] in ["host dismissed", "foreground owner replaced"], params
        snapshot(68, 70, 20)  # closed surface cannot be resurrected
        notice("closed-ready")
    elif method == "$/cancelRequest":
        assert params["id"] == cancel_id, params
        send({"jsonrpc":"2.0","id":cancel_id,"error":{"code":-32800,"message":"cancelled"}})
        notice("cancel-ack")
    elif method == "shutdown":
        send({"jsonrpc":"2.0","id":message["id"],"result":{}})
        break
    else:
        raise AssertionError(message)
"#;

#[cfg(unix)]
async fn start_remote_ui_test_process() -> (
    TempDir,
    ExtensionProcess,
    broadcast::Receiver<ExtensionEvent>,
    Arc<Notify>,
) {
    let temp = TempDir::new().unwrap();
    write_executable_script(&temp.path().join("extension.py"), REMOTE_UI_PROCESS_SCRIPT);
    let manifest = ExtensionManifest::parse(
        r#"name = "remote-ui-process"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
commands = ["open", "close", "cancel", "crash"]
notifications = true
"#,
    )
    .unwrap();
    let wake = Arc::new(Notify::new());
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.remote_ui = Some(Arc::clone(&wake));
    config.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    let events = process.subscribe();
    (temp, process, events, wake)
}

#[cfg(unix)]
async fn remote_ui_next_operation(
    events: &mut broadcast::Receiver<ExtensionEvent>,
) -> (
    ExtensionRequestId,
    u64,
    ExtensionResourceOwner,
    ExtensionRemoteUiOperation,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::RemoteUiRequested {
                request_id,
                generation,
                owner,
                operation,
            } = events.recv().await.unwrap()
            {
                return (request_id, generation, owner, operation);
            }
        }
    })
    .await
    .unwrap()
}

#[cfg(unix)]
async fn remote_ui_wait_notice(events: &mut broadcast::Receiver<ExtensionEvent>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::Notification { notification } = events.recv().await.unwrap() {
                if notification.message == expected {
                    return;
                }
            }
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
async fn remote_ui_open_test_surface(
    process: &ExtensionProcess,
    events: &mut broadcast::Receiver<ExtensionEvent>,
    command: &str,
) -> (
    ExtensionResourceOwner,
    tokio::task::JoinHandle<Result<CommandOutput, ExtensionRuntimeError>>,
) {
    let context = process.current_context_for_resource_owner("session-owner");
    let call = tokio::spawn({
        let process = process.clone();
        let command = command.to_owned();
        async move { process.execute_command(command, vec![], context).await }
    });
    let (request_id, generation, owner, operation) = remote_ui_next_operation(events).await;
    assert!(matches!(
        operation,
        ExtensionRemoteUiOperation::Open {
            mouse_capture: true,
            ..
        }
    ));
    // A malformed frontend success must not silently admit invalid geometry.
    assert!(process
        .respond_to_extension_request(
            request_id.clone(),
            generation,
            ExtensionRequestOutcome::Ok(serde_json::json!({"columns":0,"rows":24}))
        )
        .await
        .is_err());
    process
        .respond_to_extension_request(
            request_id,
            generation,
            ExtensionRequestOutcome::Ok(serde_json::json!({"columns":80,"rows":24})),
        )
        .await
        .unwrap();
    remote_ui_wait_notice(
        events,
        if command == "cancel" {
            "cancel-ready"
        } else {
            "frames-ready"
        },
    )
    .await;
    (owner, call)
}

#[cfg(unix)]
#[tokio::test]
async fn remote_ui_real_process_caches_frames_and_delivers_notifications() {
    let (_temp, process, mut events, wake) = start_remote_ui_test_process().await;
    let (owner, call) = remote_ui_open_test_surface(&process, &mut events, "open").await;
    assert_eq!(call.await.unwrap().unwrap().text, "opened");
    let cached = process.take_remote_ui_frames();
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[0].revision, 63);
    assert_eq!(cached[0].resource_owner, owner);
    assert!(process.remote_ui_surface_is_current(&owner, "demo"));
    tokio::time::timeout(Duration::from_millis(100), wake.notified())
        .await
        .unwrap();
    let key = ExtensionRemoteUiKey {
        surface_id: "demo".into(),
        key: "Enter".into(),
        kind: crate::ExtensionRemoteUiKeyKind::Press,
        modifiers: vec![],
        editor_input: None,
    };
    process.notify_remote_ui_key(key).unwrap();
    remote_ui_wait_notice(&mut events, "key-ready").await;
    assert_eq!(process.take_remote_ui_frames()[0].revision, 64);
    let mut mouse = ExtensionRemoteUiMouse {
        surface_id: "demo".into(),
        kind: crate::ExtensionRemoteUiMouseKind::Press,
        button: crate::ExtensionRemoteUiMouseButton::Left,
        x: 80,
        y: 23,
        modifiers: vec![],
        wheel_delta: 0,
    };
    assert!(process.notify_remote_ui_mouse(mouse.clone()).is_err());
    mouse.x = 79;
    process.notify_remote_ui_mouse(mouse).unwrap();
    remote_ui_wait_notice(&mut events, "mouse-ready").await;
    assert_eq!(process.take_remote_ui_frames()[0].revision, 65);
    process
        .notify_remote_ui_resize(ExtensionRemoteUiResize {
            surface_id: "demo".into(),
            columns: 70,
            rows: 20,
        })
        .unwrap();
    remote_ui_wait_notice(&mut events, "resize-ready").await;
    let resized = process.take_remote_ui_frames();
    assert_eq!(resized[0].revision, 67);
    assert_eq!((resized[0].columns, resized[0].rows), (70, 20));
    let mut state = process.current_context().host;
    state.model = Some("updated-model".into());
    process.set_host_state(state);
    remote_ui_wait_notice(&mut events, "context-ready").await;
    process
        .notify_remote_ui_closed(ExtensionRemoteUiClosed {
            surface_id: "demo".into(),
            reason: "host dismissed".into(),
        })
        .unwrap();
    remote_ui_wait_notice(&mut events, "closed-ready").await;
    assert!(process.take_remote_ui_frames().is_empty());
    assert!(!process.remote_ui_surface_is_current(&owner, "demo"));
    let (_, reopened) = remote_ui_open_test_surface(&process, &mut events, "open").await;
    reopened.await.unwrap().unwrap();
    let close = tokio::spawn({
        let process = process.clone();
        async move {
            process
                .execute_command(
                    "close",
                    vec![],
                    process.current_context_for_resource_owner("session-owner"),
                )
                .await
        }
    });
    let (id, generation, _, operation) = remote_ui_next_operation(&mut events).await;
    assert!(matches!(
        operation,
        ExtensionRemoteUiOperation::Close { .. }
    ));
    process
        .respond_to_extension_request(
            id,
            generation,
            ExtensionRequestOutcome::Ok(serde_json::json!({})),
        )
        .await
        .unwrap();
    close.await.unwrap().unwrap();
    assert!(!process.remote_ui_surface_is_current(&owner, "demo"));
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn remote_ui_real_process_cancellation_reload_and_crash_clear_state() {
    let (_temp, process, mut events, _wake) = start_remote_ui_test_process().await;
    let (old_owner, opened) = remote_ui_open_test_surface(&process, &mut events, "open").await;
    opened.await.unwrap().unwrap();
    let report = process.reload().await.unwrap();
    assert_eq!(report.generation, 2);
    assert!(!process.remote_ui_surface_is_current(&old_owner, "demo"));
    assert!(process.take_remote_ui_frames().is_empty());
    let (cancel_owner, pending) =
        remote_ui_open_test_surface(&process, &mut events, "cancel").await;
    pending.abort();
    let _ = pending.await;
    remote_ui_wait_notice(&mut events, "cancel-ack").await;
    assert!(!process.remote_ui_surface_is_current(&cancel_owner, "cancelled"));
    assert!(process.take_remote_ui_frames().is_empty());
    // The cancelled request retires its own surface; the extension keeps its
    // issued session owner, so a later mount does not need a reload.
    assert!(
        lock_std_mutex(&read_std_lock(&process.inner.connection).issued_resource_owners)
            .contains(&cancel_owner)
    );
    let (owner, opened) = remote_ui_open_test_surface(&process, &mut events, "open").await;
    opened.await.unwrap().unwrap();
    assert!(process
        .execute_command(
            "crash",
            vec![],
            process.current_context_for_resource_owner("session-owner")
        )
        .await
        .is_err());
    assert!(!process.remote_ui_surface_is_current(&owner, "demo"));
    assert!(process.take_remote_ui_frames().is_empty());
    assert!(!process.is_running());
    assert!(!process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn remote_ui_real_process_owner_replacement_revokes_retained_context() {
    let (_temp, process, mut events, _wake) = start_remote_ui_test_process().await;
    let (owner, opened) = remote_ui_open_test_surface(&process, &mut events, "open").await;
    opened.await.unwrap().unwrap();
    let mut state = process.current_context().host;
    state.session_id = Some("replacement-session".into());
    process.set_host_state(state);
    remote_ui_wait_notice(&mut events, "closed-ready").await;
    assert!(!process.remote_ui_surface_is_current(&owner, "demo"));
    assert!(process.take_remote_ui_frames().is_empty());
    assert!(
        !lock_std_mutex(&read_std_lock(&process.inner.connection).issued_resource_owners)
            .contains(&owner)
    );
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn remote_ui_api_v04_session_hooks_issue_owner_and_legacy_context() {
    for hooks in [
        r#"["session_start"]"#,
        r#"["session_end"]"#,
        r#"["session_start", "session_end"]"#,
    ] {
        let temp = TempDir::new().unwrap();
        write_executable_script(
            &temp.path().join("extension.py"),
            r#"#!/usr/bin/env python3
import json, sys
def receive(): return json.loads(sys.stdin.readline())
def send(value): print(json.dumps(value, separators=(",", ":")), flush=True)
init = receive()
protocol = init["params"]["protocol"]
send({"jsonrpc":"2.0","id":init["id"],"result":{"api_version":"0.4","tools":[],"commands":[],
    "protocol":{"version":"0.4","features":protocol["required_features"]+["remote_ui"],"limits":{"max_concurrent_requests":1}}}})
while True:
    request = receive()
    if request["method"] == "shutdown":
        send({"jsonrpc":"2.0","id":request["id"],"result":{}})
        break
    params = request["params"]
    assert request["method"] == "hook/run"
    assert params["context"]["workspace"] == init["params"]["workspace"]
    assert params["context"]["host"]["model"] == "initial-model"
    assert params["context"]["resource_owner"] == params["payload"]["binding"]
    if params["hook"] == "session_start":
        send({"jsonrpc":"2.0","id":"footer","method":"ui/open","params":{
            "parent_request_id":request["id"],"surface_id":"footer","title":"Footer","placement":"footer"}})
        answer = receive()
        assert answer["id"] == "footer" and answer["result"] == {"columns":80,"rows":1}, answer
        send({"jsonrpc":"2.0","method":"ui/frame","params":{
            "resource_owner":params["payload"]["binding"],"surface_id":"footer","revision":0,"columns":80,"rows":1,"lines":["footer"]}})
    else:
        assert params["hook"] == "session_end"
    send({"jsonrpc":"2.0","id":request["id"],"result":{"disposition":{"action":"continue"}}})
"#,
        );
        let manifest = ExtensionManifest::parse(&format!(
            r#"name = "remote-ui-hooks"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
hooks = {hooks}
"#
        ))
        .unwrap();
        let mut config = ExtensionRuntimeConfig::new(temp.path());
        config.remote_ui = Some(Arc::new(Notify::new()));
        config.supervise = false;
        config.host_state.model = Some("initial-model".into());
        let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
            .await
            .unwrap();
        assert!(process.declares_session_hooks());
        if !hooks.contains("session_start") {
            process
                .start_session_hook_binding("session-owner")
                .await
                .unwrap();
            process
                .settle_session_hook_binding("session-owner", ExtensionLifecycleOutcome::Completed)
                .await
                .unwrap();
            assert!(process.shutdown().await);
            continue;
        }
        let mut events = process.subscribe();
        let start = tokio::spawn({
            let process = process.clone();
            async move { process.start_session_hook_binding("session-owner").await }
        });
        let (id, generation, owner, operation) = remote_ui_next_operation(&mut events).await;
        assert!(matches!(
            operation,
            ExtensionRemoteUiOperation::Open {
                placement: crate::ExtensionRemoteUiPlacement::Footer,
                ..
            }
        ));
        process
            .respond_to_extension_request(
                id,
                generation,
                ExtensionRequestOutcome::Ok(serde_json::json!({"columns":80,"rows":1})),
            )
            .await
            .unwrap();
        start.await.unwrap().unwrap();
        assert_eq!(process.take_remote_ui_frames()[0].lines, vec!["footer"]);
        assert!(process.remote_ui_surface_is_current(&owner, "footer"));
        process
            .settle_session_hook_binding("session-owner", ExtensionLifecycleOutcome::Completed)
            .await
            .unwrap();
        assert!(!process.remote_ui_surface_is_current(&owner, "footer"));
        assert!(
            !lock_std_mutex(&read_std_lock(&process.inner.connection).issued_resource_owners)
                .contains(&owner)
        );
        assert!(process.shutdown().await);
    }
}

#[test]
fn remote_editor_checkpoint_wire_fields_and_bounds_are_strict() {
    let wire = serde_json::json!({
        "surface_id": "editor", "mount_id": "remote.1",
        "input_revision": 0, "checkpoint_revision": 1,
    });
    let mut checkpoint: ExtensionEditorCheckpoint = serde_json::from_value(wire.clone()).unwrap();
    checkpoint.validate().unwrap();
    let max = crate::extension_remote_ui::MAX_EXTENSION_REMOTE_UI_REVISION;
    checkpoint.input_revision = max;
    checkpoint.checkpoint_revision = max;
    checkpoint.validate().unwrap();
    for (field, value) in [
        ("checkpoint_revision", serde_json::json!(0)),
        ("checkpoint_revision", serde_json::json!(max + 1)),
        ("input_revision", serde_json::json!(max + 1)),
        ("surface_id", serde_json::json!("")),
        ("mount_id", serde_json::json!("x".repeat(65))),
        ("mount_id", serde_json::json!("bad/id")),
    ] {
        let mut invalid = wire.clone();
        invalid[field] = value;
        assert!(serde_json::from_value::<ExtensionEditorCheckpoint>(invalid)
            .unwrap()
            .validate()
            .is_err());
    }
    for field in [
        "surface_id",
        "mount_id",
        "input_revision",
        "checkpoint_revision",
    ] {
        let mut missing = wire.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<ExtensionEditorCheckpoint>(missing).is_err());
    }
    for value in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("1"),
        serde_json::Value::Null,
    ] {
        let mut invalid = wire.clone();
        invalid["input_revision"] = value;
        assert!(serde_json::from_value::<ExtensionEditorCheckpoint>(invalid).is_err());
    }
    let mut extra = wire;
    extra["resource_owner"] = serde_json::json!({});
    assert!(serde_json::from_value::<ExtensionEditorCheckpoint>(extra).is_err());
    let plain: ComposerTextRequest =
        serde_json::from_value(serde_json::json!({"parent_request_id":1,"text":"plain"})).unwrap();
    assert!(plain.editor_checkpoint.is_none());
    assert!(serde_json::to_value(plain)
        .unwrap()
        .get("editor_checkpoint")
        .is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn remote_editor_checkpoint_process_gates_and_authoritative_owner() {
    for (api, remote) in [("0.4", true), ("0.4", false), ("0.2", false)] {
        let temp = TempDir::new().unwrap();
        let script = r#"#!/usr/bin/env python3
import json, sys
api, remote = '__API__', __REMOTE__
def receive(): return json.loads(sys.stdin.readline())
def send(value): print(json.dumps(dict(jsonrpc='2.0', **value)), flush=True)
init = receive()
features = init['params']['protocol']['required_features'] + ['composer'] + (['remote_ui'] if remote else [])
send(dict(id=init['id'], result=dict(api_version=api, tools=[], commands=[dict(name='probe',description='probe')],
    protocol=dict(version=api, features=features, limits=dict(max_concurrent_requests=1)))))
call = receive()
parent, owner = call['id'], call['params']['context']['resource_owner']
checkpoint = dict(surface_id='editor', mount_id='remote.1', input_revision=0, checkpoint_revision=1)
def request(id, method, checkpoint, expected):
    params = dict(parent_request_id=parent, text='draft', resource_owner=dict(owner, session_id='ignored-foreign-session'))
    if checkpoint is not None: params['editor_checkpoint'] = checkpoint
    send(dict(id=id, method=method, params=params))
    answer = receive()
    assert answer['id'] == id, answer
    if expected: assert answer['error']['message'].startswith(expected), answer
    else: assert 'result' in answer, answer
request('insert', 'composer/insert', checkpoint, 'invalid_request')
if remote:
    request('future', 'composer/set', dict(checkpoint, input_revision=2**53), 'bounds_exceeded')
    request('zero', 'composer/set', dict(checkpoint, checkpoint_revision=0), 'bounds_exceeded')
    request('unknown', 'composer/set', dict(checkpoint, extra=1), 'invalid_request')
    request('valid', 'composer/set', checkpoint, None)
else:
    request('unsupported', 'composer/set', checkpoint, 'unsupported_feature')
request('plain', 'composer/set', None, None)
send(dict(id=parent, result=dict(text='probe-ok')))
shutdown = receive()
assert shutdown['method'] == 'shutdown', shutdown
send(dict(id=shutdown['id'], result={}))
"#.replace("__API__", api).replace("__REMOTE__", if remote { "True" } else { "False" });
        write_executable_script(&temp.path().join("extension.py"), &script);
        let manifest = ExtensionManifest::parse(&format!(
            r#"
name = "editor-checkpoint-process"
version = "0.1.0"
api_version = "{api}"
[entrypoint]
command = "extension.py"
[contributes]
commands = ["probe"]
"#
        ))
        .unwrap();
        let mut config = ExtensionRuntimeConfig::new(temp.path());
        config.remote_ui = remote.then(|| Arc::new(Notify::new()));
        config.supervise = false;
        config.request_timeout = Duration::from_secs(3);
        let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
            .await
            .unwrap();
        let mut events = process.subscribe();
        let context = process.current_context_for_resource_owner("session-owner");
        let expected_owner = context.resource_owner.clone().unwrap();
        let call = tokio::spawn({
            let process = process.clone();
            async move { process.execute_command("probe", vec![], context).await }
        });
        for expected_id in if remote {
            vec!["valid", "plain"]
        } else {
            vec!["plain"]
        } {
            let (request_id, generation, event_owner, operation) =
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let ExtensionEvent::ComposerRequested {
                            request_id,
                            generation,
                            owner,
                            operation,
                        } = events.recv().await.unwrap()
                        {
                            break (request_id, generation, owner, operation);
                        }
                    }
                })
                .await
                .unwrap();
            assert_eq!(request_id, ExtensionRequestId::String(expected_id.into()));
            assert_eq!(event_owner, Some(expected_owner.clone()));
            let result = if expected_id == "valid" {
                let ExtensionComposerOperation::Checkpoint {
                    text,
                    owner,
                    checkpoint,
                } = operation
                else {
                    panic!("expected checkpoint")
                };
                assert_eq!(owner, expected_owner);
                assert_eq!(text, "draft");
                assert_eq!(
                    (checkpoint.input_revision, checkpoint.checkpoint_revision),
                    (0, 1)
                );
                serde_json::json!({"input_revision":0,"checkpoint_revision":1})
            } else {
                assert_eq!(
                    operation,
                    ExtensionComposerOperation::Set {
                        text: "draft".into()
                    }
                );
                serde_json::json!({})
            };
            process
                .respond_to_extension_request(
                    request_id,
                    generation,
                    ExtensionRequestOutcome::Ok(result),
                )
                .await
                .unwrap();
        }
        assert_eq!(call.await.unwrap().unwrap().text, "probe-ok");
        assert!(process.shutdown().await);
    }
}
