//! A retired append lane must not trap the next prompt or replay a callback.
use super::super::tests::{trusted_descriptor, write_executable_script};
use super::tests::Capture;
use super::*;
use crate::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
use octet_ai::{CacheRetention, ModelCatalog, ModelId, ReasoningMode};

#[tokio::test]
async fn retired_context_process_continues_and_new_generation_appends_once() {
    assert_context_recovery(false, false).await;
}

#[tokio::test]
async fn terminated_context_process_continues_then_reload_appends_once() {
    assert_context_recovery(true, false).await;
}

#[tokio::test]
async fn terminated_process_observations_continue_then_reload_appends_once() {
    assert_context_recovery(true, true).await;
}

async fn assert_context_recovery(terminate: bool, observers: bool) {
    let temp = tempfile::tempdir().unwrap();
    write_executable_script(
        &temp.path().join("extension.py"),
        r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
def send(value): print(json.dumps(value), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    if method == 'initialize':
        send({'jsonrpc':'2.0','id':m['id'],'result':{'api_version':'0.4','tools':[],'commands':[],
          'protocol':{'version':'0.4','features':m['params']['protocol']['required_features']+['session_entries'],
            'limits':{'max_concurrent_requests':1}}}})
    elif method == 'shutdown':
        send({'jsonrpc':'2.0','id':m['id'],'result':{}})
        break
    elif method == 'hook/run':
        p = m['params']; grant = p['session_leaf']
        with Path('calls.jsonl').open('a') as f: f.write(json.dumps(p['context']['resource_owner'])+'\n')
        request_id = 'append-' + str(m['id'])
        send({'jsonrpc':'2.0','id':request_id,'method':'session/append_entry','params':{
          'parent_request_id':m['id'],'entry_type':'checkpoint',
          'data':{'generation':grant['owner']['process_generation']},
          'session_leaf':{k:grant[k] for k in ['grant_id','activation_epoch','operation_id']}}})
        while True:
            receipt = json.loads(sys.stdin.readline())
            if receipt.get('id') == request_id: break
        assert 'result' in receipt, receipt
        send({'jsonrpc':'2.0','id':m['id'],'result':{}})
"#,
    );
    let manifest_source = r#"name = "recovery-process"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
hooks = ["provider_context"]
"#;
    let manifest_source = if observers {
        manifest_source.replace(
            "hooks = [\"provider_context\"]",
            r#"hooks = ["model_turn_start", "model_turn_end"]"#,
        )
    } else {
        manifest_source.to_owned()
    };
    let manifest = ExtensionManifest::parse(&manifest_source).unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), runtime)
        .await
        .unwrap();
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let client = octet_ai::AiClient::new();
    let captures = Arc::new(StdMutex::new(Vec::new()));
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(temp.path().join("session.jsonl")).unwrap(),
        extensions: host,
        system: "canonical system".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    assert_eq!(agent.complete("first").await.unwrap().text, "answer");
    let prefix = std::fs::read(agent.session().path()).unwrap();
    if terminate {
        let connection = read_std_lock(&process.inner.connection).clone();
        connection.terminate().await;
    } else {
        assert!(process.begin_drain());
    }
    let mut warnings = Vec::new();
    let mut run = agent.prompt("during restart").await.unwrap();
    while let Some(event) = run.next().await {
        match event {
            crate::AgentEvent::ExtensionObservationWarning { message } => warnings.push(message),
            crate::AgentEvent::RunFinished { reason, .. } => assert!(
                matches!(reason, crate::FinishReason::Completed),
                "{reason:?}"
            ),
            _ => {}
        }
    }
    drop(run);
    assert_eq!(
        warnings.len(),
        if observers { 2 } else { 1 },
        "{warnings:?}"
    );
    for warning in &warnings {
        assert!(warning.contains("recovery-process"), "{warning}");
        assert!(
            !warning.contains("observer 1") && !warning.contains("turn 0"),
            "{warning}"
        );
    }
    let report = process.reload().await.unwrap();
    assert_eq!(report.generation, 2);
    assert_eq!(
        agent.complete("after restart").await.unwrap().text,
        "answer"
    );
    assert_eq!(captures.lock().unwrap().len(), 3);
    let calls: Vec<serde_json::Value> = std::fs::read_to_string(temp.path().join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let per_generation = if observers { 2 } else { 1 };
    assert_eq!(
        calls.len(),
        per_generation * 2,
        "completed callbacks are never replayed"
    );
    assert_eq!(calls[0]["process_generation"], 1);
    assert_eq!(calls[per_generation]["process_generation"], 2);
    assert_eq!(
        calls[0]["extension_instance_id"],
        calls[1]["extension_instance_id"]
    );
    assert!(std::fs::read(agent.session().path())
        .unwrap()
        .starts_with(&prefix));
    let reopened = Session::open_read_only(agent.session().path()).unwrap();
    let checkpoints: Vec<_> = reopened
        .entries()
        .iter()
        .filter_map(|entry| reopened.extension_entry(&entry.id, "recovery-process"))
        .collect();
    assert_eq!(checkpoints.len(), per_generation * 2);
    assert_eq!(checkpoints[0].data["generation"], 1);
    assert_eq!(checkpoints[per_generation].data["generation"], 2);
    assert!(process.shutdown().await);
}
