#![allow(missing_docs)]

//! Focused deterministic qualification fixtures for ordered read waves.
//!
//! These tests deliberately use barriers and one-shot signals instead of elapsed
//! time. They are source fixtures for the separate Rust verification lane.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use octet_agent::{
    Agent, AgentConfig, AgentEvent, EffectBroker, EffectPolicy, ExtensionHost, FinishReason,
    ReadTool, ReplaySafety, SandboxConfig, Session, Tool, ToolCallHook, ToolConcurrency,
    ToolContext, ToolEffect, ToolError, ToolOutput,
};
use octet_ai::{
    AiClient, Auth, CacheCompatibility, Capabilities, Endpoint, EndpointId, Modality, ModalitySet,
    Model, ModelId, ModelLimits, ModelSpec, Protocol, ReasoningConfig, ReasoningMode,
    RequestRuntime,
};
use tokio::sync::{oneshot, Barrier, Notify};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nordered-image";
const WAV_BYTES: &[u8] = b"RIFF\x04\x00\x00\x00WAVEpayload";

fn frame(event: &str, data: serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn message_start() -> String {
    frame(
        "message_start",
        serde_json::json!({
            "type": "message_start",
            "message": {"id": "msg", "usage": {"input_tokens": 5, "output_tokens": 0}}
        }),
    )
}

fn message_end(stop_reason: &str) -> String {
    frame(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 3}
        }),
    ) + &frame("message_stop", serde_json::json!({"type": "message_stop"}))
}

fn anthropic_tool_block(
    index: usize,
    id: &str,
    name: &str,
    arguments: &serde_json::Value,
) -> String {
    frame(
        "content_block_start",
        serde_json::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "tool_use", "id": id, "name": name}
        }),
    ) + &frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": arguments.to_string()}
        }),
    ) + &frame(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": index}),
    )
}

fn anthropic_tool_turn(calls: &[(&str, &str, serde_json::Value)]) -> String {
    let mut body = message_start();
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        body += &anthropic_tool_block(index, id, name, arguments);
    }
    body + &message_end("tool_use")
}

fn anthropic_text_turn(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    message_start()
        + &frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
        )
        + &frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": serde_json::from_str::<String>(&text).unwrap()}
            }),
        )
        + &frame(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        )
        + &message_end("end_turn")
}

fn openai_tool_turn(calls: &[(&str, &str, serde_json::Value)]) -> String {
    let mut body = String::new();
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        body += &format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": "chat-tools",
                "choices": [{
                    "index": 0,
                    "delta": {"tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments.to_string()}
                    }]}
                }]
            })
        );
    }
    body + "data: {\"id\":\"chat-tools\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"
}

fn openai_text_turn(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    format!(
        "data: {{\"id\":\"chat\",\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":{text}}}}}]}}\n\ndata: {{\"id\":\"chat\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}}}\n\ndata: [DONE]\n\n"
    )
}

struct Script {
    bodies: Vec<String>,
    next: AtomicUsize,
}

impl Respond for Script {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(index)
            .or_else(|| self.bodies.last())
            .expect("script has a response")
            .clone();
        ResponseTemplate::new(200)
            .set_body_string(body)
            .insert_header("content-type", "text/event-stream")
    }
}

fn test_model(uri: &str, protocol: Protocol, audio: bool) -> Model {
    let mut input_modalities = ModalitySet::none().with(Modality::Image);
    if audio {
        input_modalities = input_modalities.with(Modality::Audio);
    }
    let base_url = match protocol {
        Protocol::AnthropicMessages => url::Url::parse(uri).unwrap(),
        Protocol::OpenAiChat => url::Url::parse(&format!("{uri}/v1/")).unwrap(),
        other => panic!("unsupported fixture protocol: {other:?}"),
    };
    Model {
        spec: Arc::new(ModelSpec {
            id: ModelId("read-concurrency-scripted".into()),
            endpoint: EndpointId("read-concurrency-test".into()),
            api_name: "read-concurrency-scripted".into(),
            display_name: None,
            protocol,
            capabilities: Capabilities {
                input_modalities,
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 200_000,
                max_output_tokens: 8_192,
            },
            pricing: None,
            cache: CacheCompatibility::default(),
            preset: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("read-concurrency-test".into()),
            base_url,
            auth: Auth::bearer("fixture-key"),
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: RequestRuntime::default(),
            timeout: Duration::from_secs(10),
        }),
    }
}

