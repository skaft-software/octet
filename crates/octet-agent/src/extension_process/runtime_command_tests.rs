use super::super::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner, wave1_line,
};
use super::*;

fn state() -> (ProtocolReadState, mpsc::Receiver<WriterFrame>) {
    let (events, _received) = broadcast::channel(16);
    let (state, frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
        protocol
            .features
            .insert(EXTENSION_FEATURE_RUNTIME_COMMANDS.into());
        protocol
            .features
            .insert("runtime_command_registration_v1".into());
    }
    insert_test_parent(&state, 7, Some(test_resource_owner("session")));
    (state, frames)
}

fn request(id: u64, commands: serde_json::Value) -> Vec<u8> {
    wave1_line(
        id,
        "commands/register",
        serde_json::json!({
            "parent_request_id": 7,
            "resource_owner": test_resource_owner("session"),
            "commands": commands,
        }),
    )
}

#[test]
fn runtime_command_registration_acknowledges_live_catalog_before_dispatch() {
    let (state, mut frames) = state();
    handle_protocol_line(
        &request(
            100,
            serde_json::json!([
                {"name":"acp-fleet","description":"Inspect delegates"},
                {"name":"mcp:start","description":"Start server"}
            ]),
        ),
        &state,
    )
    .unwrap();
    let response: serde_json::Value =
        serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
    assert_eq!(
        response["result"]["commands"],
        serde_json::json!(["acp-fleet", "mcp:start"])
    );
    assert_eq!(read_std_lock(&state.command_catalog).len(), 2);
    handle_protocol_line(
        &request(
            101,
            serde_json::json!([
                {"name":"acp-fleet","description":"Updated delegates"}
            ]),
        ),
        &state,
    )
    .unwrap();
    let response: serde_json::Value =
        serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        read_std_lock(&state.command_catalog)[0].description,
        "Updated delegates"
    );
    assert_eq!(read_std_lock(&state.command_catalog).len(), 2);
}

#[test]
fn runtime_command_registration_rejects_unnegotiated_invalid_stale_and_draining_requests() {
    let (state, mut frames) = state();
    let commands = serde_json::json!([{"name":"acp-fleet","description":"Inspect delegates"}]);
    write_std_lock(&state.protocol).features.clear();
    handle_protocol_line(&request(100, commands.clone()), &state).unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    write_std_lock(&state.protocol)
        .features
        .insert(EXTENSION_FEATURE_RUNTIME_COMMANDS.into());
    write_std_lock(&state.protocol)
        .features
        .insert("runtime_command_registration_v1".into());
    for (id, invalid) in [
        (
            101,
            serde_json::json!([{"name":"bad/name","description":"invalid"}]),
        ),
        (
            102,
            serde_json::json!([{"name":"acp-fleet","description":""}]),
        ),
        (
            103,
            serde_json::json!([{"name":"acp-fleet","description":"valid"},{"name":"acp-fleet","description":"duplicate"}]),
        ),
    ] {
        handle_protocol_line(&request(id, invalid), &state).unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
                .unwrap()
                .get("error")
                .is_some()
        );
        assert!(read_std_lock(&state.command_catalog).is_empty());
    }
    lock_std_mutex(&state.issued_resource_owners).clear();
    handle_protocol_line(&request(104, commands.clone()), &state).unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert!(read_std_lock(&state.command_catalog).is_empty());
    lock_std_mutex(&state.issued_resource_owners).insert(test_resource_owner("session"));
    state.draining.store(true, Ordering::Release);
    handle_protocol_line(&request(105, commands), &state).unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert!(read_std_lock(&state.command_catalog).is_empty());
}

