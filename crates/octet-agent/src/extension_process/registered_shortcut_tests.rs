use super::*;
use crate::extension_process::tests::{trusted_descriptor, write_executable_script};
use crate::session::Session;
use serde_json::{json, Value};

const SCRIPT: &str = r#"#!/usr/bin/env python3
import json,sys

def send(v): print(json.dumps(v),flush=True)
def reply(m,r): send({'jsonrpc':'2.0','id':m['id'],'result':r})
def control(m):
 if m.get('method')=='session/snapshot/prepare':
  d=m['params']['snapshot']; reply(m,{'accepted':True,'transfer_id':d['transfer_id'],'view_revision':d['view_revision'],'head':d['head'],'sha256':d['sha256']}); return True
 if m.get('method')=='shutdown': reply(m,{}); sys.exit(0)
 return False
def wait_child(ident):
 for line in sys.stdin:
  m=json.loads(line)
  if control(m): continue
  if m.get('id')==ident and ('result' in m or 'error' in m): return m
init=json.loads(sys.stdin.readline()); offer=init['params']['protocol']
features=offer['required_features']+['shortcuts','session_entries','session_owner_routes_v1','session_snapshot_transport_v1','session_append_receipts_v1']
reply(init,{'api_version':'0.4','tools':[],'commands':[{'name':'prime','description':'Register a shortcut'}],
 'shortcuts':[{'key':'ctrl+shift+p','name':'static','description':'Static action'}],
 'protocol':{'version':'0.4','features':features,'limits':{'max_concurrent_requests':4,'max_message_bytes':offer['limits']['max_message_bytes']},'session_snapshot_transport_v1':offer['session_snapshot_transport_v1']}})
for line in sys.stdin:
 m=json.loads(line); method=m.get('method'); p=m.get('params',{})
 if control(m): continue
 if method=='command/execute':
  ident='register-'+str(m['id']); name=p['arguments'][0]
  send({'jsonrpc':'2.0','id':ident,'method':'shortcut/register','params':{'parent_request_id':m['id'],'id':name,'key':'ctrl+alt+f','description':'Registered action'}})
  ack=wait_child(ident); reply(m,{'text':json.dumps(ack)})
 elif method=='shortcut/execute':
  ident='append-'+str(m['id'])
  send({'jsonrpc':'2.0','id':ident,'method':'session/append_entry','params':{'parent_request_id':m['id'],'entry_type':'shortcut','data':{'name':p['name'],'parent':m['id']}}})
  ack=wait_child(ident); reply(m,{'text':json.dumps(ack)})
"#;

async fn fixture() -> (
    tempfile::TempDir,
    ExtensionProcess,
    Session,
    broadcast::Receiver<ExtensionEvent>,
) {
    let temp = tempfile::tempdir().unwrap();
    write_executable_script(&temp.path().join("shortcut.py"), SCRIPT);
    let manifest = ExtensionManifest::parse(
        r#"name = "registered-shortcut"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "shortcut.py"
[contributes]
commands = ["prime"]
shortcuts = [{key = "ctrl+shift+p", name = "static", description = "Static action"}]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    let events = process.subscribe();
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    publish(&process, &session).await;
    (temp, process, session, events)
}

async fn publish(process: &ExtensionProcess, session: &Session) {
    process
        .set_host_state_with_session(ExtensionHostState::default(), session)
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
}

fn prime(
    process: &ExtensionProcess,
    session: &Session,
    name: &str,
) -> tokio::task::JoinHandle<Result<CommandOutput, ExtensionRuntimeError>> {
    let process = process.clone();
    let context = process.current_context_for_resource_owner(session.resource_owner_key());
    let name = name.to_owned();
    tokio::spawn(async move { process.execute_command("prime", vec![name], context).await })
}

async fn registration(
    events: &mut broadcast::Receiver<ExtensionEvent>,
) -> (ExtensionRequestId, u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::ShortcutRequested {
                request_id,
                generation,
                owner,
                ..
            } = events.recv().await.unwrap()
            {
                assert!(owner.is_some());
                return (request_id, generation);
            }
        }
    })
    .await
    .unwrap()
}

async fn refused(process: &ExtensionProcess, name: &str, context: ExtensionExecutionContext) {
    let (started, mut operation) = oneshot::channel();
    assert!(process
        .execute_shortcut_controlled(name, context, started)
        .await
        .is_err());
    assert!(operation.try_recv().is_err());
}

#[tokio::test]
async fn registered_shortcut_requires_ack_and_current_owner_then_appends_durably() {
    let (_temp, process, mut session, mut events) = fixture().await;
    let context = process.current_context_for_resource_owner(session.resource_owner_key());
    let call = prime(&process, &session, "accepted");
    let (id, generation) = registration(&mut events).await;
    refused(&process, "accepted", context.clone()).await;
    process
        .respond_to_extension_request(id, generation, ExtensionRequestOutcome::Ok(json!({})))
        .await
        .unwrap();
    call.await.unwrap().unwrap();
    refused(&process, "unknown", context.clone()).await;
    let mut absent = context.clone();
    absent.resource_owner = None;
    refused(&process, "accepted", absent).await;
    let mut foreign = context.clone();
    foreign
        .resource_owner
        .as_mut()
        .unwrap()
        .extension_instance_id = "foreign".into();
    refused(&process, "accepted", foreign).await;
    let mut stale = context.clone();
    stale.resource_owner.as_mut().unwrap().process_generation += 1;
    refused(&process, "accepted", stale).await;
    let (started, operation) = oneshot::channel();
    let invocation = tokio::spawn({
        let process = process.clone();
        async move {
            process
                .execute_shortcut_controlled("accepted", context, started)
                .await
        }
    });
    let operation = operation.await.unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::SessionEntryRequested {
                request_id,
                generation,
                operation: ExtensionSessionEntryOperation::Append { entry_type, data },
                ..
            } = events.recv().await.unwrap()
            {
                assert_eq!(data["parent"], json!(operation.parent_request_id));
                assert_eq!(data["name"], "accepted");
                break process
                    .append_session_entry_request(
                        &request_id,
                        generation,
                        &mut session,
                        &entry_type,
                        data,
                    )
                    .unwrap();
            }
        }
    })
    .await
    .unwrap();
    let result = invocation.await.unwrap().unwrap();
    let ack: Value = serde_json::from_str(&result.text).unwrap();
    assert_eq!(ack["result"], receipt);
    assert_eq!(session.entries().len(), 1);
    assert_eq!(Session::open(session.path()).unwrap().entries().len(), 1);
    assert!(
        !lock_std_mutex(&read_std_lock(&process.inner.connection).pending)
            .contains_key(&operation.parent_request_id)
    );
    process.shutdown().await;
}

