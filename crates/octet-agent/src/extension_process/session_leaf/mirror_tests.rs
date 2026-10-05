//! Actual subprocess evidence for initial and retained native session mirrors.
use super::super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use crate::session::{
    EntryMetadata, EntryValue, ExtensionEntryMetadata, ExtensionMetadataProvenance,
};
use pretty_assertions::assert_eq;
use serde_json::{json, Value};
use tempfile::TempDir;

const SCRIPT: &str = r#"#!/usr/bin/env python3
import json, os, sys

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)
def reply(message, result):
    send({"jsonrpc":"2.0", "id":message["id"], "result":result})
def record(message):
    with open(os.path.join(os.path.dirname(__file__), "observed.jsonl"), "a") as f:
        f.write(json.dumps(message) + "\n")
init = json.loads(sys.stdin.readline())
p = init["params"]
features = p["protocol"]["required_features"] + ["remote_ui"]
if os.path.exists(os.path.join(os.path.dirname(__file__), "mirror-enabled")):
    features.append("session_entries")
reply(init, {"api_version":"0.4", "tools":[],
    "commands":[{"name":name,"description":name} for name in ["inspect", "open"]],
    "protocol":{"version":"0.4", "features":features,"limits":{"max_concurrent_requests":4}}})
start = None
update = None
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    params = message.get("params", {})
    if method == "hook/run":
        record(message)
        assert params["payload"]["binding"] == params["context"]["resource_owner"]
        if params["hook"] == "session_start":
            start = params
        reply(message, {"disposition":{"action":"continue"}})
    elif method == "command/execute":
        if params["name"] == "open":
            send({"jsonrpc":"2.0", "id":"open", "method":"ui/open", "params":{
                "parent_request_id":message["id"], "surface_id":"mirror", "title":"Mirror", "placement":"footer"}})
            ack = json.loads(sys.stdin.readline())
            assert ack["id"] == "open" and "result" in ack, ack
            reply(message, {"text":"opened"})
        else:
            reply(message, {"text":json.dumps({"start":start,"update":update,"command":params})})
    elif method == "context/updated":
        update = params
        record(message)
    elif method == "ui/closed":
        pass
    elif method == "shutdown":
        reply(message, {})
        break
    else:
        raise AssertionError(message)
"#;

async fn process(
    enabled: bool,
) -> (
    TempDir,
    ExtensionProcess,
    broadcast::Receiver<ExtensionEvent>,
) {
    let temp = TempDir::new().unwrap();
    // The host stages entrypoint bytes outside the author directory. Fixture
    // markers and observations belong in the original private directory.
    let script = SCRIPT.replace(
        "os.path.dirname(__file__)",
        &serde_json::to_string(temp.path().to_str().unwrap()).unwrap(),
    );
    write_executable_script(&temp.path().join("extension.py"), &script);
    if enabled {
        std::fs::write(temp.path().join("mirror-enabled"), "").unwrap();
    }
    let manifest = ExtensionManifest::parse(
        r#"name = "mirror-process"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
commands = ["inspect", "open"]
hooks = ["session_start", "session_end"]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.remote_ui = Some(Arc::new(Notify::new()));
    config.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    let events = process.subscribe();
    (temp, process, events)
}

fn state(session: &crate::Session) -> ExtensionHostState {
    ExtensionHostState {
        session_id: Some(session.path().file_stem().unwrap().to_str().unwrap().into()),
        model: Some("fixed-model".into()),
        ..Default::default()
    }
}

fn append(session: &mut crate::Session) -> crate::session::EntryId {
    session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap()
}

async fn inspect(process: &ExtensionProcess, session: &crate::Session) -> Value {
    let output = process
        .execute_command(
            "inspect",
            vec![],
            process.current_context_for_resource_owner(session.resource_owner_key()),
        )
        .await
        .unwrap();
    serde_json::from_str(&output.text).unwrap()
}

async fn open(
    process: &ExtensionProcess,
    session: &crate::Session,
    events: &mut broadcast::Receiver<ExtensionEvent>,
) {
    let context = process.current_context_for_resource_owner(session.resource_owner_key());
    let call = tokio::spawn({
        let process = process.clone();
        async move { process.execute_command("open", vec![], context).await }
    });
    let (request_id, generation) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::RemoteUiRequested {
                request_id,
                generation,
                ..
            } = events.recv().await.unwrap()
            {
                break (request_id, generation);
            }
        }
    })
    .await
    .unwrap();
    process
        .respond_to_extension_request(
            request_id,
            generation,
            ExtensionRequestOutcome::Ok(json!({"columns":80,"rows":24})),
        )
        .await
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap().text, "opened");
}

