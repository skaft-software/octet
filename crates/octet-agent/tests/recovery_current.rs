#![allow(missing_docs)]

//! Focused #350 qualification fixtures for the current Codex recovery path.
//!
//! These tests intentionally use loopback HTTP/WebSocket fixtures. They do not
//! claim provider availability, remote billing reconciliation, or endurance.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use octet_agent::{
    Agent, AgentConfig, AgentEvent, CoreTools, EffectBroker, EffectPolicy, ExtensionHost,
    FinishReason, SandboxConfig, Session,
};
use octet_ai::{
    AiClient, Auth, Capabilities, Endpoint, EndpointId, EndpointTransport, ModalitySet, Model,
    ModelId, ModelLimits, ModelSpec, Protocol, ReasoningConfig, RequestRuntime,
    ResponsesRuntimeProfile,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message as WebSocketMessage};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

fn responses_event(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

fn responses_text_turn(response_id: &str, text: &str) -> String {
    [
        serde_json::json!({"type": "response.created", "response": {"id": response_id}}),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"id": format!("msg_{response_id}"), "type": "message"},
        }),
        serde_json::json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": text,
        }),
        serde_json::json!({
            "type": "response.output_text.done",
            "output_index": 0,
            "content_index": 0,
        }),
        serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "output": [{
                    "type": "message",
                    "id": format!("msg_{response_id}"),
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text, "annotations": []}],
                }],
                "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
            },
        }),
    ]
    .into_iter()
    .map(responses_event)
    .collect()
}

fn responses_function_turn(
    response_id: &str,
    call_id: &str,
    name: &str,
    arguments: &serde_json::Value,
) -> String {
    let arguments = arguments.to_string();
    let output = serde_json::json!([{
        "type": "function_call",
        "id": format!("fc_{response_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }]);
    [
        serde_json::json!({"type": "response.created", "response": {"id": response_id}}),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "id": format!("fc_{response_id}"),
                "type": "function_call",
                "call_id": call_id,
                "name": name,
            },
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.done",
            "output_index": 0,
            "arguments": arguments,
        }),
        serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "output": output,
                "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8},
            },
        }),
    ]
    .into_iter()
    .map(responses_event)
    .collect()
}

fn interrupted_function_prefix(
    response_id: &str,
    call_id: &str,
    name: &str,
    arguments: &serde_json::Value,
) -> String {
    responses_function_turn(response_id, call_id, name, arguments)
        .lines()
        .take_while(|line| !line.contains("\"type\":\"response.completed\""))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn provider_stream_error(code: &str) -> String {
    responses_event(serde_json::json!({
        "type": "error",
        "code": code,
        "message": "synthetic provider interruption",
    }))
}

struct SseScript {
    bodies: Vec<String>,
    next: AtomicUsize,
}

impl Respond for SseScript {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(index)
            .or_else(|| self.bodies.last())
            .expect("recovery script needs one body")
            .clone();
        ResponseTemplate::new(200)
            .set_body_string(body)
            .insert_header("content-type", "text/event-stream")
    }
}

fn codex_model(uri: &str, transport: EndpointTransport) -> Model {
    let endpoint_id = EndpointId("recovery-current".to_owned());
    Model {
        spec: Arc::new(ModelSpec {
            id: ModelId("recovery-current-model".to_owned()),
            endpoint: endpoint_id.clone(),
            api_name: "recovery-current-model".to_owned(),
            display_name: None,
            protocol: Protocol::OpenAiResponses,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: ModalitySet::none(),
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
                max_output_tokens: 8192,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
            preset: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: endpoint_id,
            base_url: url::Url::parse(&format!("{uri}/")).unwrap(),
            auth: Auth::bearer("fixture-token"),
            default_headers: http::HeaderMap::new(),
            transport,
            runtime: RequestRuntime {
                responses_profile: ResponsesRuntimeProfile::Codex,
                ..RequestRuntime::default()
            },
            timeout: Duration::from_secs(2),
        }),
    }
}

fn build_agent(model: Model, session_path: &Path, workspace: &Path) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_write = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(session_path).unwrap(),
        system: "You are a deterministic recovery fixture agent.".to_owned(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: Some("recovery-current-session".to_owned()),
    })
    .unwrap()
}

async fn collect_events(run: &mut octet_agent::Run<'_>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    events
}