fn build_agent(
    model: Model,
    workspace: &Path,
    session_path: &Path,
    extensions: ExtensionHost,
) -> Agent {
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(session_path).unwrap(),
        system: "deterministic read-wave qualification".into(),
        sandbox: SandboxConfig::new(workspace),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

async fn drive_until_signal(
    run: &mut octet_agent::Run<'_>,
    events: &mut Vec<AgentEvent>,
    signal: oneshot::Receiver<()>,
) {
    tokio::pin!(signal);
    loop {
        tokio::select! {
            biased;
            result = &mut signal => {
                result.expect("deterministic fixture signal");
                return;
            }
            event = run.next() => match event {
                Some(event) => events.push(event),
                None => panic!("run ended before deterministic fixture signal"),
            },
        }
    }
}

async fn collect_run(run: &mut octet_agent::Run<'_>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    events
}

fn release(flag: &AtomicBool, notify: &Notify) {
    flag.store(true, Ordering::Release);
    notify.notify_waiters();
}

async fn wait_release(flag: &AtomicBool, notify: &Notify) {
    loop {
        if flag.load(Ordering::Acquire) {
            return;
        }
        let notified = notify.notified();
        if flag.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

struct WaveState {
    entered: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    first: Barrier,
    second: Barrier,
    first_release: AtomicBool,
    second_release: AtomicBool,
    release_notify: Notify,
    first_signal: Mutex<Option<oneshot::Sender<()>>>,
    second_signal: Mutex<Option<oneshot::Sender<()>>>,
}

struct WaveProbe {
    state: Arc<WaveState>,
}

fn signal(sender: &Mutex<Option<oneshot::Sender<()>>>) {
    if let Ok(mut sender) = sender.lock() {
        if let Some(sender) = sender.take() {
            let _ = sender.send(());
        }
    }
}

#[async_trait::async_trait]
impl Tool for WaveProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "host_read_probe".into(),
            description: "bounded HostRead wave probe".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"slot": {"type": "integer"}},
                "required": ["slot"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::HostRead)
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let number = self.state.entered.fetch_add(1, Ordering::SeqCst) + 1;
        let active = self.state.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.maximum.fetch_max(active, Ordering::SeqCst);
        if number <= 4 {
            self.state.first.wait().await;
            signal(&self.state.first_signal);
            wait_release(&self.state.first_release, &self.state.release_notify).await;
        } else {
            self.state.second.wait().await;
            signal(&self.state.second_signal);
            wait_release(&self.state.second_release, &self.state.release_notify).await;
        }
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::new(format!("slot={}", arguments["slot"])))
    }
}

