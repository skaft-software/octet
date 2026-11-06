use super::*;
use crate::extension_process::tests::{trusted_descriptor, write_executable_script};
use crate::session::{EntryValue, Session};

const SCRIPT: &str = r#"#!/usr/bin/env python3
import json,sys

def send(v): print(json.dumps(v),flush=True)
def reply(m,r): send({'jsonrpc':'2.0','id':m['id'],'result':r})
init=json.loads(sys.stdin.readline()); p=init['params']; offer=p['protocol']
features=offer['required_features']+['session_entries','session_owner_routes_v1','session_snapshot_transport_v1','session_append_receipts_v1']
reply(init,{'api_version':'0.4','tools':[],'commands':[{'name':'append','description':'Append entries'}],
 'protocol':{'version':'0.4','features':features,'limits':{'max_concurrent_requests':4,'max_message_bytes':offer['limits']['max_message_bytes']},'session_snapshot_transport_v1':offer['session_snapshot_transport_v1']}})
for line in sys.stdin:
 m=json.loads(line); method=m.get('method'); p=m.get('params',{})
 if method=='session/snapshot/prepare':
  d=p['snapshot']; reply(m,{'accepted':True,'transfer_id':d['transfer_id'],'view_revision':d['view_revision'],'head':d['head'],'sha256':d['sha256']})
 elif method=='command/execute':
  results=[]
  for i in range(2):
   ident=str(m['id'])+':append:'+str(i)
   send({'jsonrpc':'2.0','id':ident,'method':'session/append_entry','params':{'parent_request_id':m['id'],'entry_type':'note','data':{'index':i}}})
   for child_line in sys.stdin:
    child=json.loads(child_line)
    if child.get('method')=='shutdown': reply(child,{}); sys.exit(0)
    if child.get('id')==ident and ('result' in child or 'error' in child):
     results.append(child); break
  reply(m,{'text':json.dumps(results)})
 elif method=='shutdown': reply(m,{}); break
"#;