fn assert_completed(events: &[AgentEvent]) {
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::RunFinished {
                reason: FinishReason::Completed,
                ..
            })
        ),
        "expected a completed run, got {events:?}"
    );
}

#[tokio::test]
async fn current_candidate_interrupted_codex_stream_discards_provisional_tool() {
    let server = MockServer::start().await;
    let failed_arguments = serde_json::json!({"command": "printf x >> effects.txt"});
    let accepted_arguments = serde_json::json!({"command": "printf x >> effects.txt"});
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(SseScript {
            bodies: vec![
                interrupted_function_prefix("failed", "failed-call", "bash", &failed_arguments)
                    + &provider_stream_error("server_error"),
                responses_function_turn("accepted", "accepted-call", "bash", &accepted_arguments),
                responses_text_turn("final", "recovered answer"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("session.jsonl");
    let mut agent = build_agent(
        codex_model(&server.uri(), EndpointTransport::Http),
        &session_path,
        workspace.path(),
    );
    let mut run = agent
        .prompt("finish without duplicating effects")
        .await
        .unwrap();
    let events = collect_events(&mut run).await;
    drop(run);

    assert_completed(&events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
            .count(),
        1,
        "only the committed replacement may execute a tool"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("effects.txt")).unwrap(),
        "x"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);

    drop(agent);
    let durable = std::fs::read_to_string(session_path).unwrap();
    assert!(!durable.contains("failed-call"));
    assert!(!durable.contains("synthetic provider interruption"));
    assert!(durable.contains("accepted-call"));
}

struct WsFallbackFixture {
    base_url: String,
    websocket_requests: Arc<AtomicUsize>,
    http_requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WsFallbackFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_ws_fallback_fixture(http_failures: usize, drop_first: bool) -> WsFallbackFixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let websocket_requests = Arc::new(AtomicUsize::new(0));
    let http_requests = Arc::new(AtomicUsize::new(0));
    let websocket_count = Arc::clone(&websocket_requests);
    let http_count = Arc::clone(&http_requests);
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let websocket_count = Arc::clone(&websocket_count);
            let http_count = Arc::clone(&http_count);
            tokio::spawn(async move {
                let _ = serve_ws_fallback_connection(
                    stream,
                    websocket_count,
                    http_count,
                    http_failures,
                    drop_first,
                )
                .await;
            });
        }
    });
    WsFallbackFixture {
        base_url: format!("http://{address}"),
        websocket_requests,
        http_requests,
        task,
    }
}

async fn serve_ws_fallback_connection(
    mut stream: TcpStream,
    websocket_requests: Arc<AtomicUsize>,
    http_requests: Arc<AtomicUsize>,
    http_failures: usize,
    drop_first: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut peek = [0_u8; 4096];
    let count = stream.peek(&mut peek).await?;
    let request_head = String::from_utf8_lossy(&peek[..count]).to_ascii_lowercase();
    if request_head.contains("upgrade: websocket") {
        let mut socket = accept_async(stream).await?;
        if !matches!(socket.next().await, Some(Ok(WebSocketMessage::Text(_)))) {
            return Ok(());
        }
        let attempt = websocket_requests.fetch_add(1, Ordering::SeqCst);
        if drop_first {
            let body = responses_text_turn(
                "replacement",
                if attempt == 0 {
                    "provisional"
                } else {
                    "recovered"
                },
            );
            for frame in body.split("\n\n").filter(|frame| !frame.is_empty()) {
                if attempt == 0 && frame.contains("response.completed") {
                    break;
                }
                socket
                    .send(WebSocketMessage::Text(
                        frame.strip_prefix("data: ").unwrap().to_owned().into(),
                    ))
                    .await?;
            }
            return Ok(());
        }
        for event in [
            serde_json::json!({
                "type": "response.created",
                "response": {"id": "retired-response"}
            }),
            serde_json::json!({
                "type": "response.failed",
                "response": {
                    "error": {
                        "code": "websocket_connection_limit_reached",
                        "message": "synthetic connection retirement"
                    }
                }
            }),
        ] {
            socket
                .send(WebSocketMessage::Text(event.to_string().into()))
                .await?;
        }
        return Ok(());
    }

    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(position) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let header_end = position + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        break (header_end, content_length);
    };
    while request.len().saturating_sub(header_end) < content_length {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }

    let attempt = http_requests.fetch_add(1, Ordering::SeqCst);
    let (status, content_type, body) = if attempt < http_failures {
        (
            503,
            "application/json",
            r#"{"error":{"code":"server_error","message":"synthetic fallback outage"}}"#.to_owned(),
        )
    } else {
        (
            200,
            "text/event-stream",
            responses_text_turn("http", "recovered").to_owned(),
        )
    };
    let response = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn nonstored_websocket_drop_replaces_only_the_host_qualified_attempt() {
    let fixture = start_ws_fallback_fixture(0, true).await;
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("nonstored.jsonl");
    let mut agent = build_agent(
        codex_model(&fixture.base_url, EndpointTransport::WebSocketPreferred),
        &path,
        workspace.path(),
    );
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        agent.complete("recover a nonstored response"),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(output.reason, FinishReason::Completed),
        "{:?}",
        output.reason
    );
    let context = serde_json::to_string(&agent.session().context().unwrap()).unwrap();
    assert!(
        context.contains("recovered") && !context.contains("provisional"),
        "{context}"
    );
    assert_eq!(
        fixture.websocket_requests.load(Ordering::SeqCst)
            + fixture.http_requests.load(Ordering::SeqCst),
        2
    );
    assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
    drop(agent);
    assert!(Session::open(&path).unwrap().has_uncertain_usage());
}

