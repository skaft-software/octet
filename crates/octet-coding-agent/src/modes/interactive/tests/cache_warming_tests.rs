//! Real idle-input-owner coverage with a virtual clock and no provider network.

use super::support::*;
use super::*;
use octet_agent::{Agent, AgentConfig, CacheWarmMode, UsageRecordKind};
use octet_ai::{
    AiClient, AiError, AssistantMessage, AssistantPart, Diagnostic, HostStreamModel,
    HostStreamTransport, ModelCatalog, Response, ResponseStream, StopReason, StreamEvent,
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct WarmTransport {
    calls: AtomicUsize,
    pending_warm: bool,
}

#[async_trait::async_trait]
impl HostStreamTransport for WarmTransport {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: octet_ai::Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let warm = request.max_output_tokens == Some(1);
        if warm && self.pending_warm {
            return Ok(Box::pin(tokio_stream::StreamExt::chain(
                tokio_stream::iter([Ok(StreamEvent::Started { response_id: None })]),
                tokio_stream::pending(),
            )));
        }
        let usage = octet_ai::Usage {
            cache_read_tokens: 500_000,
            output_tokens: if warm { 1 } else { 10 },
            total_tokens: if warm { 500_001 } else { 500_010 },
            ..Default::default()
        };
        let cost = model.pricing.as_ref().map(|pricing| {
            octet_ai::pricing::cost_of(pricing, &usage).expect("priced fixture usage")
        });
        let response = Response {
            message: AssistantMessage {
                content: vec![AssistantPart::Text(
                    if warm {
                        "PRIVATE WARM OUTPUT"
                    } else {
                        "real answer"
                    }
                    .into(),
                )],
                model: model.id,
                protocol: model.protocol,
            },
            stop_reason: StopReason::EndTurn,
            usage,
            cost,
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: Vec::new(),
        };
        let text = if warm {
            "PRIVATE WARM OUTPUT"
        } else {
            "real answer"
        };
        Ok(Box::pin(tokio_stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::TextStart { index: 0 }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                delta: text.into(),
            }),
            Ok(StreamEvent::TextEnd { index: 0 }),
            Ok(StreamEvent::Finished(response)),
        ])))
    }
}

async fn settled_agent(pending_warm: bool) -> (tempfile::TempDir, Agent, Arc<WarmTransport>) {
    let workspace = tempfile::tempdir().unwrap();
    let mut model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("claude-sonnet-4-6".into()))
        .unwrap();
    // The idle profile needs a large economically material prefix. Declare the
    // fixture's capacity explicitly rather than depending on a catalog limit.
    Arc::make_mut(&mut model.spec).limits.context_window = 1_000_000;
    let transport = Arc::new(WarmTransport {
        calls: AtomicUsize::new(0),
        pending_warm,
    });
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), transport.clone());
    let mut session = Session::create(workspace.path().join("idle-warming.jsonl")).unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("seed task".into())],
            },
        )))
        .unwrap();
    let entry = session
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            AssistantMessage {
                content: vec![AssistantPart::Text("seed answer".into())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            },
        )))
        .unwrap();
    let usage = octet_ai::Usage {
        cache_write_tokens: 500_000,
        output_tokens: 10,
        total_tokens: 500_010,
        ..Default::default()
    };
    session
        .record_assistant_usage_with_stop_reason(
            entry,
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            usage,
            Some(octet_ai::pricing::cost_of(model.spec.pricing.as_ref().unwrap(), &usage).unwrap()),
            StopReason::EndTurn,
        )
        .unwrap();
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        system: "instruction ".repeat(10_000),
        sandbox: octet_agent::SandboxConfig::new(workspace.path()),
        effect_broker: octet_agent::EffectBroker::new(octet_agent::EffectPolicy::Controlled),
        extensions: octet_agent::ExtensionHost::new(),
        max_turns: Some(4),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent.set_cache_warming_mode(CacheWarmMode::Idle).unwrap();
    let output = agent.complete("actual task").await.unwrap();
    assert_eq!(output.text, "real answer");
    (workspace, agent, transport)
}

