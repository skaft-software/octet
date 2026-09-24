use super::*;
use octet_agent::EntryMetadata;
use octet_ai::types::ReasoningOptions;
use octet_ai::{ReasoningEffort, ResponsesFeatures};

fn model(uri: &str) -> Model {
    let mut model = scripted_responses_model(uri);
    let features = ResponsesFeatures {
        async_tools: true,
        reasoning_effort_updates: true,
        ..Default::default()
    };
    let spec = Arc::make_mut(&mut model.spec);
    spec.capabilities.responses_features = features;
    spec.capabilities.reasoning = Some(ReasoningCapability {
        options: Some(ReasoningOptions {
            values: vec!["none".into(), "low".into(), "medium".into(), "high".into()],
            default: Some("low".into()),
        }),
        control: ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: true,
        effort_budgets: None,
        openai_chat_mode: Default::default(),
        min_effort: ReasoningEffort::Low,
        max_effort: ReasoningEffort::High,
    });
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features = features;
    model
}

#[tokio::test]
async fn missing_responses_sidecar_replays_canonically_with_effective_reasoning() {
    let server = MockServer::start().await;
    let call_id = "call_missing_output";
    let arguments = r#"{"path":"lifecycle.txt"}"#;
    let first = [
        serde_json::json!({"type": "response.created", "response": {"id": "first"}}),
        serde_json::json!({
            "type": "response.output_item.added", "output_index": 0,
            "item": {"id": "fc_first", "type": "function_call", "call_id": call_id, "name": "read"}
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.done", "output_index": 0,
            "arguments": arguments
        }),
        serde_json::json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 9, "output_tokens": 3, "total_tokens": 12}}
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect();
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![
                first,
                responses_text_turn("second", "done", "response.completed", "second"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("lifecycle.txt"), "readable").unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let model = model(&server.uri());
    let mut agent = build_agent_with_reasoning(
        model.clone(),
        &sessions.path().join("session.jsonl"),
        workspace.path(),
        ReasoningConfig::Effort(ReasoningEffort::Low),
        Some(4),
    );
    agent
        .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::Low))
        .unwrap();
    agent
        .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::High))
        .unwrap();
    let mut run = agent.prompt("read the file").await.unwrap();
    let events = collect(&mut run).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::RunFinished {
                reason: FinishReason::Completed,
                ..
            }
        )),
        "{events:?}"
    );
    drop(run);
    assert!(agent
        .session()
        .responses_replay_items(&model.endpoint.id, &model.spec.id)
        .unwrap()
        .is_none());

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["reasoning"]["effort"], "low");
    assert!(requests[0]["input"].as_array().unwrap().iter().any(|item| {
        item["type"] == "configuration_update" && item["reasoning"]["effort"] == "high"
    }));
    assert_eq!(requests[1]["reasoning"]["effort"], "high");
    let input = requests[1]["input"].as_array().unwrap();
    assert!(!input
        .iter()
        .any(|item| item["type"] == "configuration_update"));
    assert!(input.iter().any(|item| item["type"] == "function_call"));
    assert!(input
        .iter()
        .any(|item| item["type"] == "function_call_output"));

    drop(agent);
    let mut warm_model = model;
    Arc::make_mut(&mut warm_model.endpoint).transport =
        octet_ai::EndpointTransport::WebSocketPreferred;
    let resumed = build_agent_from_session_with_model(
        warm_model,
        workspace.path(),
        Session::open(sessions.path().join("session.jsonl")).unwrap(),
        ReasoningConfig::Off,
        Some(4),
    );
    let (_, _, warm) = resumed.responses_prewarm_request().unwrap().unwrap();
    assert_eq!(
        warm.reasoning,
        ReasoningConfig::Effort(ReasoningEffort::High)
    );
    assert!(warm
        .responses
        .as_ref()
        .is_none_or(|options| options.input.is_none()));
}

