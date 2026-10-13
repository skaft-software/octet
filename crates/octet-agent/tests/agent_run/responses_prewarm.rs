//! Real loopback WebSocket/HTTP tests for opt-in, caller-driven run prewarming.
use super::*;
use tokio::sync::{Mutex, Notify};

#[derive(Clone, Copy)]
enum Warmup {
    Complete,
    Fail,
    Stall,
}

struct Server {
    uri: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    first_request: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(warmup: Warmup) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let first_request = Arc::new(Notify::new());
        let recorded = requests.clone();
        let notified = first_request.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let requests = recorded.clone();
                        let first_request = notified.clone();
                        connections.spawn(async move {
                            // Cancellation may close the peer between reads/writes.
                            let _ = serve(stream, warmup, requests, first_request).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            uri,
            requests,
            first_request,
            task,
        }
    }

    async fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().await.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut stream: TcpStream,
    warmup: Warmup,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    first_request: Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut peek = [0_u8; 4096];
    let count = stream.peek(&mut peek).await?;
    if !String::from_utf8_lossy(&peek[..count])
        .to_ascii_lowercase()
        .contains("upgrade: websocket")
    {
        let mut request = Vec::new();
        let header_end = loop {
            let mut bytes = [0_u8; 4096];
            let count = stream.read(&mut bytes).await?;
            assert_ne!(count, 0, "HTTP request ended before its headers");
            request.extend_from_slice(&bytes[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .unwrap()
            .trim()
            .parse()?;
        while request.len() < header_end + length {
            let mut bytes = [0_u8; 4096];
            let count = stream.read(&mut bytes).await?;
            assert_ne!(count, 0, "HTTP request ended before its body");
            request.extend_from_slice(&bytes[..count]);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&request[header_end..header_end + length])?;
        requests
            .lock()
            .await
            .push(serde_json::json!({"transport": "http", "body": body}));
        let body = responses_text_turn("http-answer", "READY", "response.completed", "fixture");
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        return Ok(());
    }

    let mut socket = accept_async(stream).await?;
    while let Some(Ok(WebSocketMessage::Text(text))) = socket.next().await {
        let body: serde_json::Value = serde_json::from_str(&text)?;
        requests
            .lock()
            .await
            .push(serde_json::json!({"transport": "websocket", "body": body}));
        if body["generate"] == false {
            first_request.notify_one();
            socket
                .send(WebSocketMessage::Text(
                    serde_json::json!({"type": "response.created", "response": {"id": "warm-prefix"}})
                        .to_string()
                        .into(),
                ))
                .await?;
            let terminal = match warmup {
                Warmup::Complete => serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "warm-prefix", "output": [], "usage": {
                        "input_tokens": 111, "output_tokens": 0, "total_tokens": 111
                    }}
                }),
                Warmup::Fail => serde_json::json!({
                    "type": "response.failed",
                    "response": {"error": {"code": "server_error", "message": "warmup failed"}}
                }),
                Warmup::Stall => continue,
            };
            socket
                .send(WebSocketMessage::Text(terminal.to_string().into()))
                .await?;
        } else {
            for event in responses_text_turn("answer", "READY", "response.completed", "fixture")
                .split("\n\n")
                .filter_map(|frame| frame.strip_prefix("data: "))
            {
                socket
                    .send(WebSocketMessage::Text(event.to_owned().into()))
                    .await?;
            }
        }
    }
    Ok(())
}

fn model(uri: &str, lite: bool) -> Model {
    let mut model = scripted_responses_model(uri);
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.transport = octet_ai::EndpointTransport::WebSocketPreferred;
    endpoint.runtime.responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
    let spec = Arc::make_mut(&mut model.spec);
    spec.capabilities.responses_lite = lite;
    spec.capabilities.reasoning = Some(ReasoningCapability {
        control: ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: true,
        effort_budgets: None,
        options: None,
        openai_chat_mode: Default::default(),
        min_effort: octet_ai::ReasoningEffort::Low,
        max_effort: octet_ai::ReasoningEffort::High,
    });
    model
}

