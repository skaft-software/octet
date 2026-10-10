//! A07 model-boundary oracle: real Agent + real Python SDK + local HTTP/SSE only.
use super::Fixture;
use octet_agent::extension::ExtensionHost;
use octet_agent::tool::ToolProgress;
use octet_agent::{
    Agent, AgentConfig, AgentEvent, EffectBroker, EffectPolicy, FinishReason, SandboxConfig,
    Session,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn turn(tool: bool) -> ResponseTemplate {
    fn frame(event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }
    let mut body = frame(
        "message_start",
        json!({"type":"message_start","message":{"id":"local-a07","usage":{"input_tokens":5,"output_tokens":0}}}),
    );
    let (block, delta, stop) = if tool {
        (
            json!({"type":"tool_use","id":"a07-call","name":"typed_progress_descriptor"}),
            json!({"type":"input_json_delta","partial_json":"{}"}),
            "tool_use",
        )
    } else {
        (
            json!({"type":"text","text":""}),
            json!({"type":"text_delta","text":"A07 complete"}),
            "end_turn",
        )
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
async fn a07_progress_excluded_from_model_transcript() {
    let f = Fixture::start_fixture("progress_fixture.py", &["typed_progress_descriptor"]).await;
    assert!(f.process.negotiated_features().contains("request_progress"));
    let definition = f.process.tool_definitions().into_iter().next().unwrap();
    assert_eq!(
        definition.output_schema.as_ref().unwrap()["properties"]["data"]["properties"]["$blob"]
            ["maxLength"],
        128
    );
    let generation = f.process.health_snapshot().generation;
    let mut host = ExtensionHost::new();
    host.load(&f.process);
    host.finalize_tool_surface();
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            move |_: &wiremock::Request| match count.fetch_add(1, Ordering::SeqCst) {
                0 => turn(true),
                1 => turn(false),
                _ => ResponseTemplate::new(500).set_body_string("unexpected A07 request"),
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
    let mut agent = Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session: Session::create(f.root.join("session.jsonl")).unwrap(),
        system: "Local A07 conformance; no external inference".into(),
        sandbox: SandboxConfig::new(&f.root),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions: host,
        max_turns: Some(2),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let mut run = agent
        .prompt("Publish the fixture descriptor once, then finish.")
        .await
        .unwrap();
    let mut progress = Vec::new();
    let mut result = None;
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ToolProgress {
                    progress: ToolProgress::Status(text),
                    ..
                } => progress.push(text),
                AgentEvent::ToolFinished { result: output, .. } => {
                    let output = output.unwrap();
                    assert!(!output.is_error());
                    assert!(result
                        .replace((
                            output.text.clone(),
                            output.structured_content().unwrap().clone()
                        ))
                        .is_none());
                }
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
    .expect("bounded local Agent turn");
    drop(run);
    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.body_json().unwrap())
        .collect();
    let (summary, structured) = result.expect("one successful typed result");
    let evidence = json!({"host_pid":std::process::id(), "child_log":f.log(),
        "progress":progress, "summary":summary, "structured_content":structured,
        "model_requests":requests});
    let bytes = serde_json::to_vec(&evidence).unwrap();
    assert!(
        bytes.len() <= 128 * 1024,
        "bounded capture; bulk bytes must stay out"
    );
    if let Some(directory) = std::env::var_os("OCTET_PYTHON_VALUES_EVIDENCE_DIR") {
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            PathBuf::from(directory).join("a07-model-capture.json"),
            &bytes,
        )
        .unwrap();
    }
    println!("a07 model capture: {evidence}");
    assert!(completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        progress,
        [
            "a07_ephemeral_step_one [1/2 steps]",
            "a07_ephemeral_step_two [2/2 steps]"
        ]
    );
    assert_eq!(
        structured["data"]["bytes"],
        "A07-private-bulk-payload".len() * 16384
    );
    assert_eq!(
        serde_json::from_str::<Value>(summary.strip_prefix("A07 published descriptor: ").unwrap())
            .unwrap(),
        structured["data"]
    );
    for request in &requests {
        let tool = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "typed_progress_descriptor")
            .unwrap();
        assert_eq!(tool["input_schema"], definition.parameters);
        let text = request.to_string();
        for forbidden in [
            "a07_ephemeral_step_one",
            "a07_ephemeral_step_two",
            "A07-private-bulk-payload",
            "QTA3LXByaXZhdGUtYnVsay1wYXlsb2Fk",
            "octet-transfer-",
            "octet-bulk-",
            "transfer_directory",
            "locator",
        ] {
            assert!(
                !text.contains(forbidden),
                "model request leaked {forbidden}"
            );
        }
    }
    let tool_results: Vec<_> = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().unwrap().iter())
        .filter(|part| part["type"] == "tool_result")
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0]["tool_use_id"], "a07-call");
    assert_eq!(tool_results[0]["is_error"], false);
    assert_eq!(
        tool_results[0]["content"],
        json!([{"type":"text","text":summary}])
    );
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert_eq!(
        f.log()
            .iter()
            .filter(|row| row["event"] == "entered")
            .count(),
        1
    );
    f.shutdown().await;
}