#[tokio::test]
async fn grantless_append_receipt_is_durable_parent_chained_and_refuses_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_script(&temp.path().join("extension.py"), SCRIPT);
    let manifest = ExtensionManifest::parse(
        r#"name = "append-process"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
commands = ["append"]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    let mut events = process.subscribe();
    let mut session = Session::create(temp.path().join("session.jsonl")).unwrap();
    process
        .set_host_state_with_session(ExtensionHostState::default(), &session)
        .unwrap();
    let owner = process
        .current_context_for_resource_owner(session.resource_owner_key())
        .resource_owner
        .unwrap();
    process
        .wait_for_session_snapshot_prepared(
            &owner,
            Duration::from_secs(5),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    let call = tokio::spawn({
        let process = process.clone();
        let context = process.current_context_for_resource_owner(session.resource_owner_key());
        async move { process.execute_command("append", vec![], context).await }
    });
    let mut receipts = Vec::new();
    for index in 0..2 {
        let (id, generation, entry_type, data) =
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let ExtensionEvent::SessionEntryRequested {
                        request_id,
                        generation,
                        operation: ExtensionSessionEntryOperation::Append { entry_type, data },
                        ..
                    } = events.recv().await.unwrap()
                    {
                        break (request_id, generation, entry_type, data);
                    }
                }
            })
            .await
            .unwrap();
        let before = std::fs::read(session.path()).unwrap();
        if index == 0 {
            assert!(process
                .append_session_entry_request(
                    &id,
                    generation + 1,
                    &mut session,
                    &entry_type,
                    data.clone()
                )
                .is_err());
            let connection = read_std_lock(&process.inner.connection).clone();
            write_std_lock(&connection.protocol)
                .features
                .remove(APPEND_RECEIPTS);
            assert!(process
                .append_session_entry_request(
                    &id,
                    generation,
                    &mut session,
                    &entry_type,
                    data.clone()
                )
                .is_err());
            write_std_lock(&connection.protocol)
                .features
                .insert(APPEND_RECEIPTS.into());
            let mut other = Session::create(temp.path().join("other.jsonl")).unwrap();
            assert!(process
                .append_session_entry_request(
                    &id,
                    generation,
                    &mut other,
                    &entry_type,
                    data.clone()
                )
                .is_err());
            assert!(other.entries().is_empty());
            let mut permits = Vec::new();
            while let Ok(permit) = connection.writer.clone().try_reserve_owned() {
                permits.push(permit);
            }
            assert!(matches!(
                process.append_session_entry_request(
                    &id,
                    generation,
                    &mut session,
                    &entry_type,
                    data.clone()
                ),
                Err((ExtensionRequestFailure::BoundsExceeded, _))
            ));
            drop(permits);
            assert_eq!(std::fs::read(session.path()).unwrap(), before);
            // A host write other than a config selection is a real divergence
            // from the parent's pinned view: refuse without writing.
            session
                .append(EntryValue::Message(octet_ai::Message::User(
                    octet_ai::UserMessage {
                        content: vec![octet_ai::UserPart::Text("host write".into())],
                    },
                )))
                .unwrap();
            let changed = std::fs::read(session.path()).unwrap();
            assert!(process
                .append_session_entry_request(
                    &id,
                    generation,
                    &mut session,
                    &entry_type,
                    data.clone()
                )
                .is_err());
            assert_eq!(std::fs::read(session.path()).unwrap(), changed);
            session.checkout_root().unwrap();
        }
        // The parent's own model/thinking selection commits a host config
        // entry after its pinned head; the receipt chains it exactly.
        let host_config = (index == 1).then(|| {
            session
                .append(EntryValue::Config {
                    model: None,
                    reasoning: None,
                    reasoning_mode: None,
                })
                .unwrap()
        });
        let receipt = process
            .append_session_entry_request(&id, generation, &mut session, &entry_type, data)
            .unwrap();
        match &host_config {
            Some(config) => {
                assert_eq!(receipt["gap"].as_array().unwrap().len(), 1);
                assert_eq!(receipt["gap"][0]["id"], json!(config.0));
                assert_eq!(receipt["gap"][0]["value"]["type"], "config");
                assert_eq!(receipt["entry"]["parent"], json!(config.0));
            }
            None => assert!(receipt.get("gap").is_none()),
        }
        let entry = session
            .entry(&crate::session::EntryId(
                receipt["entry_id"].as_str().unwrap().into(),
            ))
            .unwrap();
        assert_eq!(
            receipt["entry"]["timestamp_unix_ms"],
            json!(entry.timestamp_unix_ms.unwrap())
        );
        assert_eq!(receipt["entry"]["parent"], json!(entry.parent));
        assert_eq!(
            receipt["entry"]["value"],
            serde_json::to_value(&entry.value).unwrap()
        );
        assert_eq!(receipt["successor"], Value::Null);
        assert_eq!(
            receipt["entry"]["metadata"]["extension_metadata"]["append-process"]["value"]["data"]
                ["index"],
            index
        );
        assert_eq!(
            receipt["entry"]["metadata"]["extension_metadata"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert!(
            receipt["view_revision"].as_u64().unwrap()
                > receipt["previous_revision"].as_u64().unwrap()
        );
        receipts.push(receipt);
    }
    assert_eq!(
        receipts[1]["previous_revision"],
        receipts[0]["view_revision"]
    );
    assert_eq!(receipts[1]["previous_head"], receipts[0]["head"]);
    let output = call.await.unwrap().unwrap();
    let returned: Vec<Value> = serde_json::from_str(&output.text).unwrap();
    assert_eq!(
        returned
            .iter()
            .map(|r| r["result"].clone())
            .collect::<Vec<_>>(),
        receipts
    );
    let reopened = Session::open(session.path()).unwrap();
    for receipt in receipts {
        assert!(reopened
            .entry(&crate::session::EntryId(
                receipt["entry_id"].as_str().unwrap().into()
            ))
            .is_some());
    }
    process
        .set_host_state_with_session(ExtensionHostState::default(), &session)
        .unwrap();
    process
        .wait_for_session_snapshot_prepared(
            &owner,
            Duration::from_secs(5),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    let cancelled_call = tokio::spawn({
        let process = process.clone();
        let context = process.current_context_for_resource_owner(session.resource_owner_key());
        async move { process.execute_command("append", vec![], context).await }
    });
    let (id, generation, entry_type, data) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::SessionEntryRequested {
                request_id,
                generation,
                operation: ExtensionSessionEntryOperation::Append { entry_type, data },
                ..
            } = events.recv().await.unwrap()
            {
                break (request_id, generation, entry_type, data);
            }
        }
    })
    .await
    .unwrap();
    cancelled_call.abort();
    assert!(cancelled_call.await.unwrap_err().is_cancelled());
    let before = std::fs::read(session.path()).unwrap();
    assert!(matches!(
        process.append_session_entry_request(&id, generation, &mut session, &entry_type, data),
        Err((ExtensionRequestFailure::NotForegroundOwner, _))
    ));
    assert_eq!(std::fs::read(session.path()).unwrap(), before);
    assert!(process.shutdown().await);
}

