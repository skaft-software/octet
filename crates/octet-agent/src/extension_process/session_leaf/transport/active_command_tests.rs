//! Real adapter + FauxProvider coverage of commands sharing a response's writer.
use super::*;
use crate::extension_process::{pi_fixture, tests::trusted_descriptor};
use crate::{
    Agent, AgentConfig, AgentEvent, EffectBroker, ExtensionHost, FinishReason, SandboxConfig,
    Session,
};

const FACTORY: &str = r#"import fs from 'node:fs';
export default pi => {
  const trace = value => fs.appendFileSync('trace.txt', value + '\n');
  pi.registerCommand('append', { handler: async (_args, ctx) => {
    trace('append-start');
    for (let index = 0; index < 2; index++) {
      pi.appendEntry('busy-command', { index });
      const entry = ctx.sessionManager.getEntries().at(-1);
      if (entry.customType !== 'busy-command' || entry.data.index !== index)
        throw new Error('durable receipt missing');
    }
    trace('append-end');
    fs.writeFileSync('append-finished', '');
  }});
  pi.registerCommand('retire', { handler: async () => { trace('retire'); process.exit(0); }});
  pi.on('turn_end', async () => {
    trace('turn-end');
    if (fs.existsSync('hold-hook')) await new Promise(() => {});
    if (fs.existsSync('command-barrier')) {
      while (!fs.existsSync('append-finished')) await new Promise(resolve => setTimeout(resolve, 5));
      pi.appendEntry('observed-end', {});
    }
  });
};"#;

