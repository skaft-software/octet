use super::*;

#[tokio::test]
async fn deepseek_reasoning_session_resumes_on_codex_with_tools() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("deepseek.jsonl");
    let call_id = octet_ai::ToolCallId("deepseek.call|foreign-item".into());
    let mut session = Session::create(&session_path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("inspect the file".into())],
        })))
        .unwrap();
    // DeepSeek's reasoning_content is canonical readable thinking with no opaque
    // state (octet-ai/tests/fixtures/openai_chat/reasoning.sse).
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            model: ModelId("deepseek/deepseek-chat".into()),
            protocol: Protocol::OpenAiChat,
            content: vec![
                AssistantPart::Reasoning(octet_ai::ReasoningPart {
                    text: Some("private DeepSeek reasoning_content".into()),
                    state: None,
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: "read".into(),
                    arguments_json: r#"{"path":"lifecycle.txt"}"#.into(),
                    async_execution: false,
                    argument_error: None,
                }),
            ],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(octet_ai::ToolResult {
                tool_call_id: call_id.clone(),
                tool_name: Some("read".into()),
                content: vec![octet_ai::ToolResultPart::Text("previous result".into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    drop(session);
    let saved = std::fs::read(&session_path).unwrap();
    std::fs::write(workspace.path().join("lifecycle.txt"), "current result").unwrap();
    let responses = Arc::new(vec![
        responses_tool_turn("codex-tool", "codex-call"),
        responses_text_turn("codex-done", "continued", "response.completed", "done"),
    ]);
    let cursor = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with({
            let cursor = cursor.clone();
            move |_: &wiremock::Request| {
                let index = cursor.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(responses[index].clone())
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let mut model = recovery_codex_model(&server.uri());
    Arc::make_mut(&mut model.endpoint).id = EndpointId("openai-codex".into());
    Arc::make_mut(&mut model.spec).endpoint = model.endpoint.id.clone();
    let mut agent = build_agent_from_session_with_model(
        model,
        workspace.path(),
        Session::open(&session_path).unwrap(),
        ReasoningConfig::Off,
        Some(3),
    );
    let output = agent.complete("continue with tools").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(output.text, "continued");
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    for body in &requests {
        let wire = body.to_string();
        assert!(
            !wire.contains("private DeepSeek reasoning_content"),
            "{wire}"
        );
        assert!(!wire.contains("deepseek.call|foreign-item"), "{wire}");
        let input = body["input"].as_array().unwrap();
        let call = input
            .iter()
            .find(|item| {
                item["name"] == "read" && item["arguments"] == r#"{"path":"lifecycle.txt"}"#
            })
            .unwrap();
        assert!(call.get("id").is_none());
        let normalized = call["call_id"].as_str().unwrap();
        assert!(normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        assert!(input.iter().any(
            |item| item["type"] == "function_call_output" && item["call_id"] == call["call_id"]
        ));
    }
    // Encoding never rewrites the durable prefix, even after new tool turns.
    let after = std::fs::read(&session_path).unwrap();
    assert!(after.starts_with(&saved));
    assert!(serde_json::to_string(&agent.session().context().unwrap())
        .unwrap()
        .contains("private DeepSeek reasoning_content"));
    assert!(requests[1].to_string().contains("current result"));
}

#[tokio::test]
async fn codex_bad_request_shows_structured_details_in_public_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string(include_str!(
            "../../../octet-ai/tests/fixtures/errors/responses_bad_request.json"
        )))
        .expect(1)
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let mut model = recovery_codex_model(&server.uri());
    Arc::make_mut(&mut model.endpoint).auth = Auth::bearer("test-api-key");
    let mut agent = build_agent_from_session_with_model(
        model,
        workspace.path(),
        Session::create(workspace.path().join("error.jsonl")).unwrap(),
        ReasoningConfig::Off,
        Some(1),
    );
    let error = agent
        .complete("PRIVATE_PROMPT_MUST_NOT_APPEAR")
        .await
        .unwrap_err();
    let diagnostic = octet_agent::public_error_diagnostic(&error, "openai-codex", "gpt-6-astra");
    for expected in [
        "status=400",
        "code=invalid_reasoning_pair",
        "type=invalid_request_error",
        "requires its paired function call",
    ] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    for private in [
        "test-api-key",
        "PRIVATE_PROMPT_MUST_NOT_APPEAR",
        "PRIVATE_HEADER_MUST_NOT_APPEAR",
    ] {
        assert!(!diagnostic.contains(private), "{diagnostic}");
    }
}