#[tokio::test]
async fn host_read_waves_are_bounded_and_keep_effect_and_result_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                anthropic_tool_turn(&[
                    ("call_0", "host_read_probe", serde_json::json!({"slot": 0})),
                    ("call_1", "host_read_probe", serde_json::json!({"slot": 1})),
                    ("call_2", "host_read_probe", serde_json::json!({"slot": 2})),
                    ("call_3", "host_read_probe", serde_json::json!({"slot": 3})),
                    ("call_4", "host_read_probe", serde_json::json!({"slot": 4})),
                    ("call_5", "host_read_probe", serde_json::json!({"slot": 5})),
                ]),
                anthropic_text_turn("all host reads completed"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let state = Arc::new(WaveState {
        entered: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        first: Barrier::new(4),
        second: Barrier::new(2),
        first_release: AtomicBool::new(false),
        second_release: AtomicBool::new(false),
        release_notify: Notify::new(),
        first_signal: Mutex::new(Some(first_tx)),
        second_signal: Mutex::new(Some(second_tx)),
    });
    let mut extensions = ExtensionHost::new();
    extensions.tool(WaveProbe {
        state: Arc::clone(&state),
    });
    let mut agent = build_agent(
        test_model(&server.uri(), Protocol::AnthropicMessages, false),
        workspace_dir.path(),
        &session_dir.path().join("host-waves.jsonl"),
        extensions,
    );

    let mut run = agent.prompt("read six host files").await.unwrap();
    let mut events = Vec::new();
    while events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        .count()
        < 4
    {
        events.push(run.next().await.expect("first wave starts"));
    }
    drive_until_signal(&mut run, &mut events, first_rx).await;
    assert_eq!(state.entered.load(Ordering::Acquire), 4);
    assert_eq!(state.maximum.load(Ordering::Acquire), 4);

    release(&state.first_release, &state.release_notify);
    drive_until_signal(&mut run, &mut events, second_rx).await;
    assert_eq!(state.entered.load(Ordering::Acquire), 6);
    assert_eq!(state.maximum.load(Ordering::Acquire), 4);

    release(&state.second_release, &state.release_notify);
    events.extend(collect_run(&mut run).await);
    drop(run);

    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFinished {
            reason: FinishReason::Completed,
            ..
        })
    ));
    let finished_ids: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { id, .. } => Some(id.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        finished_ids,
        vec!["call_0", "call_1", "call_2", "call_3", "call_4", "call_5"]
    );
    let effects: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolPolicyDecision { decision, .. } => Some(decision.effect),
            _ => None,
        })
        .collect();
    assert_eq!(effects, vec![Some(ToolEffect::HostRead); 6]);
    assert_eq!(agent.session().entries().len(), 9);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
}

struct BarrierState {
    first: Barrier,
    second: Barrier,
    entered_second: AtomicUsize,
    first_release: AtomicBool,
    second_release: AtomicBool,
    mutation_release: AtomicBool,
    release_notify: Notify,
    first_signal: Mutex<Option<oneshot::Sender<()>>>,
    second_signal: Mutex<Option<oneshot::Sender<()>>>,
    mutation_started: AtomicBool,
    mutation_signal: Mutex<Option<oneshot::Sender<()>>>,
}

struct PhaseRead {
    state: Arc<BarrierState>,
}

#[async_trait::async_trait]
impl Tool for PhaseRead {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "host_read_probe".into(),
            description: "barrier HostRead probe".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"phase": {"type": "integer"}, "tag": {"type": "string"}},
                "required": ["phase", "tag"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::HostRead)
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        if arguments["phase"] == 1 {
            self.state.first.wait().await;
            signal(&self.state.first_signal);
            wait_release(&self.state.first_release, &self.state.release_notify).await;
        } else {
            self.state.entered_second.fetch_add(1, Ordering::SeqCst);
            self.state.second.wait().await;
            signal(&self.state.second_signal);
            wait_release(&self.state.second_release, &self.state.release_notify).await;
        }
        Ok(ToolOutput::new(
            arguments["tag"].as_str().unwrap_or("phase"),
        ))
    }
}

struct BarrierMutation {
    state: Arc<BarrierState>,
}

#[async_trait::async_trait]
impl Tool for BarrierMutation {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "mutation_probe".into(),
            description: "serialized mutation barrier".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"tag": {"type": "string"}},
                "required": ["tag"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::WorkspaceMutation)
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.state.mutation_started.store(true, Ordering::Release);
        signal(&self.state.mutation_signal);
        wait_release(&self.state.mutation_release, &self.state.release_notify).await;
        Ok(ToolOutput::new("mutation"))
    }
}

struct OrderedHook {
    events: Arc<Mutex<Vec<String>>>,
    active: AtomicBool,
    overlap: Arc<AtomicBool>,
}