#[test]
fn grantless_append_receipts_require_api04_and_both_transport_features_and_entries() {
    for (version, paired, entries, accepted) in [
        ("0.4", true, true, true),
        ("0.4", false, true, false),
        ("0.4", true, false, false),
        ("0.2", true, true, false),
    ] {
        let manifest = ExtensionManifest::parse(&format!("name = \"append-features\"\nversion = \"0.1.0\"\napi_version = {version:?}\n[entrypoint]\ncommand = \"never-run\"\n")).unwrap();
        let frame = 1024 * 1024;
        let mut features = vec!["request_cancellation", "content_parts", APPEND_RECEIPTS];
        if paired {
            features.extend([OWNER_ROUTES, SNAPSHOT_TRANSPORT]);
        }
        if entries {
            features.push("session_entries");
        }
        let response: InitializeResponse = serde_json::from_value(json!({
            "api_version":version,"tools":[],"commands":[],
            "protocol":{"version":version,"features":features,
                "limits":{"max_concurrent_requests":2,"max_message_bytes":frame},
                "session_snapshot_transport_v1":SessionSnapshotProfile::for_frame(frame).unwrap()},
        }))
        .unwrap();
        let result = negotiate_contributions_with_host_services(
            &manifest,
            response,
            8,
            OfferedHostServices {
                max_message_bytes: frame,
                ..Default::default()
            },
        );
        assert_eq!(
            result.is_ok(),
            accepted,
            "{version}/{paired}/{entries}: {result:?}"
        );
    }
}

