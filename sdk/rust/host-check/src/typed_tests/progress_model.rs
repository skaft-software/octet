//! A07: real SDK -> ExtensionProcess -> registered tool -> Agent -> HTTP provider.
//! Only the provider's two responses are scripted; neither SDK nor host is a fake.
mod unnegotiated;
use super::{record, shutdown, start};
use octet_agent::{
    Agent, AgentConfig, AgentEvent, EffectBroker, EffectPolicy, EntryValue, ExtensionHost,
    FinishReason, OutputChannel, SandboxConfig, Session, ToolProgress,
};
use octet_ai::{Message, UserPart};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CALL_ID: &str = "sdk-a07-call";
const STATUSES: [&str; 2] = ["typed started", "typed finished [2/2 steps]"];

fn provider_turn(tool_call: bool) -> ResponseTemplate {
    let frame = |event: &str, value: Value| format!("event: {event}\ndata: {value}\n\n");
    let mut body = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"sdk-a07","usage":{"input_tokens":5,"output_tokens":0}}}),
    );
    if tool_call {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":CALL_ID,"name":"typed"}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":record("progress").to_string()}}),
        );
    } else {
        body += &frame(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        body += &frame(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
        );
    }
    body += &frame(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    );
    body += &frame(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":if tool_call {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":3}}),
    );
    body += &frame("message_stop", json!({"type":"message_stop"}));
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

#[tokio::test]
async fn a07_sdk_progress_is_ephemeral_in_actual_agent_provider_requests() {
    let workspace = tempfile::tempdir().unwrap();
    // The existing SDK fixture sets its HOME to this private workspace and logs
    // every domain entry with its real child PID. Missing binaries fail start.
    let process = start(workspace.path()).await;
    assert!(process.supports_feature("request_progress"));
    let generation = process.health_snapshot().generation;
    let definition = process.tool_definitions().remove(0);
    let mut extensions = ExtensionHost::new();
    process.register_dynamic_tool_catalog(&mut extensions);
    extensions.finalize_tool_surface();

    let server = MockServer::start().await;
    let turns = Arc::new(AtomicUsize::new(0));
    let counter = turns.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            move |_: &wiremock::Request| match counter.fetch_add(1, Ordering::SeqCst) {
                0 => provider_turn(true),
                1 => provider_turn(false),
                _ => ResponseTemplate::new(400).set_body_string("unexpected provider request"),
            },
        )
        .expect(2)
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
    let session_path = workspace.path().join("session.jsonl");
    let mut agent = Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session: Session::create(&session_path).unwrap(),
        system: "Local SDK progress projection conformance".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(3),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let expected = json!({"name":"progress","enabled":true,"samples":[0.5],"note":null});
    let mut run = agent.prompt("Call typed once, then finish.").await.unwrap();
    let mut statuses = Vec::new();
    let (mut started, mut finished, mut terminals) = (0, 0, 0);
    let mut answer = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ToolStarted { id, name, .. } => {
                    assert_eq!(id.0, CALL_ID);
                    assert_eq!(name, "typed");
                    started += 1;
                }
                AgentEvent::ToolProgress { id, progress } => {
                    assert_eq!(id.0, CALL_ID);
                    assert_eq!((started, finished), (1, 0));
                    let ToolProgress::Status(status) = progress else {
                        panic!("unexpected SDK progress: {progress:?}");
                    };
                    statuses.push(status);
                }
                AgentEvent::ToolFinished { id, result, .. } => {
                    assert_eq!(id.0, CALL_ID);
                    assert_eq!(statuses, STATUSES);
                    let output = result.expect("the registered SDK tool must succeed");
                    assert!(!output.is_error());
                    assert_eq!(output.text, "typed record");
                    assert_eq!(output.structured_content(), Some(&expected));
                    finished += 1;
                }
                AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text,
                } => {
                    answer.push_str(&text);
                }
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                    terminals += 1;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("bounded local provider/SDK Agent run");
    drop(run);
    assert_eq!((started, finished, terminals), (1, 1, 1));
    assert_eq!(answer, "done");
    eprintln!("A07 live Agent statuses: {}", json!(statuses));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(turns.load(Ordering::SeqCst), 2);
    let mut bodies = Vec::new();
    for (index, request) in requests.iter().enumerate() {
        assert!(request.body.len() <= 64 * 1024, "bounded request evidence");
        let exact_body = std::str::from_utf8(&request.body).unwrap();
        eprintln!("A07 exact local provider request {index}: {exact_body}");
        // Do not merely assert that the final result omits progress: inspect
        // every actual provider request, including its full model messages.
        for forbidden in ["typed started", "typed finished", "$/progress"] {
            assert!(
                !exact_body.contains(forbidden),
                "progress leaked: {forbidden}"
            );
        }
        bodies.push(request.body_json::<Value>().unwrap());
    }
    let projected = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "typed")
        .expect("real SDK schema projected");
    assert_eq!(projected["input_schema"], definition.parameters);
    let results: Vec<_> = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter(|part| part["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["tool_use_id"], CALL_ID);
    assert_ne!(results[0]["is_error"], true);
    assert_eq!(
        results[0]["content"],
        json!([{"type":"text","text":"typed record"}])
    );

    // Structured content is retained as typed session details, not silently
    // stringified into the explicit model-facing summary.
    drop(agent);
    let reopened = Session::open(&session_path).unwrap();
    let retained: Vec<_> = reopened.entries().iter().filter(|entry| {
        matches!(&entry.value, EntryValue::Message(Message::User(message)) if message.content.iter().any(|part| matches!(part, UserPart::ToolResult(result) if result.tool_call_id.0 == CALL_ID)))
    }).collect();
    assert_eq!(retained.len(), 1);
    let details = retained[0]
        .metadata
        .as_ref()
        .unwrap()
        .tool_output
        .as_ref()
        .unwrap();
    assert_eq!(details.structured_content(), Some(&expected));
    eprintln!(
        "A07 retained typed output: {}",
        details.structured_content().unwrap()
    );
    let journal = std::fs::read_to_string(&session_path).unwrap();
    for forbidden in ["typed started", "typed finished", "$/progress"] {
        assert!(
            !journal.contains(forbidden),
            "progress entered durable session"
        );
    }
    let calls = std::fs::read_to_string(workspace.path().join("calls.jsonl")).unwrap();
    let calls: Vec<Value> = calls
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "one genuine SDK domain invocation, no replay"
    );
    assert_eq!(calls[0]["name"], "progress");
    assert!(calls[0]["pid"].as_u64().unwrap() > 0);
    assert_ne!(
        calls[0]["pid"].as_u64().unwrap(),
        u64::from(std::process::id())
    );
    assert_eq!(process.health_snapshot().generation, generation);
    assert!(process.is_running());
    shutdown(&process, workspace.path()).await;
}