fn observations(temp: &TempDir) -> Vec<Value> {
    std::fs::read_to_string(temp.path().join("observed.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn initial_session_mirror_is_native_complete_namespace_filtered_and_branch_exact() {
    let (temp, process, _events) = process(true).await;
    let path = temp.path().join("session.jsonl");
    let mut session = crate::Session::create(&path).unwrap();
    let root = append(&mut session);
    let abandoned = session
        .append_extension_entry(
            "mirror-process",
            Some(1),
            "abandoned",
            json!({"preserve":true}),
        )
        .unwrap();
    session.checkout(root.clone()).unwrap();
    let own = session
        .append_extension_entry(
            "mirror-process",
            Some(1),
            "clm-state",
            json!({"checkpoint":42}),
        )
        .unwrap();
    let foreign = session
        .append_extension_entry(
            "other-process",
            Some(1),
            "secret",
            json!({"secret":"never disclose"}),
        )
        .unwrap();
    let public = session
        .append_with_metadata(
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            Some(EntryMetadata {
                extension_metadata: [(
                    "public-process".into(),
                    ExtensionEntryMetadata {
                        public: true,
                        value: json!({"shared":true}),
                        provenance: ExtensionMetadataProvenance {
                            extension: "public-process".into(),
                            process_generation: Some(1),
                        },
                    },
                )]
                .into(),
                ..Default::default()
            }),
        )
        .unwrap();
    drop(session);
    let session = crate::Session::open_read_only(&path).unwrap();
    process
        .set_host_state_with_session(state(&session), &session)
        .unwrap();
    process
        .start_session_hook_binding(session.resource_owner_key())
        .await
        .unwrap();
    let seen = inspect(&process, &session).await;
    let host = &seen["start"]["context"]["host"];
    assert_eq!(host["session_entries"].as_array().unwrap().len(), 5);
    assert_eq!(
        host["session_branch"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!(root), json!(own), json!(foreign), json!(public)]
    );
    assert!(host["session_entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == json!(abandoned)));
    assert_eq!(host["session_leaf_id"], json!(session.head()));
    assert_eq!(host["session_file"], json!(path));
    assert!(!host.to_string().contains("never disclose"));
    assert!(host.to_string().contains("checkpoint"));
    assert!(host.to_string().contains("shared"));
    assert_eq!(seen["command"]["context"]["host"], *host);
    assert_eq!(
        seen["start"]["context"]["resource_owner"]["session_id"],
        session.resource_owner_key()
    );
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn history_only_refresh_reaches_retained_context_and_reload_rebinds_generation() {
    let (temp, process, mut events) = process(true).await;
    let mut session = crate::Session::create(temp.path().join("session.jsonl")).unwrap();
    append(&mut session);
    let state = state(&session);
    process
        .set_host_state_with_session(state.clone(), &session)
        .unwrap();
    process
        .start_session_hook_binding(session.resource_owner_key())
        .await
        .unwrap();
    open(&process, &session, &mut events).await;
    let latest = append(&mut session);
    process
        .set_host_state_with_session(state.clone(), &session)
        .unwrap();
    let seen = inspect(&process, &session).await;
    assert_eq!(
        seen["update"]["host"]["session_entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(seen["update"]["host"]["session_leaf_id"], json!(latest));
    assert_eq!(
        seen["update"]["resource_owner"],
        seen["start"]["context"]["resource_owner"]
    );
    let before = observations(&temp).len();
    process
        .set_host_state_with_session(state, &session)
        .unwrap();
    inspect(&process, &session).await; // ordered wire barrier
    assert_eq!(observations(&temp).len(), before);
    let old_generation = process.health_snapshot().generation;
    process.reload().await.unwrap();
    let seen = inspect(&process, &session).await;
    assert!(process.health_snapshot().generation > old_generation);
    assert_eq!(
        seen["start"]["context"]["resource_owner"]["process_generation"],
        process.health_snapshot().generation
    );
    assert_eq!(
        seen["start"]["context"]["host"]["session_leaf_id"],
        json!(latest)
    );
    let connection = read_std_lock(&process.inner.connection).clone();
    let mut stale: ExtensionResourceOwner =
        serde_json::from_value(seen["start"]["context"]["resource_owner"].clone()).unwrap();
    stale.process_generation = old_generation;
    assert!(attach_session_mirror(&connection, &stale, &mut json!({}), false).is_err());
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn empty_is_real_unavailable_is_not_empty_and_foreign_owner_cannot_read_mirror() {
    let (temp, process, mut events) = process(true).await;
    let mut session = crate::Session::create(temp.path().join("session.jsonl")).unwrap();
    let state = state(&session);
    process
        .set_host_state_with_session(state.clone(), &session)
        .unwrap();
    process
        .start_session_hook_binding(session.resource_owner_key())
        .await
        .unwrap();
    let seen = inspect(&process, &session).await;
    assert_eq!(
        seen["start"]["context"]["host"]["session_entries"],
        json!([])
    );
    assert_eq!(
        seen["start"]["context"]["host"]["session_branch"],
        json!([])
    );
    assert_eq!(
        seen["start"]["context"]["host"]["session_leaf_id"],
        Value::Null
    );
    assert!(process
        .execute_command(
            "inspect",
            vec![],
            process.current_context_for_resource_owner("foreign-owner")
        )
        .await
        .is_err());
    open(&process, &session, &mut events).await;
    // Durable per-entry limits remain intact; the complete mirror exceeds its
    // aggregate bound without any individual invalid entry.
    for _ in 0..40 {
        session
            .append_extension_entry(
                "mirror-process",
                Some(1),
                "large",
                json!({"data":"x".repeat(8000)}),
            )
            .unwrap();
    }
    assert!(process
        .set_host_state_with_session(state.clone(), &session)
        .is_err());
    assert!(process
        .execute_command(
            "inspect",
            vec![],
            process.current_context_for_resource_owner(session.resource_owner_key())
        )
        .await
        .is_err());
    // A plain metadata refresh cannot resurrect a failed mirror.
    process.set_host_state(state);
    assert!(process
        .execute_command(
            "inspect",
            vec![],
            process.current_context_for_resource_owner(session.resource_owner_key())
        )
        .await
        .is_err());
    process.shutdown().await;
    let seen = observations(&temp);
    let invalidation = seen
        .iter()
        .find(|message| message["method"] == "context/updated")
        .unwrap();
    assert_eq!(
        invalidation["params"]["host"]["session_entries"],
        Value::Null
    );
    assert_eq!(
        invalidation["params"]["host"]["session_branch"],
        Value::Null
    );
}

#[tokio::test]
async fn distinct_native_owners_with_same_display_id_retire_old_mirror_and_ui() {
    let (temp, process, mut events) = process(true).await;
    let first = crate::Session::create(temp.path().join("session.jsonl")).unwrap();
    std::fs::create_dir(temp.path().join("other")).unwrap();
    let mut second = crate::Session::create(temp.path().join("other/session.jsonl")).unwrap();
    let latest = append(&mut second);
    assert_eq!(state(&first), state(&second));
    process
        .set_host_state_with_session(state(&first), &first)
        .unwrap();
    process
        .start_session_hook_binding(first.resource_owner_key())
        .await
        .unwrap();
    open(&process, &first, &mut events).await;
    let old_owner = process
        .current_context_for_resource_owner(first.resource_owner_key())
        .resource_owner
        .unwrap();
    process
        .set_host_state_with_session(state(&second), &second)
        .unwrap();
    assert!(!process.remote_ui_surface_is_current(&old_owner, "mirror"));
    let connection = read_std_lock(&process.inner.connection).clone();
    assert!(!lock_std_mutex(&connection.issued_resource_owners).contains(&old_owner));
    assert!(process
        .execute_command(
            "inspect",
            vec![],
            process.current_context_for_resource_owner(first.resource_owner_key())
        )
        .await
        .is_err());
    process
        .start_session_hook_binding(second.resource_owner_key())
        .await
        .unwrap();
    let seen = inspect(&process, &second).await;
    assert_eq!(
        seen["start"]["context"]["host"]["session_leaf_id"],
        json!(latest)
    );
    assert_eq!(
        seen["start"]["context"]["host"]["session_entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn unnegotiated_session_entries_does_not_disclose_history() {
    let (temp, process, _events) = process(false).await;
    let mut session = crate::Session::create(temp.path().join("session.jsonl")).unwrap();
    append(&mut session);
    process
        .set_host_state_with_session(state(&session), &session)
        .unwrap();
    process
        .start_session_hook_binding(session.resource_owner_key())
        .await
        .unwrap();
    let seen = inspect(&process, &session).await;
    assert!(seen["start"]["context"]["host"]
        .get("session_entries")
        .is_none());
    assert!(seen["command"]["context"]["host"]
        .get("session_entries")
        .is_none());
    assert!(process.shutdown().await);
}