#[tokio::test]
async fn reasoning_control_pins_baseline_coalesces_and_resumes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![
                responses_text_turn("one", "first", "response.completed", "one"),
                responses_text_turn("two", "second", "response.completed", "two"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let path = sessions.path().join("session.jsonl");
    let model = model(&server.uri());
    let mut agent = build_agent_with_reasoning(
        model.clone(),
        &path,
        workspace.path(),
        ReasoningConfig::Effort(ReasoningEffort::Low),
        Some(4),
    );
    let mut run = agent.prompt("answer").await.unwrap();
    let control = run.control();
    let mut changed = false;
    while let Some(event) = run.next().await {
        if matches!(event, AgentEvent::TurnStarted) && !changed {
            control
                .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::High))
                .await
                .unwrap();
            control
                .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::Medium))
                .await
                .unwrap();
            changed = true;
        }
        if let AgentEvent::RunFinished { reason, .. } = event {
            assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
        }
    }
    drop(run);
    assert_eq!(
        agent.reasoning(),
        &ReasoningConfig::Effort(ReasoningEffort::Medium)
    );
    assert!(control
        .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::High))
        .await
        .is_err());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(
            request.body_json::<serde_json::Value>().unwrap()["reasoning"]["effort"],
            "low"
        );
    }
    let input = requests[1].body_json::<serde_json::Value>().unwrap()["input"]
        .as_array()
        .unwrap()
        .clone();
    let updates: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "configuration_update")
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["reasoning"]["effort"], "medium");
    assert!(
        input
            .iter()
            .position(|item| item["type"] == "message" && item["role"] == "assistant")
            .unwrap()
            < input
                .iter()
                .position(|item| item["type"] == "configuration_update")
                .unwrap()
    );
    drop(agent);
    let mut warm_model = model.clone();
    Arc::make_mut(&mut warm_model.endpoint).transport =
        octet_ai::EndpointTransport::WebSocketPreferred;
    let resumed = build_agent_from_session_with_model(
        warm_model,
        workspace.path(),
        Session::open(&path).unwrap(),
        ReasoningConfig::Off,
        Some(4),
    );
    let (_, _, warm) = resumed.responses_prewarm_request().unwrap().unwrap();
    assert_eq!(
        warm.reasoning,
        ReasoningConfig::Effort(ReasoningEffort::Low)
    );
    let actual_wire = requests[1].body_json::<serde_json::Value>().unwrap();
    for tool in &warm.tools {
        let actual = actual_wire["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["name"] == tool.name)
            .unwrap();
        assert_eq!(
            tool.async_execution,
            actual["async"].as_bool().unwrap_or(false)
        );
    }
    assert!(warm.tools.iter().any(|tool| tool.async_execution));
    assert_eq!(
        resumed.reasoning(),
        &ReasoningConfig::Effort(ReasoningEffort::Medium)
    );
    assert_eq!(
        resumed
            .session()
            .responses_reasoning(&model.endpoint.id, &model.spec.id)
            .unwrap(),
        Some((
            ReasoningConfig::Effort(ReasoningEffort::Low),
            ReasoningConfig::Effort(ReasoningEffort::Medium)
        ))
    );
}

struct AsyncRead {
    release: Arc<tokio::sync::Notify>,
    executed: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Tool for AsyncRead {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "read".into(),
            description: "bounded test lookup".into(),
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
            constrained_sampling: None,
            async_execution: true,
        }
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }
    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        self.release.notified().await;
        Ok(ToolOutput::new("lookup-result"))
    }
}

fn async_tool_turn() -> String {
    responses_tool_turn("lookup", "original-call")
        .lines()
        .map(|line| {
            if let Some(data) = line.strip_prefix("data: ") {
                let mut value: serde_json::Value = serde_json::from_str(data).unwrap();
                if value["item"]["type"] == "function_call" {
                    value["item"]["async"] = true.into();
                }
                if let Some(output) = value["response"]["output"].as_array_mut() {
                    for item in output {
                        if item["type"] == "function_call" {
                            item["async"] = true.into();
                        }
                    }
                }
                format!("data: {value}\n")
            } else {
                "\n".into()
            }
        })
        .collect()
}