#[tokio::test]
async fn grantless_hook_append_runs_real_pi_handler_and_publishes_durable_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("hook-append.mjs");
    std::fs::write(&factory, r#"import fs from 'node:fs';
    export default pi => {
      pi.on('session_start', (_event, ctx) => {
        const before = ctx.sessionManager.getEntries();
        const previous = ctx.sessionManager.getLeafId();
        if (before.length !== 1) throw new Error('authentic initial history missing');
        for (let index = 0; index < 2; index++) {
          if (pi.appendEntry('hook-state', { index }) !== undefined) throw new Error('append must be synchronous void');
          const entries = ctx.sessionManager.getEntries();
          const entry = entries.at(-1);
          if (entries.length !== index + 2 || entry.customType !== 'hook-state' || entry.data.index !== index || ctx.sessionManager.getLeafId() !== entry.id)
            throw new Error('durable hook receipt not published into pinned context');
        }
        fs.writeFileSync(`${process.cwd()}/hook-appended.json`, JSON.stringify({ previous, entries: ctx.sessionManager.getEntries() }));
      });
    };"#).unwrap();
    let manifest_path =
        crate::extension_process::pi_fixture::capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    config.host_state.has_ui = Some(false);
    let process = ExtensionProcess::start(
        trusted_descriptor(manifest_path.parent().unwrap(), manifest),
        config,
    )
    .await
    .unwrap();
    let mut events = process.subscribe();
    let mut session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let initial = session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    process
        .set_host_state_with_session(ExtensionHostState::default(), &session)
        .unwrap();
    let owner = process
        .current_context_for_resource_owner(session.resource_owner_key())
        .resource_owner
        .unwrap();
    process
        .wait_for_session_snapshot_prepared(
            &owner,
            Duration::from_secs(5),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    let hook = tokio::spawn({
        let process = process.clone();
        let session_key = session.resource_owner_key();
        async move { process.start_session_hook_binding(session_key).await }
    });
    let mut receipts = Vec::new();
    for index in 0..2 {
        let (id, generation, entry_type, data) =
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let ExtensionEvent::SessionEntryRequested {
                        request_id,
                        generation,
                        operation: ExtensionSessionEntryOperation::Append { entry_type, data },
                        ..
                    } = events.recv().await.unwrap()
                    {
                        break (request_id, generation, entry_type, data);
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(entry_type, "hook-state");
        assert_eq!(data, json!({"index": index}));
        let receipt = process
            .append_session_entry_request(&id, generation, &mut session, &entry_type, data)
            .unwrap();
        assert_eq!(receipt["successor"], Value::Null);
        assert_eq!(receipt["entry"]["id"], receipt["entry_id"]);
        receipts.push(receipt);
    }
    hook.await.unwrap().unwrap();
    let witness: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("hook-appended.json")).unwrap())
            .unwrap();
    assert_eq!(witness["previous"], json!(initial));
    assert_eq!(witness["entries"].as_array().unwrap().len(), 3);
    assert_eq!(witness["entries"][1]["id"], receipts[0]["entry_id"]);
    assert_eq!(witness["entries"][1]["parentId"], json!(initial));
    assert_eq!(witness["entries"][2]["id"], receipts[1]["entry_id"]);
    assert_eq!(witness["entries"][2]["parentId"], receipts[0]["entry_id"]);
    assert_eq!(
        receipts[1]["previous_revision"],
        receipts[0]["view_revision"]
    );
    assert_eq!(receipts[1]["previous_head"], receipts[0]["head"]);
    let reopened = Session::open_read_only(session.path()).unwrap();
    assert_eq!(reopened.entries().len(), 3);
    for receipt in receipts {
        let entry = reopened
            .entry(&crate::session::EntryId(
                receipt["entry_id"].as_str().unwrap().into(),
            ))
            .unwrap();
        assert_eq!(
            receipt["entry"]["timestamp_unix_ms"],
            json!(entry.timestamp_unix_ms.unwrap())
        );
        assert_eq!(receipt["entry"]["parent"], json!(entry.parent));
        assert_eq!(
            receipt["entry"]["value"],
            serde_json::to_value(&entry.value).unwrap()
        );
    }
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn pending_messages_retained_real_pi_context_updates_without_a_ui_mount() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("pending.mjs");
    std::fs::write(&factory, r#"import fs from 'node:fs';
    export default pi => {
      let retained, timer;
      pi.on('session_start', (_event, ctx) => {
        retained = ctx;
        timer = setInterval(() => fs.writeFileSync(`${process.cwd()}/pending-observed.txt`, String(ctx.hasPendingMessages())), 5);
        timer.unref();
      });
      pi.on('session_shutdown', () => clearInterval(timer));
      pi.registerCommand('queue-state', { description: 'Check real queue snapshot', handler: async (args, ctx) => {
        const expected = args === 'true';
        if (ctx.hasPendingMessages() !== expected || retained.hasPendingMessages() !== expected)
          throw new Error('stale or fabricated pending-message observation');
      }});
    };"#).unwrap();
    let manifest_path =
        crate::extension_process::pi_fixture::capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    config.host_state.has_ui = Some(false);
    config.host_state.has_pending_messages = Some(false);
    let process = ExtensionProcess::start(
        trusted_descriptor(manifest_path.parent().unwrap(), manifest),
        config,
    )
    .await
    .unwrap();
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    let mut state = process.current_context().host;
    process
        .set_host_state_with_session(state.clone(), &session)
        .unwrap();
    let context = process.current_context_for_resource_owner(session.resource_owner_key());
    process
        .wait_for_session_snapshot_prepared(
            context.resource_owner.as_ref().unwrap(),
            Duration::from_secs(5),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    process
        .start_session_hook_binding(session.resource_owner_key())
        .await
        .unwrap();
    for pending in [false, true, false] {
        state.has_pending_messages = Some(pending);
        process.set_host_state(state.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::fs::read_to_string(temp.path().join("pending-observed.txt"))
                    .ok()
                    .as_deref()
                    == Some(if pending { "true" } else { "false" })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        process
            .execute_command(
                "queue-state",
                vec![pending.to_string()],
                process.current_context_for_resource_owner(session.resource_owner_key()),
            )
            .await
            .unwrap();
    }
    assert!(process.shutdown().await);
}