async fn fixture() -> (tempfile::TempDir, ExtensionProcess, Agent) {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("commands.mjs");
    std::fs::write(&factory, FACTORY).unwrap();
    let manifest_path = pi_fixture::capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.supervise = false;
    runtime.host_state.has_ui = Some(false);
    let process = ExtensionProcess::start(
        trusted_descriptor(manifest_path.parent().unwrap(), manifest),
        runtime,
    )
    .await
    .unwrap();
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    process
        .set_host_state_with_session(process.current_context().host, &session)
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
    process
        .start_session_hook_binding(session.resource_owner_key())
        .await
        .unwrap();
    let provider = octet_ai::FauxProvider::new(octet_ai::FauxOptions::default());
    provider.set_responses(vec![
        octet_ai::FauxResponse::Message(octet_ai::FauxMessage::new("answer")),
        octet_ai::FauxResponse::Message(octet_ai::FauxMessage::new("next answer")),
    ]);
    let client = octet_ai::AiClient::new();
    provider.register(&client);
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    let agent = Agent::new(AgentConfig {
        client,
        model: provider.model().clone(),
        session,
        extensions: host,
        system: "test".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    (temp, process, agent)
}

#[tokio::test]
async fn active_command_appends_finish_with_response_hooks_and_durable_receipts() {
    assert_active_appends(false).await;
}

#[tokio::test]
async fn active_command_appends_while_response_hook_waits_and_hook_can_append_afterwards() {
    assert_active_appends(true).await;
}

async fn assert_active_appends(during_hook: bool) {
    let (temp, process, mut agent) = fixture().await;
    if during_hook {
        std::fs::write(temp.path().join("command-barrier"), "").unwrap();
    }
    let context = process.current_context_for_resource_owner(agent.session().resource_owner_key());
    let mut events = process.subscribe();
    let mut run = agent.prompt("prompt").await.unwrap();
    if during_hook {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    event = run.next() => { assert!(event.is_some()); }
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {
                        if std::fs::read_to_string(temp.path().join("trace.txt")).unwrap_or_default().contains("turn-end") { break; }
                    }
                }
            }
        }).await.unwrap();
    } else {
        while !matches!(run.next().await, Some(AgentEvent::OutputDelta { .. })) {}
    }
    let command = tokio::spawn({
        let process = process.clone();
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
    assert!(run
        .queue_extension_append(
            process.clone(),
            id.clone(),
            generation + 1,
            entry_type.clone(),
            data.clone()
        )
        .is_err());
    run.queue_extension_append(process.clone(), id, generation, entry_type, data)
        .unwrap();
    let mut appends = 1;
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                event = events.recv() => {
                    if let ExtensionEvent::SessionEntryRequested { request_id, generation,
                        operation: ExtensionSessionEntryOperation::Append { entry_type, data }, .. } = event.unwrap() {
                        assert!(run.queue_extension_append(process.clone(), request_id.clone(), generation + 1, entry_type.clone(), data.clone()).is_err());
                        run.queue_extension_append(process.clone(), request_id, generation, entry_type, data).unwrap();
                        appends += 1;
                    }
                }
                event = run.next() => match event {
                    Some(AgentEvent::RunFinished { reason, .. }) => {
                        assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                        completed = true;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
        command.await.unwrap().unwrap();
    }).await.expect("command append must not wait for response settlement");
    assert!(completed);
    assert_eq!(appends, 2);
    drop(run);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("trace.txt")).unwrap(),
        if during_hook {
            "turn-end\nappend-start\nappend-end\n"
        } else {
            "append-start\nappend-end\nturn-end\n"
        }
    );
    if during_hook {
        std::fs::remove_file(temp.path().join("command-barrier")).unwrap();
    }
    assert!(process.is_running());
    assert_eq!(agent.complete("next").await.unwrap().text, "next answer");
    let reopened = Session::open_read_only(agent.session().path()).unwrap();
    let entries: Vec<_> = reopened
        .entries()
        .iter()
        .filter_map(|e| reopened.extension_entry(&e.id, "octet-pi-compat"))
        .collect();
    assert_eq!(
        entries.len(),
        if during_hook { 3 } else { 2 },
        "acknowledged command appends are never replayed"
    );
    if during_hook {
        assert_eq!(entries[2].entry_type, "observed-end");
    }
    assert_eq!(entries[0].data["index"], 0);
    assert_eq!(entries[1].data["index"], 1);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn command_process_retirement_during_response_hook_continues_without_replay() {
    let (temp, process, mut agent) = fixture().await;
    std::fs::write(temp.path().join("hold-hook"), "").unwrap();
    let context = process.current_context_for_resource_owner(agent.session().resource_owner_key());
    let mut run = agent.prompt("prompt").await.unwrap();
    let mut warnings = Vec::new();
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                event = run.next() => { assert!(event.is_some(), "response settled before hook barrier"); }
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    if std::fs::read_to_string(temp.path().join("trace.txt")).unwrap_or_default().contains("turn-end") { break; }
                }
            }
        }
        let command = tokio::spawn({
            let process = process.clone();
            async move { process.execute_command("retire", vec![], context).await }
        });
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed), "{reason:?}"); completed = true;
                }
                AgentEvent::ExtensionObservationWarning { message } => warnings.push(message),
                _ => {}
            }
        }
        assert!(command.await.unwrap().is_err());
    }).await.unwrap();
    assert!(completed);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("octet-pi-compat"));
    drop(run);
    assert_eq!(agent.complete("next").await.unwrap().text, "next answer");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("trace.txt")).unwrap(),
        "turn-end\nretire\n"
    );
    process.shutdown().await;
}

#[tokio::test]
async fn dropping_run_refuses_queued_command_append_without_idle_replay() {
    let (_temp, process, mut agent) = fixture().await;
    let context = process.current_context_for_resource_owner(agent.session().resource_owner_key());
    let mut events = process.subscribe();
    let mut run = agent.prompt("prompt").await.unwrap();
    while !matches!(run.next().await, Some(AgentEvent::OutputDelta { .. })) {}
    let command = tokio::spawn({
        let process = process.clone();
        async move { process.execute_command("append", vec![], context).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::SessionEntryRequested {
                request_id,
                generation,
                operation: ExtensionSessionEntryOperation::Append { entry_type, data },
                ..
            } = events.recv().await.unwrap()
            {
                run.queue_extension_append(
                    process.clone(),
                    request_id,
                    generation,
                    entry_type,
                    data,
                )
                .unwrap();
                break;
            }
        }
    })
    .await
    .unwrap();
    drop(run);
    assert!(tokio::time::timeout(Duration::from_secs(5), command)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(process.is_running());
    agent.complete("next").await.unwrap();
    let reopened = Session::open_read_only(agent.session().path()).unwrap();
    assert!(!reopened.entries().iter().any(|entry| reopened
        .extension_entry(&entry.id, "octet-pi-compat")
        .is_some()));
    assert!(process.shutdown().await);
}