#[tokio::test]
async fn async_lookup_overlaps_next_response_and_returns_original_id_once() {
    let server = MockServer::start().await;
    let release = Arc::new(tokio::sync::Notify::new());
    let executed = Arc::new(AtomicUsize::new(0));
    struct AsyncScript {
        next: AtomicUsize,
        release: Arc<tokio::sync::Notify>,
    }
    impl Respond for AsyncScript {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let index = self.next.fetch_add(1, Ordering::SeqCst);
            let request: serde_json::Value = request.body_json().unwrap();
            let results = request["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .count();
            let body = match index {
                0 => async_tool_turn(),
                1 => {
                    assert_eq!(results, 0, "generation overlaps the still-pending lookup");
                    self.release.notify_one();
                    responses_text_turn(
                        "independent",
                        "independent answer",
                        "response.completed",
                        "two",
                    )
                }
                2 => {
                    assert_eq!(results, 1);
                    let output = request["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["type"] == "function_call_output")
                        .unwrap();
                    assert_eq!(output["call_id"], "original-call");
                    responses_text_turn(
                        "done",
                        "lookup incorporated",
                        "response.completed",
                        "three",
                    )
                }
                _ => panic!("unexpected continuation"),
            };
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream")
        }
    }
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(AsyncScript {
            next: AtomicUsize::new(0),
            release: release.clone(),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut extensions = ExtensionHost::new();
    extensions.tool(AsyncRead {
        release,
        executed: executed.clone(),
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: model(&server.uri()),
        session: Session::create(sessions.path().join("async.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::Controlled),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: Default::default(),
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let mut run = agent.prompt("lookup and independent work").await.unwrap();
    let events = tokio::time::timeout(Duration::from_secs(5), collect(&mut run))
        .await
        .unwrap();
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolFinished { .. }))
            .count(),
        1
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn local_compaction_rebases_only_on_success_and_fork_keeps_pin() {
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let route = model("http://127.0.0.1:1");
    let path = sessions.path().join("source.jsonl");
    let mut agent = build_agent_with_reasoning(
        route.clone(),
        &path,
        workspace.path(),
        ReasoningConfig::Effort(ReasoningEffort::Low),
        Some(1),
    );
    agent
        .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::Low))
        .unwrap();
    agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("old".into())],
        })))
        .unwrap();
    agent
        .set_reasoning(ReasoningConfig::Effort(ReasoningEffort::High))
        .unwrap();
    let first_kept = agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("retain".into())],
        })))
        .unwrap();
    let before = agent
        .session()
        .responses_reasoning(&route.endpoint.id, &route.spec.id)
        .unwrap();
    assert!(agent
        .session_mut()
        .compact("summary", EntryId("missing".into()))
        .is_err());
    assert_eq!(
        agent
            .session()
            .responses_reasoning(&route.endpoint.id, &route.spec.id)
            .unwrap(),
        before
    );
    assert!(
        agent.compact_responses_native().await.is_err(),
        "standalone compact rejects before network"
    );
    agent.session_mut().compact("summary", first_kept).unwrap();
    let rebased = Some((
        ReasoningConfig::Effort(ReasoningEffort::High),
        ReasoningConfig::Effort(ReasoningEffort::High),
    ));
    assert_eq!(
        agent
            .session()
            .responses_reasoning(&route.endpoint.id, &route.spec.id)
            .unwrap(),
        rebased
    );
    let fork = agent
        .session()
        .fork_to(
            sessions.path().join("fork.jsonl"),
            agent.session().head_ref(),
        )
        .unwrap();
    assert_eq!(
        fork.responses_reasoning(&route.endpoint.id, &route.spec.id)
            .unwrap(),
        rebased
    );
    assert!(!fork
        .responses_replay_items(&route.endpoint.id, &route.spec.id)
        .unwrap()
        .unwrap()
        .iter()
        .any(|item| matches!(item, octet_ai::ResponsesReplayItem::ConfigurationUpdate(_))));
    assert_eq!(
        serde_json::to_value(fork.context().unwrap()).unwrap(),
        serde_json::to_value(agent.session().context().unwrap()).unwrap()
    );
}

#[tokio::test]
async fn unqualified_reasoning_control_and_provider_authored_update_are_rejected() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let route = scripted_responses_model(&server.uri());
    let mut agent = build_agent_with_reasoning(
        route.clone(),
        &sessions.path().join("source.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(1),
    );
    let run = agent.prompt("not driven").await.unwrap();
    assert!(run
        .control()
        .set_reasoning(ReasoningConfig::Off)
        .await
        .is_err());
    drop(run);
    assert!(server.received_requests().await.unwrap().is_empty());
    let output = octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(
        serde_json::json!({"type":"configuration_update","reasoning":{"effort":"high"}}),
    )
    .unwrap()]);
    assert!(agent
        .session_mut()
        .append_assistant_turn(
            AssistantMessage {
                content: vec![AssistantPart::Text("answer".into())],
                model: route.spec.id.clone(),
                protocol: Protocol::OpenAiResponses
            },
            route.endpoint.id.clone(),
            route.spec.id.clone(),
            Usage::default(),
            None,
            octet_ai::StopReason::EndTurn,
            Some(output)
        )
        .is_err());
}