fn hook_tag(arguments: &serde_json::Value) -> &str {
    arguments["tag"].as_str().unwrap_or("unknown")
}

#[async_trait::async_trait]
impl ToolCallHook for OrderedHook {
    async fn before_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        if self.active.swap(true, Ordering::SeqCst) {
            self.overlap.store(true, Ordering::Release);
        }
        self.events
            .lock()
            .unwrap()
            .push(format!("before:{name}:{}", hook_tag(arguments)));
        tokio::task::yield_now().await;
        self.active.store(false, Ordering::Release);
        Ok(())
    }

    async fn after_tool_call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        _output: &str,
        _is_error: bool,
        _context: &ToolContext<'_>,
    ) {
        if self.active.swap(true, Ordering::SeqCst) {
            self.overlap.store(true, Ordering::Release);
        }
        self.events
            .lock()
            .unwrap()
            .push(format!("after:{name}:{}", hook_tag(arguments)));
        tokio::task::yield_now().await;
        self.active.store(false, Ordering::Release);
    }
}

#[tokio::test]
async fn read_waves_stop_at_mutation_barriers_and_serialize_hooks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                anthropic_tool_turn(&[
                    (
                        "r1",
                        "host_read_probe",
                        serde_json::json!({"phase": 1, "tag": "r1"}),
                    ),
                    (
                        "r2",
                        "host_read_probe",
                        serde_json::json!({"phase": 1, "tag": "r2"}),
                    ),
                    ("m", "mutation_probe", serde_json::json!({"tag": "m"})),
                    (
                        "r3",
                        "host_read_probe",
                        serde_json::json!({"phase": 2, "tag": "r3"}),
                    ),
                    (
                        "r4",
                        "host_read_probe",
                        serde_json::json!({"phase": 2, "tag": "r4"}),
                    ),
                ]),
                anthropic_text_turn("barriers preserved"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let (mutation_tx, mutation_rx) = oneshot::channel();
    let state = Arc::new(BarrierState {
        first: Barrier::new(2),
        second: Barrier::new(2),
        entered_second: AtomicUsize::new(0),
        first_release: AtomicBool::new(false),
        second_release: AtomicBool::new(false),
        mutation_release: AtomicBool::new(false),
        release_notify: Notify::new(),
        first_signal: Mutex::new(Some(first_tx)),
        second_signal: Mutex::new(Some(second_tx)),
        mutation_started: AtomicBool::new(false),
        mutation_signal: Mutex::new(Some(mutation_tx)),
    });
    let hook_events = Arc::new(Mutex::new(Vec::new()));
    let overlap = Arc::new(AtomicBool::new(false));
    let hook = OrderedHook {
        events: Arc::clone(&hook_events),
        active: AtomicBool::new(false),
        overlap: Arc::clone(&overlap),
    };
    let mut extensions = ExtensionHost::new();
    extensions.tool(PhaseRead {
        state: Arc::clone(&state),
    });
    extensions.tool(BarrierMutation {
        state: Arc::clone(&state),
    });
    extensions.tool_call_hook(hook);
    let mut agent = build_agent(
        test_model(&server.uri(), Protocol::AnthropicMessages, false),
        workspace_dir.path(),
        &session_dir.path().join("barriers.jsonl"),
        extensions,
    );

    let mut run = agent.prompt("read around the mutation").await.unwrap();
    let mut events = Vec::new();
    while events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        .count()
        < 2
    {
        events.push(run.next().await.expect("first read wave starts"));
    }
    drive_until_signal(&mut run, &mut events, first_rx).await;
    assert!(!state.mutation_started.load(Ordering::Acquire));
    assert_eq!(state.entered_second.load(Ordering::Acquire), 0);

    release(&state.first_release, &state.release_notify);
    drive_until_signal(&mut run, &mut events, mutation_rx).await;
    assert!(state.mutation_started.load(Ordering::Acquire));
    assert_eq!(state.entered_second.load(Ordering::Acquire), 0);

    release(&state.mutation_release, &state.release_notify);
    drive_until_signal(&mut run, &mut events, second_rx).await;
    assert_eq!(state.entered_second.load(Ordering::Acquire), 2);
    release(&state.second_release, &state.release_notify);
    events.extend(collect_run(&mut run).await);
    drop(run);

    assert!(!overlap.load(Ordering::Acquire));
    assert_eq!(
        *hook_events.lock().unwrap(),
        vec![
            "before:host_read_probe:r1",
            "before:host_read_probe:r2",
            "after:host_read_probe:r1",
            "after:host_read_probe:r2",
            "before:mutation_probe:m",
            "after:mutation_probe:m",
            "before:host_read_probe:r3",
            "before:host_read_probe:r4",
            "after:host_read_probe:r3",
            "after:host_read_probe:r4",
        ]
    );
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStarted { id, .. } => Some(id.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec!["r1", "r2", "m", "r3", "r4"]);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFinished {
            reason: FinishReason::Completed,
            ..
        })
    ));
}

