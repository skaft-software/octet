//! Regression: an awaited lifecycle callback must receive its dynamic catalog ACK.
use super::*;
use crate::extension_process::tests::{trusted_descriptor, write_executable_script};

#[tokio::test]
async fn session_start_catalog_ack_and_reload_keep_generation_fences() {
    let directory = tempfile::tempdir().unwrap();
    write_executable_script(
        &directory.path().join("late-tools.py"),
        r#"#!/usr/bin/env python3
import json
import os
import sys

def receive():
    line = sys.stdin.readline()
    assert line, 'host closed stdin'
    return json.loads(line)

def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)

initialize = receive()
send({'jsonrpc':'2.0','id':initialize['id'],'result':{
    'api_version':'0.4','tools':[],'commands':[],
    'protocol':{'version':'0.4','features':['request_cancellation','content_parts','dynamic_tools'],
                'limits':{'max_concurrent_requests':1}}}})
while True:
    message = receive()
    if message.get('method') == 'hook/run':
        if message['params']['hook'] == 'session_start':
            generation = message['params']['payload']['binding']['process_generation']
            with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'entered'), 'a') as trace:
                trace.write(str(generation) + '\n')
            send({'jsonrpc':'2.0','id':'late','method':'tools/register','params':{'tools':[
                {'name':'startup_late','description':'Generation '+str(generation),'parameters':{'type':'object'}}]}})
            ack = receive()
            assert ack['id'] == 'late', ack
            assert ack['result'] == {'revision':1,'tools':['startup_late']}, ack
            with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'acknowledged'), 'a') as trace:
                trace.write(str(generation) + '\n')
        send({'jsonrpc':'2.0','id':message['id'],'result':{'disposition':{'action':'continue'}}})
    elif message.get('method') == 'shutdown':
        send({'jsonrpc':'2.0','id':message['id'],'result':{}})
        break
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "startup-catalog"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "late-tools.py"
[contributes]
hooks = ["session_start", "session_end"]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(directory.path());
    config.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(directory.path(), manifest), config)
        .await
        .unwrap();
    let mut host = ExtensionHost::new();
    host.load(&process);
    host.finalize_tool_surface();
    let start = tokio::time::timeout(
        Duration::from_secs(2),
        process.start_session_hook_binding("session-owner"),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(directory.path().join("entered")).unwrap(),
        "1\n"
    );
    start
        .expect("session_start is waiting for the catalog ACK behind its own lifecycle lock")
        .unwrap();
    let old = read_std_lock(&process.inner.connection).clone();
    assert_eq!(old.catalog_revision.load(Ordering::Acquire), 1);
    assert_eq!(host.tool_definitions()[0].description, "Generation 1");
    let report = tokio::time::timeout(Duration::from_secs(3), process.reload())
        .await
        .expect("replacement session_start must also receive its catalog ACK")
        .unwrap();
    assert_eq!(report.generation, 2);
    assert_eq!(host.tool_definitions()[0].description, "Generation 2");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("acknowledged")).unwrap(),
        "1\n2\n"
    );

    // A queued mutation from the retired generation must not alter the new
    // catalog, even when its request id and mutation are otherwise valid.
    let current = read_std_lock(&process.inner.connection).clone();
    let (events, _events) = broadcast::channel(4);
    let (state, mut frames) = crate::extension_process::tests::protocol_read_state_for_test(
        ManifestContributions::default(),
        events,
    );
    let stale_id = ExtensionRequestId::String("stale".into());
    insert_child_request(&state, stale_id.clone(), None, None).unwrap();
    process
        .inner
        .catalog_updates
        .send(CatalogUpdateRequest {
            request_id: stale_id,
            generation: old.generation,
            catalog: Arc::clone(&old.tool_catalog),
            mutation: CatalogMutation::Unregister(vec!["startup_late".into()]),
            writer: state.writer.clone(),
            child_requests: Arc::clone(&state.child_requests),
            max_message_bytes: DEFAULT_EXTENSION_MESSAGE_BYTES,
        })
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
        .await
        .unwrap()
        .unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&frame.line).unwrap();
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("inactive extension generation"));
    assert_eq!(current.catalog_revision.load(Ordering::Acquire), 1);
    assert_eq!(host.tool_definitions()[0].description, "Generation 2");
    process
        .settle_session_hook_binding("session-owner", ExtensionLifecycleOutcome::Completed)
        .await
        .unwrap();
    // Publication without deliverable ACK must still withdraw and terminate,
    // never leave the extension and host with divergent accepted catalogs.
    process
        .inner
        .catalog_updates
        .send(CatalogUpdateRequest {
            request_id: ExtensionRequestId::String("already-settled".into()),
            generation: current.generation,
            catalog: Arc::clone(&current.tool_catalog),
            mutation: CatalogMutation::Unregister(vec!["startup_late".into()]),
            writer: state.writer.clone(),
            child_requests: Arc::clone(&state.child_requests),
            max_message_bytes: DEFAULT_EXTENSION_MESSAGE_BYTES,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !current.closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("undeliverable ACK must terminate its generation");
    assert!(host.tool_definitions().is_empty());
    assert!(read_std_lock(&current.tool_catalog).is_empty());
    process.shutdown().await;
}
