//! Supported Rust author opt-out, not an altered host or protocol peer.
use super::*;
use crate::typed_tests::start_with_progress;
use octet_agent::extension_process::ExtensionRuntimeError;

#[tokio::test]
async fn a07_author_opt_out_refuses_progress_without_live_callbacks() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start_with_progress(workspace.path(), false).await;
    assert!(!process.supports_feature("request_progress"));
    assert!(process.supports_feature("request_cancellation"));
    let generation = process.health_snapshot().generation;
    let error = process
        .call_tool("typed", record("progress"), process.current_context())
        .await
        .unwrap_err();
    let ExtensionRuntimeError::Remote {
        code,
        message,
        data,
    } = error
    else {
        panic!("expected SDK RPC refusal, got {error:?}");
    };
    assert_eq!(
        (code, message.as_str(), data),
        (-32601, "request_progress was not negotiated", None)
    );
    let prefix = std::fs::read(workspace.path().join("calls.jsonl")).unwrap();
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
    let mut agent = Agent::new(AgentConfig {
        client: octet_ai::AiClient::new(),
        model,
        session: Session::create(workspace.path().join("session.jsonl")).unwrap(),
        system: "Local SDK author opt-out conformance".into(),
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
    let mut run = agent.prompt("Call typed once, then finish.").await.unwrap();
    let (mut started, mut finished, mut terminal, mut progress) = (0, 0, 0, 0);
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ToolStarted { id, .. } => {
                    assert_eq!(id.0, CALL_ID);
                    started += 1;
                }
                AgentEvent::ToolProgress { .. } => progress += 1,
                AgentEvent::ToolFinished { id, result, .. } => {
                    assert_eq!(id.0, CALL_ID);
                    assert_eq!(
                        result.unwrap_err().to_string(),
                        "extension RPC error -32601: request_progress was not negotiated"
                    );
                    finished += 1;
                }
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed));
                    terminal += 1;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("bounded real Agent run");
    drop(run);
    assert_eq!((started, finished, terminal, progress), (1, 1, 1, 0));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(turns.load(Ordering::SeqCst), 2);
    for request in requests {
        assert!(request.body.len() < 64 * 1024);
        let body = std::str::from_utf8(&request.body).unwrap();
        for forbidden in ["typed started", "typed finished", "$/progress"] {
            assert!(!body.contains(forbidden));
        }
        eprintln!("A07 opt-out exact provider request: {body}");
    }
    let healthy = process
        .call_tool("typed", record("healthy"), process.current_context())
        .await
        .unwrap();
    assert!(!healthy.is_error);
    assert_eq!(healthy.content, "typed record");
    assert_eq!(
        healthy.structured_content,
        Some(json!({"name":"healthy","enabled":true,"samples":[0.5],"note":null}))
    );
    let log = std::fs::read(workspace.path().join("calls.jsonl")).unwrap();
    assert!(log.starts_with(&prefix));
    let entries: Vec<Value> = std::str::from_utf8(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["progress", "progress", "healthy"]
    );
    let pid = entries[0]["pid"].as_u64().unwrap();
    assert_ne!(pid, u64::from(std::process::id()));
    assert!(entries.iter().all(|entry| entry["pid"] == pid));
    assert!(process.is_running());
    assert_eq!(process.health_snapshot().generation, generation);
    assert!(!process.supports_feature("request_progress"));
    shutdown(&process, workspace.path()).await;
}