#[test]
fn runtime_command_registration_is_atomic_on_catalog_limit_and_writer_backpressure() {
    let (state, mut frames) = state();
    let commands = (0..MAX_EXTENSION_COMMANDS)
        .map(|i| serde_json::json!({"name":format!("cmd-{i}"),"description":"Command"}))
        .collect::<Vec<_>>();
    handle_protocol_line(&request(100, serde_json::json!(commands)), &state).unwrap();
    let _ack = frames.try_recv().unwrap();
    assert_eq!(
        read_std_lock(&state.command_catalog).len(),
        MAX_EXTENSION_COMMANDS
    );
    handle_protocol_line(
        &request(
            101,
            serde_json::json!([{"name":"extra","description":"Too many"}]),
        ),
        &state,
    )
    .unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert_eq!(
        read_std_lock(&state.command_catalog).len(),
        MAX_EXTENSION_COMMANDS
    );
    write_std_lock(&state.command_catalog).clear();
    while state
        .writer
        .try_send(WriterFrame {
            line: b"{}\n".to_vec(),
            state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
            completion: None,
            bus_delivery: None,
        })
        .is_ok()
    {}
    assert!(handle_protocol_line(
        &request(
            102,
            serde_json::json!([{"name":"acp-fleet","description":"No credit"}])
        ),
        &state
    )
    .is_err());
    assert!(read_std_lock(&state.command_catalog).is_empty());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}

#[test]
fn runtime_command_registration_accepts_retained_issued_owner_but_not_cancelled_parent() {
    let (state, mut frames) = state();
    lock_std_mutex(&state.pending).clear();
    handle_protocol_line(
        &request(
            100,
            serde_json::json!([{"name":"acp","description":"Delegates"}]),
        ),
        &state,
    )
    .unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_none()
    );
    lock_std_mutex(&state.tombstones).insert(7, Duration::from_secs(30));
    handle_protocol_line(
        &request(
            101,
            serde_json::json!([{"name":"cancelled","description":"Must not publish"}]),
        ),
        &state,
    )
    .unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert_eq!(read_std_lock(&state.command_catalog).len(), 1);
}

#[test]
fn runtime_command_registration_preserves_version_owner_and_closed_params_fences() {
    let (state, mut frames) = state();
    let definitions = serde_json::json!([{"name":"acp","description":"Delegates"}]);
    write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_2.into();
    handle_protocol_line(&request(100, definitions.clone()), &state).unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_4.into();
    for (id, owner) in [
        (101, test_resource_owner("foreign")),
        (
            102,
            ExtensionResourceOwner {
                process_generation: 2,
                ..test_resource_owner("session")
            },
        ),
    ] {
        handle_protocol_line(&wave1_line(id, "commands/register", serde_json::json!({"parent_request_id":7,"resource_owner":owner,"commands":definitions})), &state).unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
                .unwrap()
                .get("error")
                .is_some()
        );
    }
    handle_protocol_line(&wave1_line(103, "commands/register", serde_json::json!({"parent_request_id":7,"resource_owner":test_resource_owner("session"),"commands":definitions,"unknown":true})), &state).unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&frames.try_recv().unwrap().line)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert!(read_std_lock(&state.command_catalog).is_empty());
}

