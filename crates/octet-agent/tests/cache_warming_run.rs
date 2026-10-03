//! Virtual-clock coverage of cache warming inside the actual agent run loop.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use octet_agent::{
    Agent, AgentConfig, AgentEvent, CacheWarmMode, CacheWarmRecord, CacheWarmState,
    CacheWarmingPhase, CacheWarmingState, EffectBroker, EffectPolicy, EntryValue, EventObserver,
    ExtensionHost, FinishReason, RunControl, SandboxConfig, Session, Tool, ToolConcurrency,
    ToolContext, ToolEffect, ToolError, ToolOutput, UsageRecordKind,
};
use octet_ai::{
    AiClient, AiError, AssistantMessage, AssistantPart, CacheRetention, Diagnostic,
    HostStreamModel, HostStreamTransport, Message, Model, ModelCatalog, ModelId, ReasoningConfig,
    ReasoningMode, Request, Response, ResponseStream, StopReason, StreamEvent, ToolCall,
    ToolCallId, ToolDef, Usage, UserMessage, UserPart,
};
use tokio::time::Instant;

const TOOL_NAME: &str = "long_observation";
const WARM_OUTPUT: &str = "private warm output, never part of the conversation";
const DELAY: Duration = Duration::from_secs(270);

#[derive(Clone, Copy)]
enum Hold {
    Opening,
    Body,
    Tool,
}

struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Observed {
    warmed: AtomicUsize,
    uncertain: AtomicUsize,
}

struct Observer(Arc<Observed>);