#[tokio::test]
async fn current_candidate_retired_websocket_uses_http_fallback_without_stale_reuse() {
    let fixture = start_ws_fallback_fixture(1, false).await;
    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("session.jsonl");
    let mut agent = build_agent(
        codex_model(&fixture.base_url, EndpointTransport::WebSocketPreferred),
        &session_path,
        workspace.path(),
    );

    let first = agent
        .complete("recover through the retired socket")
        .await
        .unwrap();
    assert_eq!(first.text, "recovered");
    assert_eq!(fixture.websocket_requests.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.http_requests.load(Ordering::SeqCst), 2);
    assert!(agent.session().has_uncertain_usage());

    let second = agent
        .complete("do not reuse the stale socket")
        .await
        .unwrap();
    assert_eq!(second.text, "recovered");
    assert_eq!(fixture.websocket_requests.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.http_requests.load(Ordering::SeqCst), 3);
}

struct OfflineCredentials;

#[async_trait]
impl octet_ai::CredentialResolver for OfflineCredentials {
    async fn resolve(&self) -> Result<octet_ai::ResolvedCredential, octet_ai::AuthError> {
        Err(octet_ai::AuthError::Unavailable)
    }
}

fn offline_agent(workspace: &tempfile::TempDir) -> Agent {
    let mut model = codex_model("http://127.0.0.1:9", EndpointTransport::Http);
    Arc::make_mut(&mut model.endpoint).auth = Auth::dynamic(Arc::new(OfflineCredentials));
    build_agent(
        model,
        &workspace.path().join("session.jsonl"),
        workspace.path(),
    )
}

#[tokio::test(start_paused = true)]
async fn current_candidate_network_wait_is_bounded_and_cancellable() {
    let workspace = tempfile::tempdir().unwrap();
    let mut bounded = offline_agent(&workspace);
    let limit = Duration::from_secs(2);
    bounded.set_max_network_wait(Some(limit));
    let started = tokio::time::Instant::now();
    match bounded.complete("wait for the provider").await {
        Err(octet_agent::AgentError::NetworkWaitLimit {
            limit: actual,
            usage_unknown: false,
        }) => assert_eq!(actual, limit),
        other => panic!("expected bounded network wait, got {other:?}"),
    }
    assert_eq!(started.elapsed(), limit);
    assert!(!bounded.session().has_uncertain_usage());

    let cancelled_workspace = tempfile::tempdir().unwrap();
    let mut cancelled = offline_agent(&cancelled_workspace);
    let mut run = cancelled.prompt("cancel the outage wait").await.unwrap();
    let control = run.control();
    let mut saw_wait = false;
    let mut finish = None;
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::ProviderWaitingForNetwork { .. } => {
                saw_wait = true;
                control.abort();
            }
            AgentEvent::RunFinished { reason, .. } => {
                finish = Some(reason);
                break;
            }
            _ => {}
        }
    }
    assert!(saw_wait);
    assert!(matches!(finish, Some(FinishReason::Aborted)));
    drop(run);
    assert!(!cancelled.session().has_uncertain_usage());
}
