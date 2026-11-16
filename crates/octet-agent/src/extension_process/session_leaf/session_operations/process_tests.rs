//! Native session-operation observations through the actual Pi adapter.
use super::*;
use crate::compaction::run_session_operation_hooks;
use crate::extension_process::pi_fixture;
use crate::session::EntryValue;
use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol};

#[tokio::test]
async fn pi_turn_observers_append_durable_entries_once() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("observers.mjs");
    std::fs::write(
        &factory,
        r#"export default pi => {
  pi.on('turn_start', event => pi.appendEntry('observed-start', { index: event.turnIndex }));
  pi.on('turn_end', event => pi.appendEntry('observed-end', { index: event.turnIndex }));
};"#,
    )
    .unwrap();
    let faulty = temp.path().join("faulty.mjs");
    std::fs::write(
        &faulty,
        r#"export default pi => {
  for (const [event, code] of [['turn_start', -32002], ['turn_end', -32800]]) {
    pi.on(event, () => {
      pi.appendEntry('before-fault', { event });
      throw Object.assign(new Error('private callback detail'), { code });
    });
  }
};"#,
    )
    .unwrap();
    let manifest_path = pi_fixture::capture(temp.path(), &[faulty, factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.supervise = false;
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();
    let mut session = Session::create(temp.path().join("session.jsonl")).unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("prompt".into())],
            },
        )))
        .unwrap();
    let hooks: Vec<Arc<dyn SessionOperationHook>> = vec![Arc::new(process.clone())];
    let cancellation = CancellationToken::default();
    let start = SessionOperation::ModelTurnStart {
        run_id: "run:observations".into(),
        turn_index: 0,
        timestamp_ms: 1,
    };
    let result = run_session_operation_hooks(
        &mut session,
        &hooks,
        &start,
        &cancellation,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        matches!(result, Ok(SessionOperationDecision::Continue)),
        "start: {result:?}"
    );
    let assistant = session
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            AssistantMessage {
                model: ModelId("gpt-4o-mini".into()),
                protocol: Protocol::OpenAiChat,
                content: vec![AssistantPart::Text("answer".into())],
            },
        )))
        .unwrap();
    let end = SessionOperation::ModelTurnEnd {
        run_id: "run:observations".into(),
        turn_index: 0,
        timestamp_ms: 2,
        assistant_entry: session.entry(&assistant).unwrap().clone(),
        assistant_metadata: None,
        tool_result_entries: vec![],
    };
    let result = run_session_operation_hooks(
        &mut session,
        &hooks,
        &end,
        &cancellation,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        matches!(result, Ok(SessionOperationDecision::Continue)),
        "end: {result:?}"
    );
    // Exercise the actual model-turn warning boundary, not just its operation
    // driver. Both later factories and subsequent prompts must keep running.
    let provider = octet_ai::FauxProvider::new(octet_ai::FauxOptions::default());
    provider.set_responses(vec![
        octet_ai::FauxResponse::Message(octet_ai::FauxMessage::new("answer one")),
        octet_ai::FauxResponse::Message(octet_ai::FauxMessage::new("answer two")),
    ]);
    let client = octet_ai::AiClient::new();
    provider.register(&client);
    let mut host = crate::ExtensionHost::new();
    process.register(&mut host);
    let mut agent = crate::Agent::new(crate::AgentConfig {
        client,
        model: provider.model().clone(),
        session,
        extensions: host,
        system: "test".into(),
        sandbox: crate::SandboxConfig::new(temp.path()),
        effect_broker: crate::EffectBroker::default(),
        max_turns: Some(1),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    for _ in 0..2 {
        let mut run = agent.prompt("continue").await.unwrap();
        while let Some(event) = run.next().await {
            match event {
                crate::AgentEvent::ExtensionObservationWarning { message } => {
                    panic!("callback fault escaped factory isolation: {message}")
                }
                crate::AgentEvent::RunFinished { reason, .. } => assert!(
                    matches!(reason, crate::FinishReason::Completed),
                    "{reason:?}"
                ),
                _ => {}
            }
        }
    }
    assert_eq!(provider.state().call_count, 2);
    let reopened = Session::open_read_only(agent.session().path()).unwrap();
    let entries: Vec<_> = reopened
        .entries()
        .iter()
        .filter_map(|entry| reopened.extension_entry(&entry.id, "octet-pi-compat"))
        .collect();
    assert_eq!(
        entries.len(),
        12,
        "acknowledged effects survive callback faults without retries"
    );
    for turn in entries.chunks_exact(4) {
        assert_eq!(turn[0].entry_type, "before-fault");
        assert_eq!(turn[1].entry_type, "observed-start");
        assert_eq!(turn[2].entry_type, "before-fault");
        assert_eq!(turn[3].entry_type, "observed-end");
    }
    assert!(process.shutdown().await);
}