impl EventObserver for Observer {
    fn on_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::CacheWarmed { .. } => {
                self.0.warmed.fetch_add(1, Ordering::SeqCst);
            }
            AgentEvent::ProviderUsageUncertain => {
                self.0.uncertain.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

struct LongObservation {
    duration: Duration,
    parallel: bool,
    terminate: bool,
    executions: Arc<AtomicUsize>,
    self_cancel: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for LongObservation {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: TOOL_NAME.into(),
            description: "A long pure observation".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            async_execution: false,
            constrained_sampling: None,
        }
    }

    fn concurrency(&self) -> ToolConcurrency {
        if self.parallel {
            ToolConcurrency::Parallel
        } else {
            ToolConcurrency::Sequential
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.duration).await;
        if self.self_cancel.load(Ordering::SeqCst) {
            context.cancellation.cancel();
            std::future::pending::<()>().await;
        }
        let output = ToolOutput::new("real tool result");
        Ok(if self.terminate {
            output.requesting_termination()
        } else {
            output
        })
    }
}

struct Transport {
    hold: Hold,
    duration: Duration,
    tool_calls: usize,
    pending_warm: bool,
    main_calls: AtomicUsize,
    requests: Mutex<Vec<(Instant, Request)>>,
    warm_dropped: Arc<AtomicUsize>,
    self_cancel_tool: Arc<AtomicBool>,
    self_cancel_main: Mutex<Option<RunControl>>,
}

fn usage(warm: bool) -> Usage {
    if warm {
        Usage {
            cache_read_tokens: 30_000,
            output_tokens: 1,
            total_tokens: 30_001,
            ..Usage::default()
        }
    } else {
        Usage {
            cache_write_tokens: 30_000,
            output_tokens: 10,
            total_tokens: 30_010,
            ..Usage::default()
        }
    }
}

fn stream(
    model: HostStreamModel,
    content: Vec<AssistantPart>,
    stop_reason: StopReason,
    warm: bool,
    delay: Duration,
    self_cancel: Option<RunControl>,
) -> ResponseStream {
    Box::pin(async_stream::stream! {
        if !delay.is_zero() { tokio::time::sleep(delay).await; }
        if let Some(control) = self_cancel {
            control.abort();
            std::future::pending::<()>().await;
        }
        yield Ok(StreamEvent::Started { response_id: None });
        for (index, part) in content.iter().enumerate() {
            match part {
                AssistantPart::Text(text) => {
                    yield Ok(StreamEvent::TextStart { index });
                    yield Ok(StreamEvent::TextDelta { index, delta: text.clone() });
                    yield Ok(StreamEvent::TextEnd { index });
                }
                AssistantPart::ToolCall(call) => {
                    yield Ok(StreamEvent::ToolCallStart {
                        index, async_execution: false, id: call.id.clone(), name: call.name.clone(),
                    });
                    yield Ok(StreamEvent::ToolCallArgsDelta { index, delta: call.arguments_json.clone() });
                    yield Ok(StreamEvent::ToolCallEnd { index, argument_error: None });
                }
                _ => unreachable!("fixture only produces text and complete tool calls"),
            }
        }
        yield Ok(StreamEvent::Finished(Response {
            message: AssistantMessage { content, model: model.id, protocol: model.protocol },
            stop_reason,
            usage: usage(warm),
            cost: None,
            response_id: None,
            responses_output: None,
            deferred: None,
                inference: None,
            diagnostics: Vec::new(),
        }));
    })
}

#[async_trait::async_trait]
impl HostStreamTransport for Transport {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        let warm = request.max_output_tokens == Some(1);
        self.requests
            .lock()
            .unwrap()
            .push((Instant::now(), request));
        if warm && self.pending_warm {
            let dropped = self.warm_dropped.clone();
            return Ok(Box::pin(async_stream::stream! {
                let _guard = DropCounter(dropped);
                yield Ok(StreamEvent::Started { response_id: None });
                std::future::pending::<()>().await;
            }));
        }
        if warm {
            return Ok(stream(
                model,
                vec![AssistantPart::Text(WARM_OUTPUT.into())],
                StopReason::EndTurn,
                true,
                Duration::ZERO,
                None,
            ));
        }
        let index = self.main_calls.fetch_add(1, Ordering::SeqCst);
        let self_cancel = self.self_cancel_main.lock().unwrap().clone();
        if matches!(self.hold, Hold::Opening) {
            tokio::time::sleep(self.duration).await;
            if let Some(control) = self_cancel.as_ref() {
                control.abort();
                std::future::pending::<()>().await;
            }
        }
        let (content, stop_reason) = if matches!(self.hold, Hold::Tool) && index == 0 {
            let content = (0..self.tool_calls)
                .map(|index| {
                    AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId(format!("observation-{index}")),
                        name: TOOL_NAME.into(),
                        arguments_json: "{}".into(),
                        async_execution: false,
                        argument_error: None,
                    })
                })
                .collect();
            (content, StopReason::ToolUse)
        } else {
            (
                vec![AssistantPart::Text("real answer".into())],
                StopReason::EndTurn,
            )
        };
        Ok(stream(
            model,
            content,
            stop_reason,
            false,
            if matches!(self.hold, Hold::Body) {
                self.duration
            } else {
                Duration::ZERO
            },
            if matches!(self.hold, Hold::Body) {
                self_cancel
            } else {
                None
            },
        ))
    }
}

fn fixture(
    hold: Hold,
    duration: Duration,
    tool_calls: usize,
    pending_warm: bool,
    terminate: bool,
) -> (
    Agent,
    Arc<Transport>,
    Arc<Observed>,
    Arc<AtomicUsize>,
    tempfile::TempDir,
) {
    let model: Model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("claude-sonnet-4-6".into()))
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut session = Session::create(workspace.path().join("cache-run.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("seed task".into())],
        })))
        .unwrap();
    let entry = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("seed answer".into())],
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
        })))
        .unwrap();
    session
        .record_assistant_usage_with_stop_reason(
            entry,
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            usage(false),
            Some(
                octet_ai::pricing::cost_of(model.spec.pricing.as_ref().unwrap(), &usage(false))
                    .unwrap(),
            ),
            StopReason::EndTurn,
        )
        .unwrap();
    let transport = Arc::new(Transport {
        hold,
        duration,
        tool_calls,
        pending_warm,
        main_calls: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
        warm_dropped: Arc::new(AtomicUsize::new(0)),
        self_cancel_tool: Arc::new(AtomicBool::new(false)),
        self_cancel_main: Mutex::new(None),
    });
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), transport.clone());
    let observed = Arc::new(Observed::default());
    let executions = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.observe(Observer(observed.clone()));
    extensions.tool(LongObservation {
        duration,
        parallel: tool_calls > 1,
        terminate,
        executions: executions.clone(),
        self_cancel: transport.self_cancel_tool.clone(),
    });
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        extensions,
        system: "Preserve this exact system and tool surface in the warm snapshot.".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("cache-run-affinity".into()),
    })
    .unwrap();
    agent
        .set_cache_warming_mode(CacheWarmMode::Streaming)
        .unwrap();
    (agent, transport, observed, executions, workspace)
}

