use super::*;

#[tokio::test]
async fn deepseek_reasoning_session_resumes_on_codex_with_tools() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("deepseek.jsonl");
    let chat_server = MockServer::start().await;
    let tool_turn = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"private DeepSeek reasoning_content\"}}]}\n\n",
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"deepseek.call|foreign-item","type":"function","function":{"name":"read","arguments":"{\"path\":\"lifecycle.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        "\n\ndata: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script {
            bodies: vec![
                tool_turn.into(),
                include_str!("../../../octet-ai/tests/fixtures/openai_chat/reasoning.sse").into(),
            ],
            next: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&chat_server)
        .await;
    std::fs::write(workspace.path().join("lifecycle.txt"), "previous result").unwrap();
    let mut chat = openai_multimodal_model(&chat_server.uri());
    Arc::make_mut(&mut chat.spec).id = ModelId("deepseek/deepseek-chat".into());
    let mut agent = build_agent_from_session_with_model(
        chat,
        workspace.path(),
        Session::create(&session_path).unwrap(),
        ReasoningConfig::Off,
        Some(3),
    );
    assert_eq!(
        agent.complete("inspect the file").await.unwrap().text,
        "Answer: 42"
    );
    drop(agent);
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
        assert!(
            wire.contains("Answer: 42") && wire.contains("previous result"),
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
async fn codex_encrypted_terra_session_resumes_on_sol_with_tools() {
    codex_encrypted_session_resume("codex/gpt-6.1-sol").await;
}

#[tokio::test]
async fn codex_encrypted_terra_session_resumes_on_terra_verbatim_with_tools() {
    codex_encrypted_session_resume("codex/gpt-6.1-terra").await;
}

async fn codex_encrypted_session_resume(target_model: &str) {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("terra.jsonl");
    let tool_path = workspace.path().join("lifecycle.txt");
    std::fs::write(&tool_path, "Terra tool result").unwrap();
    let terra_reasoning = serde_json::json!({
        "type": "reasoning",
        "id": "rs_terra-tool",
        "encrypted_content": "encrypted-Terra-state",
        "future_reasoning_field": {"preserved": true}
    });
    let terra_call = serde_json::json!({
        "type": "function_call", "id": "fc_terra-tool", "call_id": "terra-call",
        "name": "read", "arguments": r#"{"path":"lifecycle.txt"}"#
    });
    let terra_turn: String = [
        serde_json::json!({"type": "response.created", "response": {"id": "terra-tool"}}),
        serde_json::json!({
            "type": "response.output_item.added", "output_index": 0,
            "item": {"type": "reasoning", "id": "rs_terra-tool", "summary": []}
        }),
        serde_json::json!({
            "type": "response.output_item.done", "output_index": 0, "item": terra_reasoning
        }),
        serde_json::json!({
            "type": "response.output_item.added", "output_index": 1, "item": terra_call
        }),
        serde_json::json!({
            "type": "response.output_item.done", "output_index": 1, "item": terra_call
        }),
        serde_json::json!({
            "type": "response.completed", "response": {
                "output": [terra_reasoning, terra_call],
                "usage": {"input_tokens": 9, "output_tokens": 3, "total_tokens": 12}
            }
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect();
    let responses = Arc::new(vec![
        terra_turn,
        responses_text_turn("terra-done", "saved", "response.completed", "terra"),
        responses_tool_turn("resumed-tool", "resumed-call"),
        responses_text_turn("resumed-done", "continued", "response.completed", "done"),
    ]);
    let cursor = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(move |_: &wiremock::Request| {
            let index = cursor.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(responses[index].clone())
        })
        .expect(4)
        .mount(&server)
        .await;
    let mut model = recovery_codex_model(&server.uri());
    Arc::make_mut(&mut model.endpoint).id = EndpointId("openai-codex".into());
    let spec = Arc::make_mut(&mut model.spec);
    spec.endpoint = model.endpoint.id.clone();
    spec.id = ModelId("codex/gpt-6.1-terra".into());
    spec.api_name = "gpt-6.1-terra".into();
    let mut agent = build_agent_from_session_with_model(
        model.clone(),
        workspace.path(),
        Session::create(&session_path).unwrap(),
        ReasoningConfig::Off,
        Some(3),
    );
    assert_eq!(
        agent.complete("inspect the file").await.unwrap().text,
        "saved"
    );
    drop(agent);
    let saved = std::fs::read(&session_path).unwrap();
    let original_context = Session::open(&session_path).unwrap().context().unwrap();
    let terra_assistant = original_context
        .iter()
        .find(|message| {
            matches!(message, Message::Assistant(assistant)
            if assistant.content.iter().any(|part| matches!(part, AssistantPart::Reasoning(_))))
        })
        .unwrap();
    assert!(serde_json::to_string(terra_assistant)
        .unwrap()
        .contains("encrypted-Terra-state"));

    // Pi compatibility: packages/ai/src/api/transform-messages.ts:122-137 keeps
    // same-model encrypted thinking verbatim and drops it on a model switch.
    let same_model = target_model == "codex/gpt-6.1-terra";
    let spec = Arc::make_mut(&mut model.spec);
    spec.id = ModelId(target_model.into());
    spec.api_name = target_model.strip_prefix("codex/").unwrap().into();
    std::fs::write(&tool_path, "resumed tool result").unwrap();
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
    drop(agent);

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 4);
    let original_input = requests[1]["input"].as_array().unwrap();
    assert_eq!(
        original_input
            .iter()
            .find(|item| item["id"] == "rs_terra-tool"),
        Some(&terra_reasoning)
    );
    for body in &requests[2..] {
        assert_eq!(body["model"], target_model.strip_prefix("codex/").unwrap());
        let input = body["input"].as_array().unwrap();
        if same_model {
            assert_eq!(
                input.iter().find(|item| item["id"] == "rs_terra-tool"),
                Some(&terra_reasoning)
            );
        } else {
            let wire = body.to_string();
            assert!(!wire.contains("encrypted-Terra-state"), "{wire}");
            assert!(!wire.contains("rs_terra-tool"), "{wire}");
        }
        let call = input
            .iter()
            .find(|item| item["type"] == "function_call" && item["call_id"] == "terra-call")
            .unwrap();
        assert_eq!(call["name"], "read");
        assert!(input
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == call["call_id"]
                && item["output"].to_string().contains("Terra tool result")));
    }
    assert!(requests[3].to_string().contains("resumed tool result"));
    // Reopen after the resumed tool turn: neither canonical state nor the
    // opaque provider sidecar in the durable prefix may be rewritten.
    assert!(std::fs::read(&session_path).unwrap().starts_with(&saved));
    let resumed_context = Session::open(&session_path).unwrap().context().unwrap();
    assert!(resumed_context.iter().any(|message| {
        serde_json::to_value(message).unwrap() == serde_json::to_value(terra_assistant).unwrap()
    }));
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
        "paired function call to be replayed together",
    ] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    for private in [
        "Reasoning item requires its paired function call",
        "test-api-key",
        "PRIVATE_PROMPT_MUST_NOT_APPEAR",
        "PRIVATE_HEADER_MUST_NOT_APPEAR",
    ] {
        assert!(!diagnostic.contains(private), "{diagnostic}");
    }
}