async fn ws_events(
    socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    body: &str,
    terminal: bool,
) {
    for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        let is_terminal = matches!(
            event["type"].as_str(),
            Some("response.completed" | "response.incomplete")
        );
        if is_terminal == terminal {
            socket
                .send(WebSocketMessage::Text(line.to_owned().into()))
                .await
                .unwrap();
        }
    }
}
async fn ws_command(
    socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
) -> serde_json::Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            WebSocketMessage::Text(text) => return serde_json::from_str(&text).unwrap(),
            WebSocketMessage::Ping(payload) => {
                socket.send(WebSocketMessage::Pong(payload)).await.unwrap()
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

#[tokio::test]
async fn native_steering_persists_intent_before_dispatch_and_orders_two_billed_segments() {
    native_steering_ordering(false).await;
}

#[tokio::test]
async fn native_late_acceptance_materializes_before_its_exact_successor() {
    native_steering_ordering(true).await;
}

async fn native_steering_ordering(late_acceptance: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut route = model(&format!("http://{}/", listener.local_addr().unwrap()));
    Arc::make_mut(&mut route.spec)
        .capabilities
        .responses_features
        .steering = true;
    Arc::make_mut(&mut route.endpoint)
        .runtime
        .responses_features
        .steering = true;
    Arc::make_mut(&mut route.endpoint).transport = octet_ai::EndpointTransport::WebSocketPreferred;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("native.jsonl");
    let inspect = session_path.clone();
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        assert_eq!(ws_command(&mut socket).await["type"], "response.create");
        let first =
            responses_text_turn("first", "completed-prefix", "response.incomplete", "first")
                .replace("max_output_tokens", "steered");
        ws_events(&mut socket, &first, false).await;
        let steer = ws_command(&mut socket).await;
        assert_eq!(steer["type"], "response.steer");
        assert_eq!(steer["input"], "use corrected requirements");
        let persisted = std::fs::read_to_string(&inspect).unwrap();
        assert!(persisted.contains("responses_steering"));
        assert!(persisted.contains("use corrected requirements"));
        let mut second =
            responses_text_turn("second", "corrected-answer", "response.completed", "second");
        if late_acceptance {
            ws_events(&mut socket, &first, true).await;
            let created = second
                .lines()
                .find(|line| line.contains("response.created"))
                .unwrap();
            ws_events(&mut socket, created, false).await;
            second = second
                .lines()
                .filter(|line| !line.contains("response.created"))
                .collect::<Vec<_>>()
                .join("\n");
        }
        socket.send(WebSocketMessage::Text(serde_json::json!({"type":"response.steer.accepted","steer":{"id":"s1","previous_response_id":"first"}}).to_string().into())).await.unwrap();
        if !late_acceptance {
            ws_events(&mut socket, &first, true).await;
        }
        ws_events(&mut socket, &second, false).await;
        ws_events(&mut socket, &second, true).await;
    });
    let mut agent = build_agent_with_reasoning(
        route.clone(),
        &session_path,
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );
    let mut run = agent.prompt("original task").await.unwrap();
    let control = run.control();
    let mut submitted = false;
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            if matches!(event, AgentEvent::TurnStarted) && !submitted {
                control.steer("use corrected requirements").await.unwrap();
                submitted = true;
            }
            events.push(event);
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::TurnFinished { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::SteeringDelivered { .. }))
            .count(),
        1
    );
    drop(run);
    assert_eq!(agent.session().usage_records().len(), 2);
    let messages = agent.session().context().unwrap();
    assert_eq!(messages.len(), 4);
    assert!(
        matches!(&messages[1], Message::Assistant(assistant) if matches!(&assistant.content[0], AssistantPart::Text(text) if text == "completed-prefix"))
    );
    assert!(
        matches!(&messages[2], Message::User(user) if matches!(&user.content[0], UserPart::Text(text) if text == "use corrected requirements"))
    );
    assert!(!agent.session().has_uncertain_usage());
    server.await.unwrap();
}