async fn drive(agent: &mut Agent) -> Vec<AgentEvent> {
    let mut run = agent
        .prompt("actual task, not a synthetic warm suffix")
        .await
        .unwrap();
    let mut events = Vec::new();
    loop {
        let before = run.context_snapshot();
        let Some(event) = run.next().await else {
            break;
        };
        if matches!(event, AgentEvent::CacheWarmed { .. }) {
            assert_eq!(
                run.context_snapshot(),
                before,
                "warming must not change context, response state, or assistant-run usage"
            );
        }
        events.push(event);
    }
    events
}

fn assert_snapshot(transport: &Transport, expected_warms: usize) {
    let requests = transport.requests.lock().unwrap();
    let (opened_at, original) = &requests[0];
    assert_ne!(original.max_output_tokens, Some(1));
    let mut expected = serde_json::to_value(original).unwrap();
    expected["max_output_tokens"] = serde_json::json!(1);
    let warms: Vec<_> = requests
        .iter()
        .filter(|(_, request)| request.max_output_tokens == Some(1))
        .collect();
    assert_eq!(warms.len(), expected_warms);
    for (index, (at, request)) in warms.into_iter().enumerate() {
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            expected,
            "only the output cap may change; no new transcript or synthetic suffix"
        );
        assert_eq!(*at - *opened_at, DELAY * (index as u32 + 1));
    }
}