fn agent(model: Model, workspace: &Path, session_path: &Path) -> Agent {
    let mut agent = build_agent_from_session_with_model(
        model,
        workspace,
        Session::create(session_path).unwrap(),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low),
        Some(4),
    );
    agent.set_system_prompt("Stable prewarm system prefix.\n".repeat(64));
    agent.set_tool_prompt_section_enabled(true);
    agent
}

fn assert_ready(events: &[AgentEvent]) {
    assert!(matches!(
        assert_single_run_finished(events),
        FinishReason::Completed
    ));
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "READY");
    let usage: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TurnFinished { usage, .. } => Some(usage),
            _ => None,
        })
        .collect();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].input_tokens, 5);
    assert_eq!(usage[0].output_tokens, 2);
}

#[tokio::test]
async fn headless_responses_prewarm_is_deferred_reuses_prefix_and_excludes_warm_usage() {
    for lite in [false, true] {
        let server = Server::start(Warmup::Complete).await;
        let workspace = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let mut agent = agent(
            model(&server.uri, lite),
            workspace.path(),
            &sessions.path().join("session.jsonl"),
        );
        let input = "Unique first submission, not part of the stable warm prefix.";
        let mut run = agent.prompt_with_responses_prewarm(input).await.unwrap();
        assert!(
            server.requests().await.is_empty(),
            "constructing a Run performs no I/O"
        );
        assert!(matches!(run.next().await, Some(AgentEvent::TurnStarted)));
        assert!(
            server.requests().await.is_empty(),
            "prewarm begins only when the stream is driven"
        );
        let events = run.collect::<Vec<_>>().await;
        assert_ready(&events);
        let requests = server.requests().await;
        assert_eq!(requests.len(), 2);
        let warm = &requests[0]["body"];
        let generation = &requests[1]["body"];
        assert_eq!(warm["generate"], false);
        assert!(!warm.to_string().contains(input));
        assert_eq!(generation["previous_response_id"], "warm-prefix");
        assert_eq!(generation["input"].as_array().unwrap().len(), 1);
        assert!(generation["input"].to_string().contains(input));
        assert_eq!(warm["prompt_cache_key"], generation["prompt_cache_key"]);
        assert_eq!(warm["reasoning"], generation["reasoning"]);
        assert_eq!(warm["tools"], generation["tools"]);
        assert!(warm.to_string().contains("Stable prewarm system prefix"));
        assert!(
            warm.to_string().contains("Available tools:"),
            "warmup includes model-visible tool instructions"
        );

        let events = agent
            .prompt_with_responses_prewarm("Second submission")
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_ready(&events);
        let requests = server.requests().await;
        assert_eq!(
            requests.len(),
            3,
            "a live connection is not prewarmed again"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["body"]["generate"] == false)
                .count(),
            1
        );
        assert_eq!(agent.session().usage_records().len(), 2);
        assert!(agent
            .session()
            .usage_records()
            .iter()
            .all(|record| record.usage.input_tokens == 5));
    }
}

#[tokio::test]
async fn headless_responses_prewarm_failure_and_timeout_do_not_prevent_inference() {
    for warmup in [Warmup::Fail, Warmup::Stall] {
        let server = Server::start(warmup).await;
        let workspace = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let mut model = model(&server.uri, false);
        Arc::make_mut(&mut model.endpoint).timeout = Duration::from_millis(300);
        let mut agent = agent(
            model,
            workspace.path(),
            &sessions.path().join("session.jsonl"),
        );
        let events = tokio::time::timeout(
            Duration::from_secs(5),
            agent
                .prompt_with_responses_prewarm("Reply READY")
                .await
                .unwrap()
                .collect::<Vec<_>>(),
        )
        .await
        .expect("warmup must be bounded");
        assert_ready(&events);
        assert_eq!(agent.session().usage_records().len(), 1);
        let requests = server.requests().await;
        assert_eq!(requests[0]["body"]["generate"], false);
        assert!(
            requests
                .iter()
                .any(|request| request["transport"] == "http"),
            "unusable WebSocket must fall back to HTTP"
        );
        let events = agent
            .prompt_with_responses_prewarm("Second submission after fallback")
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_ready(&events);
        assert_eq!(
            server
                .requests()
                .await
                .iter()
                .filter(|request| request["body"]["generate"] == false)
                .count(),
            1,
            "a latched fallback is not prewarmed again"
        );
    }
}