async fn idle_for_285_seconds(agent: &mut Agent) -> Idle {
    let mut shell = InteractiveShell::test_shell();
    shell.extension_set_editor("next real prompt".into());
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(285)).await;
        sender
            .send(Ok(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
    });
    let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
    let mut scroll = tokio::time::interval(Duration::from_millis(16));
    let mut extensions_tick = tokio::time::interval(Duration::from_millis(50));
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let (watcher, mut reload, mut reload_tick) = test_reload();
    wait_for_prompt(
        &mut shell,
        &mut input,
        &mut scroll,
        &mut extensions_tick,
        &mut extensions,
        None,
        &mut reload_tick,
        &watcher,
        &mut reload,
        Some(agent),
    )
    .await
    .unwrap()
}

fn exact_cost(session: &Session) -> u128 {
    u128::from(session.total_cost_microdollars()) * 1_000_000
        + u128::from(session.total_cost_picodollars_remainder())
}

#[tokio::test(start_paused = true)]
async fn idle_input_owner_survives_repeated_select_cancellation_and_accounts_refresh() {
    let (_workspace, mut agent, transport) = settled_agent(false).await;
    let context = agent.session().context().unwrap();
    let head = agent.session().head_ref().cloned();
    let cost = exact_cost(agent.session());
    let output_tokens = agent
        .session()
        .usage_records()
        .iter()
        .map(|record| record.usage.output_tokens)
        .sum::<u64>();
    let start = tokio::time::Instant::now();
    assert!(matches!(
        idle_for_285_seconds(&mut agent).await,
        Idle::Submit(_)
    ));
    assert_eq!(
        tokio::time::Instant::now() - start,
        Duration::from_secs(285)
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(agent.session().head_ref(), head.as_ref());
    assert_eq!(
        serde_json::to_value(agent.session().context().unwrap()).unwrap(),
        serde_json::to_value(context).unwrap()
    );
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::CacheWarm))
            .count(),
        1
    );
    assert!(exact_cost(agent.session()) > cost);
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .map(|record| record.usage.output_tokens)
            .sum::<u64>(),
        output_tokens + 1
    );
    let reopened = Session::open(agent.session().path()).unwrap();
    assert_eq!(exact_cost(&reopened), exact_cost(agent.session()));
    assert!(!std::fs::read_to_string(reopened.path())
        .unwrap()
        .contains("PRIVATE WARM OUTPUT"));
}

#[tokio::test(start_paused = true)]
async fn idle_input_remains_responsive_during_a_pending_refresh_and_mode_off_settles_uncertainty() {
    let (_workspace, mut agent, transport) = settled_agent(true).await;
    let start = tokio::time::Instant::now();
    assert!(matches!(
        idle_for_285_seconds(&mut agent).await,
        Idle::Submit(_)
    ));
    assert_eq!(
        tokio::time::Instant::now() - start,
        Duration::from_secs(285)
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    agent.set_cache_warming_mode(CacheWarmMode::Off).unwrap();
    assert!(agent.session().has_uncertain_usage());
    assert!(Session::open(agent.session().path())
        .unwrap()
        .has_uncertain_usage());
}

#[tokio::test]
async fn active_cache_warming_report_uses_the_live_control_not_the_launch_snapshot() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let mut inspection = ActiveRunInspection::capture(&app);
    // Do not poll the provider stream: this is a local control/report test.
    let run = app.agent.prompt("inspect the active policy").await.unwrap();
    let control = run.control();
    control.set_cache_warming_mode(CacheWarmMode::Off).unwrap();
    inspection.cache_warming_control = Some(control);
    let session = inspection.read_only_session().unwrap();
    let text = inspection.cache_warming_text(&session);
    assert!(text.contains("Mode: off"), "{text}");
    assert!(text.contains("cache warming disabled"), "{text}");
    assert!(!text.contains("snapshot at run start"));
    let mut shell = InteractiveShell::test_shell();
    let (queue, quit_requested) =
        run_active_command(&mut shell, Command::CacheWarming(None), &inspection).await;
    assert!(shell.has_overlay());
    assert!(queue.is_empty());
    assert!(!quit_requested);
    assert_eq!(shell.debug_error(), None);
    drop(run);
}