#[tokio::test]
async fn failed_nonempty_and_over_limit_registration_ack_never_grants_authority() {
    let (_temp, process, session, mut events) = fixture().await;
    let context = process.current_context_for_resource_owner(session.resource_owner_key());
    for (name, outcome) in [
        (
            "failed",
            ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "denied".into(),
            ),
        ),
        (
            "nonempty",
            ExtensionRequestOutcome::Ok(json!({"accepted": true})),
        ),
    ] {
        let call = prime(&process, &session, name);
        let (id, generation) = registration(&mut events).await;
        process
            .respond_to_extension_request(id, generation, outcome)
            .await
            .unwrap();
        call.await.unwrap().unwrap();
        refused(&process, name, context.clone()).await;
    }
    for index in 0..MAX_EXTENSION_SHORTCUTS {
        let call = prime(&process, &session, &format!("registered_{index}"));
        let (id, generation) = registration(&mut events).await;
        process
            .respond_to_extension_request(id, generation, ExtensionRequestOutcome::Ok(json!({})))
            .await
            .unwrap();
        call.await.unwrap().unwrap();
    }
    let call = prime(&process, &session, "over_limit");
    let (id, generation) = registration(&mut events).await;
    assert!(process
        .respond_to_extension_request(
            id.clone(),
            generation,
            ExtensionRequestOutcome::Ok(json!({}))
        )
        .await
        .is_err());
    refused(&process, "over_limit", context).await;
    process
        .respond_to_extension_request(
            id,
            generation,
            ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, "full".into()),
        )
        .await
        .unwrap();
    call.await.unwrap().unwrap();
    assert!(session.entries().is_empty());
    process.shutdown().await;
}

#[tokio::test]
async fn registration_cancelled_while_ack_waits_for_writer_never_grants_authority() {
    let (_temp, process, session, mut events) = fixture().await;
    let call = prime(&process, &session, "cancelled");
    let (id, generation) = registration(&mut events).await;
    let connection = read_std_lock(&process.inner.connection).clone();
    let permits = tokio::time::timeout(Duration::from_secs(5), async {
        let mut permits = Vec::new();
        for _ in 0..connection.writer.max_capacity() {
            permits.push(connection.writer.clone().reserve_owned().await.unwrap());
        }
        permits
    })
    .await
    .unwrap();
    let claimed_id = id.clone();
    let ack = tokio::spawn({
        let process = process.clone();
        async move {
            process
                .respond_to_extension_request(
                    id,
                    generation,
                    ExtensionRequestOutcome::Ok(json!({})),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if lock_std_mutex(&connection.child_requests)
                .get(&claimed_id)
                .is_some_and(|child| {
                    child.response_state.state.load(Ordering::Acquire) == CHILD_RESPONDING
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!lock_std_mutex(&connection.registered_shortcuts).contains("cancelled"));
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    drop(permits);
    let _ = tokio::time::timeout(Duration::from_secs(5), ack)
        .await
        .unwrap()
        .unwrap();
    assert!(!lock_std_mutex(&connection.registered_shortcuts).contains("cancelled"));
    refused(
        &process,
        "cancelled",
        process.current_context_for_resource_owner(session.resource_owner_key()),
    )
    .await;
    assert!(session.entries().is_empty());
    process.shutdown().await;
}

#[tokio::test]
async fn static_name_collision_and_reload_do_not_reuse_dynamic_authority() {
    let (_temp, process, session, mut events) = fixture().await;
    let collision = prime(&process, &session, "static").await.unwrap().unwrap();
    let ack: Value = serde_json::from_str(&collision.text).unwrap();
    assert_eq!(ack["error"]["code"], -32602);
    let call = prime(&process, &session, "accepted");
    let (id, generation) = registration(&mut events).await;
    process
        .respond_to_extension_request(id, generation, ExtensionRequestOutcome::Ok(json!({})))
        .await
        .unwrap();
    call.await.unwrap().unwrap();
    let old = process.current_context_for_resource_owner(session.resource_owner_key());
    process.reload().await.unwrap();
    publish(&process, &session).await;
    refused(&process, "accepted", old.clone()).await;
    let call = prime(&process, &session, "accepted");
    let (id, generation) = registration(&mut events).await;
    process
        .respond_to_extension_request(id, generation, ExtensionRequestOutcome::Ok(json!({})))
        .await
        .unwrap();
    call.await.unwrap().unwrap();
    refused(&process, "accepted", old).await;
    assert_eq!(
        lock_std_mutex(&read_std_lock(&process.inner.connection).registered_shortcuts).len(),
        1
    );
    assert!(session.entries().is_empty());
    process.shutdown().await;
}