#[tokio::test]
async fn native_required_tool_input_returns_on_same_socket_without_repeating_steer() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut route = model(&format!("http://{}/", listener.local_addr().unwrap()));
    Arc::make_mut(&mut route.spec)
        .capabilities
        .responses_features
        .steering = true;
    Arc::make_mut(&mut route.endpoint)
        .runtime
        .responses_features
        .steering = true;
    Arc::make_mut(&mut route.endpoint).transport = octet_ai::EndpointTransport::WebSocketPreferred;
    let server = tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        ws_command(&mut socket).await;
        let first = responses_tool_turn("first", "sync-call");
        ws_events(&mut socket, &first, false).await;
        assert_eq!(ws_command(&mut socket).await["type"], "response.steer");
        socket.send(WebSocketMessage::Text(serde_json::json!({"type":"response.steer.accepted","steer":{"id":"s1","previous_response_id":"first"}}).to_string().into())).await.unwrap();
        ws_events(&mut socket, &first, true).await;
        socket.send(WebSocketMessage::Text(serde_json::json!({"type":"response.steer.pending","steer":{"id":"s1","previous_response_id":"first"},"reason":"waiting_for_required_input","required_input":[{"type":"function_call_output","call_id":"sync-call","name":"read"}]}).to_string().into())).await.unwrap();
        let continuation = ws_command(&mut socket).await;
        assert_eq!(continuation["type"], "response.create");
        assert_eq!(continuation["previous_response_id"], "first");
        assert_eq!(continuation["input"].as_array().unwrap().len(), 1);
        assert_eq!(continuation["input"][0]["type"], "function_call_output");
        assert_eq!(continuation["input"][0]["call_id"], "sync-call");
        assert!(!continuation.to_string().contains("correct once"));
        let second = responses_text_turn("second", "done", "response.completed", "second");
        ws_events(&mut socket, &second, false).await;
        ws_events(&mut socket, &second, true).await;
    });
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("lifecycle.txt"), "durable read").unwrap();
    let mut agent = build_agent_with_reasoning(
        route,
        &sessions.path().join("native.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );
    let mut run = agent.prompt("read").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let mut submitted = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            if matches!(event, AgentEvent::TurnStarted) && !submitted {
                control.steer("correct once").await.unwrap();
                submitted = true;
            }
            events.push(event);
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolFinished { .. }))
            .count(),
        1
    );
    server.await.unwrap();
}

#[tokio::test]
async fn misalignment_terminal_cancels_pending_async_job_without_retry_or_reexecution() {
    let server = MockServer::start().await;
    struct MonitorScript(AtomicUsize);
    impl Respond for MonitorScript {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            match self.0.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(200).set_body_string(async_tool_turn()).insert_header("content-type", "text/event-stream"),
                1 => ResponseTemplate::new(403).set_body_json(serde_json::json!({"error":{"code":"misalignment_policy_violation","message":"blocked"}})),
                _ => panic!("must not retry monitoring terminal"),
            }
        }
    }
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(MonitorScript(AtomicUsize::new(0)))
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let executed = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.tool(AsyncRead {
        release: Arc::new(tokio::sync::Notify::new()),
        executed: executed.clone(),
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: model(&server.uri()),
        session: Session::create(sessions.path().join("async.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::Controlled),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: Default::default(),
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let mut run = agent.prompt("lookup").await.unwrap();
    let events = tokio::time::timeout(Duration::from_secs(5), collect(&mut run))
        .await
        .unwrap();
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolFinished { .. }))
            .count(),
        1
    );
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    drop(run);
    let context = serde_json::to_value(agent.session().context().unwrap())
        .unwrap()
        .to_string();
    assert_eq!(
        context.matches("original-call").count(),
        2,
        "one original call and one paired result"
    );
}