struct CancelRead {
    ready: Barrier,
    signal: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait::async_trait]
impl Tool for CancelRead {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "cancel_read_probe".into(),
            description: "cancellable HostRead probe".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::HostRead)
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.ready.wait().await;
        signal(&self.signal);
        std::future::pending::<()>().await;
        Ok(ToolOutput::new("unreachable"))
    }
}

#[tokio::test]
async fn abort_during_a_read_wave_keeps_pairing_and_stops_future_turns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                anthropic_tool_turn(&[
                    ("cancel_a", "cancel_read_probe", serde_json::json!({})),
                    ("cancel_b", "cancel_read_probe", serde_json::json!({})),
                ]),
                anthropic_text_turn("must not be requested after abort"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let (ready_tx, ready_rx) = oneshot::channel();
    let mut extensions = ExtensionHost::new();
    extensions.tool(CancelRead {
        ready: Barrier::new(2),
        signal: Mutex::new(Some(ready_tx)),
    });
    let mut agent = build_agent(
        test_model(&server.uri(), Protocol::AnthropicMessages, false),
        workspace_dir.path(),
        &session_dir.path().join("cancel.jsonl"),
        extensions,
    );

    let mut run = agent.prompt("start cancellable reads").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        .count()
        < 2
    {
        events.push(run.next().await.expect("cancellable wave starts"));
    }
    drive_until_signal(&mut run, &mut events, ready_rx).await;
    control.abort();
    events.extend(collect_run(&mut run).await);
    drop(run);

    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFinished {
            reason: FinishReason::Aborted,
            ..
        })
    ));
    let cancelled = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished {
                result: Err(error), ..
            } => Some(error.message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cancelled.len(), 2);
    assert!(cancelled.iter().all(|message| message.contains("cancel")));
    assert_eq!(agent.session().entries().len(), 4);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

struct IdentityReadGate {
    gate: Arc<Barrier>,
}

#[async_trait::async_trait]
impl Tool for IdentityReadGate {
    fn definition(&self) -> octet_ai::ToolDef {
        ReadTool.definition()
    }

    fn effect(
        &self,
        arguments: &serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        ReadTool.effect(arguments, context)
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.gate.wait().await;
        ReadTool.execute(arguments, context).await
    }
}

