//! A07 negative: public SDK opt-out, real API 0.4 negotiation and dispatch.
use super::Fixture;
use octet_agent::extension::ExtensionHost;
use octet_agent::extension_process::ExtensionRuntimeError;
use octet_agent::{
    Agent, AgentConfig, AgentEvent, EffectBroker, EffectPolicy, FinishReason, SandboxConfig,
    Session,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REFUSAL: &str = "API 0.2 feature is not negotiated: request_progress";

fn turn(tool: Option<&str>) -> ResponseTemplate {
    fn frame(event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }
    let mut body = frame(
        "message_start",
        json!({
            "type":"message_start", "message":{"id":"local-a07-negative",
            "usage":{"input_tokens":5,"output_tokens":0}}
        }),
    );
    let (block, delta, stop) = match tool {
        Some(name) => (
            json!({"type":"tool_use","id":format!("a07-{name}"),"name":name}),
            json!({"type":"input_json_delta","partial_json":"{\"value\":41}"}),
            "tool_use",
        ),
        None => (
            json!({"type":"text","text":""}),
            json!({"type":"text_delta","text":"A07 negative complete"}),
            "end_turn",
        ),
    };
    body += &frame(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":block}),
    );
    body += &frame(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":delta}),
    );
    body += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    body += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":3}}),
    );
    body += &frame("message_stop", json!({"type":"message_stop"}));
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

#[tokio::test]
async fn a07_unnegotiated_progress_refused() {
    let names = ["unnegotiated_progress", "healthy"];
    // Same supported API 0.4 manifest as the positive fixtures. The actual SDK
    // selects only mandatory features through its public constructor, not by
    // tampering with the initialize request or private negotiated state.
    let f = Fixture::start_fixture("progress_unnegotiated_fixture.py", &names).await;
    let generation = f.process.health_snapshot().generation;
    assert_eq!(
        f.process
            .negotiated_features()
            .into_iter()
            .collect::<Vec<_>>(),
        ["content_parts", "request_cancellation"]
    );
    let error = f.call(names[0], json!({"value":41})).await.unwrap_err();
    match error {
        ExtensionRuntimeError::Remote {
            code,
            message,
            data,
        } => {
            assert_eq!(code, -32601);
            assert_eq!(message, REFUSAL);
            assert!(data.is_none());
        }
        other => panic!("expected precise SDK feature refusal, got {other:?}"),
    }

    // Use public Agent dispatch and drain its live event stream. A null progress
    // sink or private tool snapshot would not establish this production boundary.
    let mut host = ExtensionHost::new();
    host.load(&f.process);
    host.finalize_tool_surface();
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            move |_: &wiremock::Request| match count.fetch_add(1, Ordering::SeqCst) {
                0 => turn(Some("unnegotiated_progress")),
                1 => turn(Some("healthy")),
                2 => turn(None),
                _ => ResponseTemplate::new(500).set_body_string("unexpected A07 negative request"),
            },
        )
        .expect(3)
        .mount(&server)
        .await;
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::AnthropicMessages;
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.base_url = format!("{}/", server.uri()).parse().unwrap();
    endpoint.auth = octet_ai::Auth::bearer("local-scripted-no-inference");
    endpoint.transport = octet_ai::EndpointTransport::Http;
    endpoint.timeout = Duration::from_secs(3);
    let mut agent = Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session: Session::create(f.root.join("negative-session.jsonl")).unwrap(),
        system: "Local A07 negative conformance; no external inference".into(),
        sandbox: SandboxConfig::new(&f.root),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions: host,
        max_turns: Some(3),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let mut run = agent
        .prompt("Attempt progress, run the healthy control, then finish.")
        .await
        .unwrap();
    let mut results = Vec::new();
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ToolProgress { .. } => panic!("unnegotiated progress reached Agent"),
                AgentEvent::ToolFinished { result, .. } => results.push(result),
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed));
                    assert!(!completed);
                    completed = true;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("bounded negative Agent turn");
    drop(run);
    assert!(completed);
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].as_ref().unwrap_err().to_string(),
        format!("extension RPC error -32601: {REFUSAL}")
    );
    let result = results[1].as_ref().unwrap();
    assert!(!result.is_error());
    assert_eq!(result.text, "Healthy typed result.");
    assert_eq!(result.structured_content(), Some(&json!(42)));
    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.body_json().unwrap())
        .collect();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let text = request.to_string();
        assert!(!text.contains("a07_unnegotiated_must_not_emit"));
        assert!(!text.contains("$/progress"));
    }
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert!(f.process.is_running());
    let log = f.log();
    assert_eq!(
        log.iter()
            .map(|row| row["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["started", "attempt", "refused", "attempt", "refused", "healthy"]
    );
    for row in log.iter().filter(|row| row["event"] == "attempt") {
        assert_eq!(row["host_offered_progress"], true);
        assert_eq!(
            row["negotiated_features"],
            json!(["content_parts", "request_cancellation"])
        );
    }
    for row in log.iter().filter(|row| row["event"] == "refused") {
        assert_eq!(row["error"], json!({"code":-32601,"message":REFUSAL}));
    }
    assert!(log.iter().all(|row| row["pid"] == log[0]["pid"]));
    println!(
        "a07 unnegotiated: {}",
        json!({
            "host_pid":std::process::id(),"child_log":log,"generation":generation,
            "negotiated_features":f.process.negotiated_features(),
            "refusal":{"code":-32601,"message":REFUSAL},"progress_callbacks":0,
            "model_requests":requests
        })
    );
    f.shutdown().await;
}