#[tokio::test]
async fn native_restart_fails_closed_at_each_intent_materialization_boundary() {
    for boundary in 0..3 {
        let workspace = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let path = sessions.path().join("crash.jsonl");
        let route = model("http://127.0.0.1:1/");
        let mut session = Session::create(&path).unwrap();
        let input = UserMessage {
            content: vec![UserPart::Text("identical input is not an identity".into())],
        };
        session
            .append(EntryValue::ResponsesSteering {
                endpoint: route.endpoint.id.clone(),
                model: route.spec.id.clone(),
                operation: "run:crash:0".into(),
                local_id: 0,
                input: Some(input.clone()),
                state: None,
                completed: None,
            })
            .unwrap();
        if boundary >= 1 {
            session
                .append(EntryValue::ResponsesSteering {
                    endpoint: route.endpoint.id.clone(),
                    model: route.spec.id.clone(),
                    operation: "run:crash:0".into(),
                    local_id: 0,
                    input: None,
                    completed: None,
                    state: Some(octet_ai::SteeringUpdate {
                        local_id: 0,
                        steer_id: Some("s1".into()),
                        previous_response_id: Some("prefix".into()),
                        state: octet_ai::SteeringState::Applied {
                            response_id: "successor".into(),
                        },
                    }),
                })
                .unwrap();
        }
        if boundary >= 2 {
            session
                .append_with_metadata(
                    EntryValue::Message(Message::User(input)),
                    Some(EntryMetadata {
                        native_steering: Some(("run:crash:0".into(), 0)),
                        ..Default::default()
                    }),
                )
                .unwrap();
        }
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(reopened.has_uncertain_usage(), "boundary {boundary}");
        assert!(reopened.has_unsettled_native_steering());
        let linked = reopened
            .entries()
            .iter()
            .filter(|entry| {
                entry.metadata.as_ref().is_some_and(|metadata| {
                    metadata.native_steering == Some(("run:crash:0".into(), 0))
                })
            })
            .count();
        assert_eq!(linked, usize::from(boundary >= 2));
        let count = reopened.entries().len();
        let mut resumed = build_agent_from_session_with_model(
            route,
            workspace.path(),
            reopened,
            ReasoningConfig::Off,
            Some(4),
        );
        assert!(resumed.prompt("must not replay").await.is_err());
        assert_eq!(resumed.session().entries().len(), count);
    }
}