#[tokio::test]
async fn text_image_and_audio_reads_retain_identity_in_ordered_slots() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script {
            bodies: vec![
                openai_tool_turn(&[
                    ("call_text", "read", serde_json::json!({"path": "note.txt"})),
                    (
                        "call_image",
                        "read",
                        serde_json::json!({"path": "capture.png"}),
                    ),
                    (
                        "call_audio",
                        "read",
                        serde_json::json!({"path": "memo.wav"}),
                    ),
                ]),
                openai_text_turn("identity preserved"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    std::fs::write(workspace_dir.path().join("note.txt"), b"ordered text\n").unwrap();
    std::fs::write(workspace_dir.path().join("capture.png"), PNG_BYTES).unwrap();
    std::fs::write(workspace_dir.path().join("memo.wav"), WAV_BYTES).unwrap();

    let mut extensions = ExtensionHost::new();
    extensions.tool(IdentityReadGate {
        gate: Arc::new(Barrier::new(3)),
    });
    let mut agent = build_agent(
        test_model(&server.uri(), Protocol::OpenAiChat, true),
        workspace_dir.path(),
        &session_dir.path().join("media.jsonl"),
        extensions,
    );

    let output = agent.complete("read text, image, and audio").await.unwrap();
    assert_eq!(output.text, "identity preserved");
    let events = agent.session().entries();
    assert_eq!(events.len(), 6);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let messages = second["messages"].as_array().unwrap();
    let result_ids: Vec<_> = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(result_ids, vec!["call_text", "call_image", "call_audio"]);

    let serialized = second.to_string();
    let image_bytes = base64::engine::general_purpose::STANDARD.encode(PNG_BYTES);
    let audio_bytes = base64::engine::general_purpose::STANDARD.encode(WAV_BYTES);
    let image_position = serialized.find(&image_bytes).expect("image payload");
    let audio_position = serialized.find(&audio_bytes).expect("audio payload");
    assert!(image_position < audio_position);
    assert!(serialized.contains("\"type\":\"image_url\""));
    assert!(serialized.contains("\"type\":\"input_audio\""));

    let _ = events;
}

struct EffectBarrierGate {
    started: Mutex<Option<oneshot::Sender<()>>>,
    release: AtomicBool,
    release_notify: Notify,
}

struct EffectBarrierProbe {
    name: &'static str,
    effect: ToolEffect,
    events: Arc<Mutex<Vec<String>>>,
    gate: Option<Arc<EffectBarrierGate>>,
}

#[async_trait::async_trait]
impl Tool for EffectBarrierProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: self.name.into(),
            description: format!("{} effect barrier probe", self.name),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"tag": {"type": "string"}},
                "required": ["tag"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.events.lock().unwrap().push(self.name.to_owned());
        if let Some(gate) = &self.gate {
            signal(&gate.started);
            wait_release(&gate.release, &gate.release_notify).await;
        }
        Ok(ToolOutput::new(self.name))
    }
}

