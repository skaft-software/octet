use super::*;

#[tokio::test]
async fn pi_settings_native_process_uses_the_actual_settled_session_start_parent() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let agent_dir = root.join("agent");
    std::fs::create_dir(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("settings.json"),
        br#"{"theme":"retained","extensions":[]}"#,
    )
    .unwrap();
    let script = root.join("settings.py");
    std::fs::write(&script, r#"#!/usr/bin/env python3
import json, sys, os

def send(value):
    print(json.dumps(value), flush=True)

initialize = json.loads(sys.stdin.readline())
workspace = initialize["params"]["workspace"]
send({"jsonrpc":"2.0","id":initialize["id"],"result":{
    "api_version":"0.4","tools":[],"commands":[{"name":"probe","description":"Probe retained settings"}],
    "protocol":{"version":"0.4","features":["request_cancellation","content_parts","runtime_commands","lifecycle_events","pi_settings_v1"],"limits":{"max_concurrent_requests":2}}
}})
origin = None
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "shutdown":
        send({"jsonrpc":"2.0","id":message["id"],"result":{}})
        break
    if method == "hook/run":
        origin = message
        send({"jsonrpc":"2.0","id":message["id"],"result":{}})
    elif method == "command/execute":
        assert origin is not None
        params = {"parent_request_id":origin["id"], "resource_owner":origin["params"]["context"]["resource_owner"],
            "cwd":workspace, "agent_dir":os.path.join(workspace,"agent"), "project_trusted":False}
        invalid = dict(params, parent_request_id=origin["id"]+100000)
        send({"jsonrpc":"2.0","id":"foreign-parent","method":"pi/settings/read","params":invalid})
        assert "error" in json.loads(sys.stdin.readline())
        send({"jsonrpc":"2.0","id":"read","method":"pi/settings/read","params":params})
        read = json.loads(sys.stdin.readline())
        assert read["result"]["global"]["theme"] == "retained", read
        assert read["result"]["errors"] == [], read
        params.update(scope="global",extensions=["-builtin:mcp"])
        send({"jsonrpc":"2.0","id":"update","method":"pi/settings/update","params":params})
        updated = json.loads(sys.stdin.readline())
        assert updated.get("result") == {}, updated
        send({"jsonrpc":"2.0","id":message["id"],"result":{"text":"persisted with authentic retained parent"}})
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest::parse("name = \"native-pi-settings\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"settings.py\"\n[contributes]\ncommands = [\"probe\"]\nhooks = [\"session_start\"]\n").unwrap();
    let mut config = ExtensionRuntimeConfig::new(&root);
    config.pi_settings = Some(PiSettingsConfig {
        workspace: root.clone(),
        agent_dir: agent_dir.clone(),
        project_trusted: false,
    });
    config.supervise = false;
    config.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(
        super::super::tests::trusted_descriptor(&root, manifest),
        config,
    )
    .await
    .unwrap();
    assert!(process.negotiated_features().contains(FEATURE));
    process.start_session_hook_binding("session").await.unwrap();
    let output = process
        .execute_command(
            "probe",
            vec![],
            process.current_context_for_resource_owner("session"),
        )
        .await
        .unwrap();
    assert_eq!(output.text, "persisted with authentic retained parent");
    let settings: Value =
        serde_json::from_slice(&std::fs::read(agent_dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        settings,
        json!({"theme":"retained","extensions":["-builtin:mcp"]})
    );
    assert!(process.shutdown().await);
}