#[tokio::test]
async fn native_disconnect_and_abrupt_drop_preserve_uncertainty_without_replay() {
    for abrupt_drop in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut route = model(&format!("http://{}/", listener.local_addr().unwrap()));
        Arc::make_mut(&mut route.spec)
            .capabilities
            .responses_features
            .steering = true;
        Arc::make_mut(&mut route.endpoint)
            .runtime
            .responses_features
            .steering = true;
        Arc::make_mut(&mut route.endpoint).transport =
            octet_ai::EndpointTransport::WebSocketPreferred;
        let dispatched = Arc::new(tokio::sync::Notify::new());
        let notify = dispatched.clone();
        let server = tokio::spawn(async move {
            let mut socket = accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap();
            ws_command(&mut socket).await;
            ws_events(
                &mut socket,
                &responses_text_turn("first", "provisional", "response.completed", "first"),
                false,
            )
            .await;
            assert_eq!(ws_command(&mut socket).await["type"], "response.steer");
            notify.notify_one();
            if !abrupt_drop {
                socket.close(None).await.unwrap();
            } else {
                let _ = socket.next().await;
            }
        });
        let workspace = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let path = sessions.path().join("native.jsonl");
        let mut agent = build_agent_with_reasoning(
            route.clone(),
            &path,
            workspace.path(),
            ReasoningConfig::Off,
            Some(4),
        );
        let mut run = agent.prompt("original").await.unwrap();
        let control = run.control();
        let mut events = Vec::new();
        let mut submitted = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    _ = dispatched.notified(), if abrupt_drop => break,
                    event = run.next() => {
                        let Some(event) = event else { break; };
                        if matches!(event, AgentEvent::TurnStarted) && !submitted { control.steer("do not duplicate").await.unwrap(); submitted = true; }
                        events.push(event);
                    }
                }
            }
        }).await.unwrap();
        if !abrupt_drop {
            assert!(matches!(
                assert_single_run_finished(&events),
                FinishReason::Failed(_)
            ));
        }
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
        drop(run);
        drop(agent);
        let reopened = Session::open(&path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert!(reopened.has_unsettled_native_steering());
        assert_eq!(
            reopened.usage_records().len(),
            0,
            "unknown is not a zero-usage completion"
        );
        let mut resumed = build_agent_from_session_with_model(
            route,
            workspace.path(),
            reopened,
            ReasoningConfig::Off,
            Some(4),
        );
        assert!(resumed.prompt("resume").await.is_err());
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn async_drop_and_restart_never_redispatches_committed_job() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![
                async_tool_turn(),
                responses_text_turn(
                    "resumed",
                    "indeterminate lookup acknowledged",
                    "response.completed",
                    "resumed",
                ),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let path = sessions.path().join("async.jsonl");
    let executed = Arc::new(AtomicUsize::new(0));
    let make_agent = |session| {
        let mut extensions = ExtensionHost::new();
        extensions.tool(AsyncRead {
            release: Arc::new(tokio::sync::Notify::new()),
            executed: executed.clone(),
        });
        Agent::new(AgentConfig {
            client: AiClient::new(),
            model: model(&server.uri()),
            session,
            system: "test".into(),
            sandbox: SandboxConfig::new(workspace.path()),
            effect_broker: EffectBroker::new(EffectPolicy::Controlled),
            extensions,
            max_turns: Some(4),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: Default::default(),
            cache_retention: octet_ai::CacheRetention::Short,
            session_id: None,
        })
        .unwrap()
    };
    let mut agent = make_agent(Session::create(&path).unwrap());
    let mut run = agent.prompt("lookup").await.unwrap();
    let mut turns = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            if matches!(event, AgentEvent::TurnStarted) {
                turns += 1;
                if turns == 2 {
                    break;
                }
            }
        }
        while executed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let crash_snapshot = std::fs::read(&path).unwrap();
    drop(run);
    drop(agent);
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    let dropped = Session::open(&path).unwrap();
    assert!(serde_json::to_string(&dropped.context().unwrap())
        .unwrap()
        .contains("must not be replayed automatically"));
    // An abrupt process loss does not run the ordinary Drop cancellation write.
    let crash_path = sessions.path().join("crash.jsonl");
    std::fs::write(&crash_path, crash_snapshot).unwrap();
    let mut resumed = make_agent(Session::open(&crash_path).unwrap());
    let mut run = resumed
        .prompt("continue without re-executing")
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(5), collect(&mut run))
        .await
        .unwrap();
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    drop(run);
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let body = requests[1].body_json::<serde_json::Value>().unwrap();
    let results: Vec<_> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["call_id"], "original-call");
    assert!(
        results[0]
            .to_string()
            .contains("indeterminate background call"),
        "{}",
        results[0]
    );
}

#[test]
fn ultra_baseline_is_valid_but_cross_mode_idle_updates_do_not_rewrite_history() {
    let mut route = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-6-astra".into()))
        .unwrap();
    let spec = Arc::make_mut(&mut route.spec);
    spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
    spec.capabilities.reasoning.as_mut().unwrap().max_effort = ReasoningEffort::Ultra;
    spec.capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .options
        .as_mut()
        .unwrap()
        .values
        .push("ultra".into());
    let endpoint = Arc::make_mut(&mut route.endpoint);
    endpoint.auth = Auth::None;
    endpoint.base_url = url::Url::parse("http://127.0.0.1:1/").unwrap();
    endpoint.transport = octet_ai::EndpointTransport::Http;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let ultra = ReasoningConfig::Effort(ReasoningEffort::Ultra);
    let high = ReasoningConfig::Effort(ReasoningEffort::High);
    for (initial, replacement) in [(ultra.clone(), high.clone()), (high, ultra)] {
        let path = sessions.path().join(format!("{:?}.jsonl", initial));
        let mut agent = build_agent_with_reasoning(
            route.clone(),
            &path,
            workspace.path(),
            initial.clone(),
            Some(4),
        );
        agent
            .set_reasoning(initial.clone())
            .unwrap_or_else(|error| {
                panic!(
                    "{initial:?}: {error}; {:?}",
                    route.spec.capabilities.reasoning
                )
            });
        let entries = agent.session().entries().len();
        assert!(agent.set_reasoning(replacement).is_err());
        assert_eq!(agent.reasoning(), &initial);
        assert_eq!(agent.session().entries().len(), entries);
        assert_eq!(
            agent
                .session()
                .responses_reasoning(&route.endpoint.id, &route.spec.id)
                .unwrap(),
            Some((initial.clone(), initial))
        );
    }
}