#[tokio::test]
async fn headless_responses_prewarm_abort_finishes_once_without_generation_or_usage() {
    let server = Server::start(Warmup::Stall).await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = agent(
        model(&server.uri, false),
        workspace.path(),
        &sessions.path().join("session.jsonl"),
    );
    let mut run = agent
        .prompt_with_responses_prewarm("Reply READY")
        .await
        .unwrap();
    let control = run.control();
    let (events, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            async {
                let mut events = Vec::new();
                while let Some(event) = run.next().await {
                    events.push(event);
                }
                events
            },
            async {
                server.first_request.notified().await;
                control.abort();
            }
        )
    })
    .await
    .expect("abort must not wait for warmup timeout");
    drop(run);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::RunFinished { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::RunFinished {
            reason: FinishReason::Aborted,
            ..
        }
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnFinished { .. })));
    assert_eq!(server.requests().await.len(), 1);
    assert_eq!(agent.session().usage_records().len(), 0);
    assert_eq!(agent.session().context().unwrap().len(), 1);
}

#[tokio::test]
async fn headless_responses_prewarm_drop_cancels_setup_without_durable_warm_output() {
    let server = Server::start(Warmup::Stall).await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = agent(
        model(&server.uri, false),
        workspace.path(),
        &sessions.path().join("session.jsonl"),
    );
    let mut run = agent
        .prompt_with_responses_prewarm("Reply READY")
        .await
        .unwrap();
    assert!(matches!(run.next().await, Some(AgentEvent::TurnStarted)));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = server.first_request.notified() => {},
            event = run.next() => panic!("stalled warmup emitted a generation event: {event:?}"),
        }
    })
    .await
    .expect("warmup should reach the loopback peer");
    drop(run);
    assert_eq!(server.requests().await.len(), 1);
    assert!(agent.session().usage_records().is_empty());
    assert_eq!(agent.session().context().unwrap().len(), 1);
    let requests = server.requests().await;
    assert_eq!(requests[0]["body"]["generate"], false);
}

#[tokio::test]
async fn headless_responses_prewarm_respects_admission_and_default_api_stays_cold() {
    let server = Server::start(Warmup::Complete).await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut denied = agent(
        model(&server.uri, false),
        workspace.path(),
        &sessions.path().join("denied.jsonl"),
    );
    denied.set_max_session_cost_microdollars(Some(0));
    let events = denied
        .prompt_with_responses_prewarm("Reply READY")
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    assert!(server.requests().await.is_empty());
    let mut cold = agent(
        model(&server.uri, false),
        workspace.path(),
        &sessions.path().join("cold.jsonl"),
    );
    assert_eq!(cold.complete("Reply READY").await.unwrap().text, "READY");
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0]["body"].get("generate").is_none());
}

#[tokio::test]
async fn headless_responses_prewarm_skips_http_only_routes() {
    let server = Server::start(Warmup::Complete).await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut model = model(&server.uri, false);
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
    let mut agent = agent(
        model,
        workspace.path(),
        &sessions.path().join("session.jsonl"),
    );
    let events = agent
        .prompt_with_responses_prewarm("Reply READY")
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert_ready(&events);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["transport"], "http");
    assert!(requests[0]["body"].get("generate").is_none());
}