fn assert_isolated(agent: &Agent, events: &[AgentEvent], warm_count: usize) {
    let context = serde_json::to_value(agent.session().context().unwrap())
        .unwrap()
        .to_string();
    assert!(!context.contains(WARM_OUTPUT));
    assert!(!events.iter().any(
        |event| matches!(event, AgentEvent::OutputDelta { text, .. } if text.contains(WARM_OUTPUT))
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::CacheWarmed { .. }))
            .count(),
        warm_count
    );
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::CacheWarm))
            .count(),
        warm_count
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFinished {
            reason: FinishReason::Completed,
            ..
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn long_sequential_tool_warms_original_request_without_context_contamination() {
    let (mut agent, transport, observed, executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(600), 1, false, false);
    let started = Instant::now();
    let events = drive(&mut agent).await;
    assert_eq!(Instant::now() - started, Duration::from_secs(600));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(transport.main_calls.load(Ordering::SeqCst), 2);
    assert_snapshot(&transport, 2);
    assert_isolated(&agent, &events, 2);
    assert_eq!(observed.warmed.load(Ordering::SeqCst), 2);
    assert!(!agent.session().has_uncertain_usage());
}

#[tokio::test(start_paused = true)]
async fn parallel_wave_keeps_warming_and_preserves_ordered_real_results() {
    let (mut agent, transport, observed, executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(600), 2, false, false);
    let started = Instant::now();
    let events = drive(&mut agent).await;
    assert_eq!(Instant::now() - started, Duration::from_secs(600));
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_snapshot(&transport, 2);
    assert_isolated(&agent, &events, 2);
    let ids: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { id, .. } => Some(id.0.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["observation-0", "observation-1"]);
    assert_eq!(observed.warmed.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn opening_wakes_do_not_drop_or_replay_the_main_post() {
    let (mut agent, transport, _observed, _executions, _workspace) =
        fixture(Hold::Opening, Duration::from_secs(600), 0, false, false);
    let events = drive(&mut agent).await;
    assert_eq!(transport.main_calls.load(Ordering::SeqCst), 1);
    assert_snapshot(&transport, 2);
    assert_isolated(&agent, &events, 2);
}

#[tokio::test(start_paused = true)]
async fn inference_body_warms_the_request_without_inserting_provider_output() {
    let (mut agent, transport, _observed, _executions, _workspace) =
        fixture(Hold::Body, Duration::from_secs(600), 0, false, false);
    let events = drive(&mut agent).await;
    assert_eq!(transport.main_calls.load(Ordering::SeqCst), 1);
    assert_snapshot(&transport, 2);
    assert_isolated(&agent, &events, 2);
}

#[tokio::test(start_paused = true)]
async fn opening_and_body_reserve_both_main_and_warm_against_the_cost_ceiling() {
    for hold in [Hold::Opening, Hold::Body] {
        let (mut agent, transport, _observed, _executions, _workspace) =
            fixture(hold, Duration::from_secs(600), 0, false, false);
        // The known seed plus either request's reservation fits individually,
        // but the seed plus BOTH the main and warm reservations does not.
        agent.set_max_session_cost_microdollars(Some(1_300_000));
        let events = drive(&mut agent).await;
        assert_eq!(transport.main_calls.load(Ordering::SeqCst), 1);
        assert_snapshot(&transport, 0);
        assert_isolated(&agent, &events, 0);
        assert!(agent.session().cache_warm_records().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn timer_age_starts_at_actual_open_not_turn_started_suspension() {
    let (mut agent, transport, _observed, _executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(600), 1, false, false);
    let started = Instant::now();
    let mut run = agent
        .prompt("slow consumer before request opening")
        .await
        .unwrap();
    let first = run.next().await.unwrap();
    assert!(matches!(first, AgentEvent::TurnStarted));
    assert!(transport.requests.lock().unwrap().is_empty());
    tokio::time::advance(Duration::from_secs(250)).await;
    let mut events = vec![first];
    while let Some(event) = run.next().await {
        events.push(event);
    }
    drop(run);
    assert_eq!(Instant::now() - started, Duration::from_secs(850));
    assert_eq!(
        transport.requests.lock().unwrap()[0].0 - started,
        Duration::from_secs(250)
    );
    assert_snapshot(&transport, 2);
    assert_isolated(&agent, &events, 2);
}

#[tokio::test(start_paused = true)]
async fn completion_cancels_pending_warm_without_waiting_for_its_deadline() {
    let (mut agent, transport, observed, _executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(280), 1, true, true);
    let started = Instant::now();
    let events = drive(&mut agent).await;
    assert_eq!(Instant::now() - started, Duration::from_secs(280));
    assert_snapshot(&transport, 1);
    assert_isolated(&agent, &events, 0);
    assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 1);
    assert!(agent.session().has_uncertain_usage());
    assert_eq!(observed.uncertain.load(Ordering::SeqCst), 1);
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ProviderUsageUncertain)));
}

#[tokio::test(start_paused = true)]
async fn abort_cancels_pending_warm_and_tool_without_blocking() {
    let (mut agent, transport, observed, _executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(600), 1, true, false);
    let started = Instant::now();
    let mut run = agent.prompt("abort this long task").await.unwrap();
    let control = run.control();
    let abort = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(271)).await;
        control.abort();
    });
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    drop(run);
    abort.await.unwrap();
    assert_eq!(Instant::now() - started, Duration::from_secs(271));
    assert_snapshot(&transport, 1);
    assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(observed.uncertain.load(Ordering::SeqCst), 1);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFinished {
            reason: FinishReason::Aborted,
            ..
        })
    ));
    assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn driven_abort_transitions_idle_mode_without_waiting_for_pending_warm() {
    let (mut agent, transport, _observed, _executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(600), 1, true, false);
    agent.set_cache_warming_mode(CacheWarmMode::Idle).unwrap();
    let started = Instant::now();
    let mut run = agent
        .prompt("abort but retain the idle cache prefix")
        .await
        .unwrap();
    let control = run.control();
    let abort = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(271)).await;
        control.abort();
    });
    while run.next().await.is_some() {}
    drop(run);
    abort.await.unwrap();
    assert_eq!(Instant::now() - started, Duration::from_secs(271));
    assert_snapshot(&transport, 1);
    assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 0);
    let status = agent.cache_warming_status();
    assert_eq!(status.state, CacheWarmingState::Refreshing);
    assert_eq!(status.decision.unwrap().phase, CacheWarmingPhase::Idle);
    agent.set_cache_warming_mode(CacheWarmMode::Off).unwrap();
    assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn tool_poll_abort_preempts_due_warm_in_the_same_select_poll() {
    for calls in [1, 2] {
        let (mut agent, transport, _observed, _executions, _workspace) =
            fixture(Hold::Tool, DELAY, calls, false, false);
        transport.self_cancel_tool.store(true, Ordering::SeqCst);
        let events = drive(&mut agent).await;
        assert_snapshot(&transport, 0);
        assert!(agent.session().cache_warm_records().is_empty());
        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunFinished {
                reason: FinishReason::Aborted,
                ..
            })
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn provider_poll_abort_preempts_due_warm_in_the_same_select_poll() {
    for hold in [Hold::Opening, Hold::Body] {
        let (mut agent, transport, _observed, _executions, _workspace) =
            fixture(hold, DELAY, 0, false, false);
        let mut run = agent
            .prompt("provider synchronously cancels at the warm deadline")
            .await
            .unwrap();
        *transport.self_cancel_main.lock().unwrap() = Some(run.control());
        let mut events = Vec::new();
        while let Some(event) = run.next().await {
            events.push(event);
        }
        drop(run);
        assert_snapshot(&transport, 0);
        assert!(agent.session().cache_warm_records().is_empty());
        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunFinished {
                reason: FinishReason::Aborted,
                ..
            })
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn resumed_unfinished_warm_does_not_fail_a_long_real_request() {
    let (mut agent, transport, _observed, _executions, _workspace) =
        fixture(Hold::Body, Duration::from_secs(600), 0, false, false);
    let model = agent.model().clone();
    let anchor = agent.session().head();
    use std::io::Write;
    let record = octet_agent::SessionRecord::CacheWarm {
        record: CacheWarmRecord {
            attempt: 1,
            endpoint: model.endpoint.id.clone(),
            model: model.spec.id.clone(),
            state: CacheWarmState::Started,
            at_unix_ms: 0,
            anchor,
            extension_override: false,
        },
    };
    // Simulate a process that died immediately after persisting admission.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(agent.session().path())
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&record).unwrap()).unwrap();
    let resumed = Session::open(agent.session().path()).unwrap();
    agent.replace_session_at_idle(resumed).unwrap();
    let events = drive(&mut agent).await;
    assert_snapshot(&transport, 0);
    assert_isolated(&agent, &events, 0);
    assert_eq!(transport.main_calls.load(Ordering::SeqCst), 1);
    assert!(agent.session().has_uncertain_usage());
    assert!(agent.session().usage_uncertainty_exposure().is_none());
    assert_eq!(agent.session().cache_warm_records().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn mode_change_reports_cancellation_write_failure_and_retains_exposure() {
    let (mut agent, transport, _observed, _executions, _workspace) =
        fixture(Hold::Tool, Duration::from_secs(600), 1, true, false);
    agent.set_cache_warming_mode(CacheWarmMode::Idle).unwrap();
    let mut run = agent
        .prompt("cancel warm on a lost append fence")
        .await
        .unwrap();
    let control = run.control();
    let stop = tokio::time::sleep(Duration::from_secs(271));
    tokio::pin!(stop);
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => { control.abort(); break; }
            event = run.next() => { assert!(event.is_some()); }
        }
    }
    while run.next().await.is_some() {}
    drop(run);
    assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 0);
    let path = agent.session().path().to_owned();
    let mut concurrent = Session::open(&path).unwrap();
    concurrent
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(
                "another writer advanced this session".into(),
            )],
        })))
        .unwrap();
    drop(concurrent);
    assert!(agent.set_cache_warming_mode(CacheWarmMode::Off).is_err());
    assert_eq!(agent.cache_warming_mode(), CacheWarmMode::Off);
    assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 1);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.has_uncertain_usage());
    assert!(reopened.usage_uncertainty_exposure().is_none());
    assert_eq!(
        reopened.cache_warm_records().last().unwrap().state,
        CacheWarmState::Started
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_undriven_run_cancels_warm_synchronously() {
    for mode in [CacheWarmMode::Streaming, CacheWarmMode::Idle] {
        let (mut agent, transport, _observed, _executions, _workspace) =
            fixture(Hold::Tool, Duration::from_secs(600), 1, true, false);
        agent.set_cache_warming_mode(mode).unwrap();
        let started = Instant::now();
        let mut run = agent.prompt("drop this long task").await.unwrap();
        let stop = tokio::time::sleep(Duration::from_secs(271));
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => break,
                event = run.next() => { assert!(event.is_some()); }
            }
        }
        drop(run);
        assert_eq!(Instant::now() - started, Duration::from_secs(271));
        assert_snapshot(&transport, 1);
        assert_eq!(transport.warm_dropped.load(Ordering::SeqCst), 1);
        assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn active_off_cancels_pending_warm_while_real_work_continues() {
    for hold in [Hold::Tool, Hold::Opening, Hold::Body] {
        for reenable in [false, true] {
            let (mut agent, transport, observed, _executions, _workspace) = fixture(
                hold,
                Duration::from_secs(600),
                usize::from(matches!(hold, Hold::Tool)),
                true,
                false,
            );
            let started = Instant::now();
            let mut run = agent
                .prompt("change only the maintenance policy")
                .await
                .unwrap();
            let control = run.control();
            let stopped_transport = transport.clone();
            let mode_change = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(271)).await;
                assert_eq!(
                    control.cache_warming_status().state,
                    CacheWarmingState::Refreshing
                );
                control.set_cache_warming_mode(CacheWarmMode::Off).unwrap();
                if reenable {
                    // Coalescing may not undo the intervening off's cancellation.
                    control.set_cache_warming_mode(CacheWarmMode::Idle).unwrap();
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                assert_eq!(
                    control.cache_warming_status().state,
                    CacheWarmingState::Inactive
                );
                assert_eq!(stopped_transport.warm_dropped.load(Ordering::SeqCst), 1);
            });
            let mut events = Vec::new();
            while let Some(event) = run.next().await {
                events.push(event);
            }
            drop(run);
            mode_change.await.unwrap();
            assert_eq!(Instant::now() - started, Duration::from_secs(600));
            assert_eq!(
                agent.cache_warming_mode(),
                if reenable {
                    CacheWarmMode::Idle
                } else {
                    CacheWarmMode::Off
                }
            );
            assert_snapshot(&transport, 1);
            assert_isolated(&agent, &events, 0);
            assert_eq!(observed.uncertain.load(Ordering::SeqCst), 1);
            assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
            assert_eq!(
                agent.session().cache_warm_records().last().unwrap().state,
                CacheWarmState::Failed
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn completed_and_dropped_controls_cannot_change_the_retained_policy() {
    for complete in [false, true] {
        let (mut agent, _transport, _observed, _executions, _workspace) =
            fixture(Hold::Body, Duration::from_secs(1), 0, false, false);
        let mut run = agent.prompt("fence this run's host handle").await.unwrap();
        let control = run.control();
        if complete {
            while run.next().await.is_some() {}
        }
        drop(run);
        assert!(matches!(
            control.set_cache_warming_mode(CacheWarmMode::Off),
            Err(octet_agent::AgentError::RunEnded)
        ));
        assert_eq!(agent.cache_warming_mode(), CacheWarmMode::Streaming);
    }
}