#[tokio::test]
async fn non_read_effects_are_ordered_barriers_without_parallel_admission() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                anthropic_tool_turn(&[
                    (
                        "r1",
                        "host_read_probe",
                        serde_json::json!({"phase": 1, "tag": "r1"}),
                    ),
                    (
                        "r2",
                        "host_read_probe",
                        serde_json::json!({"phase": 1, "tag": "r2"}),
                    ),
                    (
                        "u",
                        "unknown_barrier",
                        serde_json::json!({"tag": "unknown"}),
                    ),
                    (
                        "m",
                        "mutation_barrier",
                        serde_json::json!({"tag": "mutation"}),
                    ),
                    (
                        "p",
                        "process_barrier",
                        serde_json::json!({"tag": "process"}),
                    ),
                    (
                        "n",
                        "network_barrier",
                        serde_json::json!({"tag": "network"}),
                    ),
                    (
                        "d",
                        "delegation_barrier",
                        serde_json::json!({"tag": "delegation"}),
                    ),
                    (
                        "x",
                        "extension_barrier",
                        serde_json::json!({"tag": "extension"}),
                    ),
                    (
                        "r3",
                        "host_read_probe",
                        serde_json::json!({"phase": 2, "tag": "r3"}),
                    ),
                    (
                        "r4",
                        "host_read_probe",
                        serde_json::json!({"phase": 2, "tag": "r4"}),
                    ),
                ]),
                anthropic_text_turn("all effect barriers preserved"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let state = Arc::new(BarrierState {
        first: Barrier::new(2),
        second: Barrier::new(2),
        entered_second: AtomicUsize::new(0),
        first_release: AtomicBool::new(false),
        second_release: AtomicBool::new(false),
        mutation_release: AtomicBool::new(false),
        release_notify: Notify::new(),
        first_signal: Mutex::new(Some(first_tx)),
        second_signal: Mutex::new(Some(second_tx)),
        mutation_started: AtomicBool::new(false),
        mutation_signal: Mutex::new(None),
    });
    let (effect_tx, effect_rx) = oneshot::channel();
    let effect_gate = Arc::new(EffectBarrierGate {
        started: Mutex::new(Some(effect_tx)),
        release: AtomicBool::new(false),
        release_notify: Notify::new(),
    });
    let effect_events = Arc::new(Mutex::new(Vec::new()));
    let mut extensions = ExtensionHost::new();
    extensions.tool(PhaseRead {
        state: Arc::clone(&state),
    });
    for (name, effect) in [
        ("unknown_barrier", ToolEffect::Unknown),
        ("mutation_barrier", ToolEffect::WorkspaceMutation),
        ("process_barrier", ToolEffect::HostProcess),
        ("network_barrier", ToolEffect::Network),
        ("delegation_barrier", ToolEffect::Delegation),
        ("extension_barrier", ToolEffect::Extension),
    ] {
        extensions.tool(EffectBarrierProbe {
            name,
            effect,
            events: Arc::clone(&effect_events),
            gate: (name == "extension_barrier").then_some(Arc::clone(&effect_gate)),
        });
    }
    let mut agent = build_agent(
        test_model(&server.uri(), Protocol::AnthropicMessages, false),
        workspace_dir.path(),
        &session_dir.path().join("effect-barriers.jsonl"),
        extensions,
    );

    let mut run = agent
        .prompt("read around every non-read effect")
        .await
        .unwrap();
    let mut events = Vec::new();
    while events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        .count()
        < 2
    {
        events.push(run.next().await.expect("first read wave starts"));
    }
    drive_until_signal(&mut run, &mut events, first_rx).await;
    assert_eq!(state.entered_second.load(Ordering::Acquire), 0);

    release(&state.first_release, &state.release_notify);
    drive_until_signal(&mut run, &mut events, effect_rx).await;
    assert_eq!(
        *effect_events.lock().unwrap(),
        vec![
            "mutation_barrier",
            "process_barrier",
            "network_barrier",
            "delegation_barrier",
            "extension_barrier",
        ]
    );
    assert_eq!(state.entered_second.load(Ordering::Acquire), 0);

    release(&effect_gate.release, &effect_gate.release_notify);
    drive_until_signal(&mut run, &mut events, second_rx).await;
    assert_eq!(state.entered_second.load(Ordering::Acquire), 2);
    release(&state.second_release, &state.release_notify);
    events.extend(collect_run(&mut run).await);
    drop(run);

    let started: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStarted { id, .. } => Some(id.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        vec!["r1", "r2", "u", "m", "p", "n", "d", "x", "r3", "r4"]
    );
    let finished: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { id, .. } => Some(id.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(finished, started);
    let decisions: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolPolicyDecision { decision, .. } => {
                Some((decision.effect, decision.allowed))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        vec![
            (Some(ToolEffect::HostRead), true),
            (Some(ToolEffect::HostRead), true),
            (Some(ToolEffect::Unknown), false),
            (Some(ToolEffect::WorkspaceMutation), true),
            (Some(ToolEffect::HostProcess), true),
            (Some(ToolEffect::Network), true),
            (Some(ToolEffect::Delegation), true),
            (Some(ToolEffect::Extension), true),
            (Some(ToolEffect::HostRead), true),
            (Some(ToolEffect::HostRead), true),
        ]
    );
    assert!(events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolFinished {
                id,
                result: Err(_),
                ..
            } if id.0.as_str() == "u"
        )
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFinished {
            reason: FinishReason::Completed,
            ..
        })
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