#[test]
fn runtime_command_registration_feature_requires_runtime_commands_and_api_04() {
    for (version, features, allowed) in [
        (
            "0.4",
            vec!["runtime_commands", "runtime_command_registration_v1"],
            true,
        ),
        ("0.4", vec!["runtime_command_registration_v1"], false),
        (
            "0.2",
            vec!["runtime_commands", "runtime_command_registration_v1"],
            false,
        ),
    ] {
        let manifest = ExtensionManifest::parse(&format!("name = \"command-features\"\nversion = \"0.1.0\"\napi_version = {version:?}\n[entrypoint]\ncommand = \"never-run\"\n")).unwrap();
        let response: InitializeResponse = serde_json::from_value(serde_json::json!({
            "api_version":version,"tools":[],"commands":[],
            "protocol":{"version":version,"features":(["request_cancellation","content_parts"].into_iter().chain(features).collect::<Vec<_>>()),"limits":{"max_concurrent_requests":1}},
        })).unwrap();
        let result = negotiate_contributions_with_host_services(
            &manifest,
            response,
            8,
            OfferedHostServices::default(),
        );
        assert_eq!(result.is_ok(), allowed, "{version}: {result:?}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_command_registration_routes_a_new_command_on_the_same_live_process() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("runtime-commands.py");
    std::fs::write(&script, r#"#!/usr/bin/env python3
import json, sys
def send(frame):
    print(json.dumps(frame), flush=True)
initialize = json.loads(sys.stdin.readline())
send({"jsonrpc":"2.0","id":initialize["id"],"result":{
    "api_version":"0.4","tools":[],"commands":[{"name":"register","description":"Register delegates"}],
    "protocol":{"version":"0.4","features":["request_cancellation","content_parts","runtime_commands","runtime_command_registration_v1"],"limits":{"max_concurrent_requests":2}}
}})
registered = False
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "shutdown":
        send({"jsonrpc":"2.0","id":message["id"],"result":{}})
        break
    if message["params"]["name"] == "register":
        send({"jsonrpc":"2.0","id":"registration","method":"commands/register","params":{
            "parent_request_id":message["id"],"resource_owner":message["params"]["context"]["resource_owner"],
            "commands":[{"name":"acp-fleet","description":"Inspect delegates"}]
        }})
        acknowledgement = json.loads(sys.stdin.readline())
        registered = acknowledgement.get("result", {}).get("commands") == ["register", "acp-fleet"]
        text = "registered" if registered else "refused"
    else:
        text = "delegates" if registered else "missing"
    send({"jsonrpc":"2.0","id":message["id"],"result":{"text":text}})
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest::parse("name = \"runtime-command-process\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"runtime-commands.py\"\n[contributes]\ncommands = [\"register\"]\n").unwrap();
    let mut config = ExtensionRuntimeConfig::new(root.path());
    config.request_timeout = Duration::from_secs(2);
    config.supervise = false;
    let process = ExtensionProcess::start(
        super::super::tests::trusted_descriptor(root.path(), manifest),
        config,
    )
    .await
    .unwrap();
    let mut context = process.current_context();
    context.resource_owner = Some(test_resource_owner("session"));
    let output = process
        .execute_command("register", vec![], context.clone())
        .await
        .unwrap();
    assert_eq!(output.text, "registered");
    let output = process
        .execute_command("acp-fleet", vec![], context)
        .await
        .unwrap();
    assert_eq!(output.text, "delegates");
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_command_registration_in_real_pi_session_start_publishes_and_executes_handlers() {
    let root = tempfile::tempdir().unwrap();
    let factory = root.path().join("late-commands.mjs");
    std::fs::write(
        &factory,
        r#"export default pi => {
      let invoked = 0;
      pi.registerCommand('verify-late', { description: 'Verify delegate', handler: async () => {
        if (invoked !== 1) throw new Error(`late handler invocation count ${invoked}`);
      }});
      pi.on('session_start', () => {
        pi.registerCommand('mcp:start', { description: 'Start delegates', handler: async args => {
          if (args !== 'worker') throw new Error('wrong delegate arguments');
          invoked++;
        }});
      });
    };"#,
    )
    .unwrap();
    let manifest_path = super::super::pi_fixture::capture(root.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(root.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    config.host_state.has_ui = Some(false);
    let process = ExtensionProcess::start(
        super::super::tests::trusted_descriptor(manifest_path.parent().unwrap(), manifest),
        config,
    )
    .await
    .unwrap();
    assert!(!process
        .command_definitions()
        .iter()
        .any(|command| command.name == "mcp:start"));
    process.start_session_hook_binding("session").await.unwrap();
    assert!(process
        .command_definitions()
        .iter()
        .any(|command| command.name == "mcp:start"));
    let context = process.current_context_for_resource_owner("session");
    process
        .execute_command("mcp:start", vec!["worker".into()], context.clone())
        .await
        .unwrap();
    process
        .execute_command("verify-late", vec![], context)
        .await
        .unwrap();
    assert!(process.shutdown().await);
}
