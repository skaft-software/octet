//! Tests for the JSONL RPC frontend: framing, streaming, and response ownership.
//!
//! Why this is a separate module: the RPC mode is one long single-owner loop, and
//! the loop body reads far better without several kilobytes of fixtures appended
//! to it. The suite stays in this crate because it constructs the same `App` the
//! loop does, which is the only way to pin the wire contract.

#[derive(Clone, Default)]
struct RpcCapture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for RpcCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl RpcCapture {
    fn output(&self, delta_only: bool) -> RpcOutput {
        RpcOutput {
            stdout: Box::new(self.clone()),
            delta_only,
        }
    }
    fn frames(&self) -> Vec<Value> {
        serde_json::Deserializer::from_slice(&self.0.lock().unwrap())
            .into_iter()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn rpc_loopback_app(uri: &str) -> (tempfile::TempDir, App) {
    rpc_loopback_app_with_session(uri, false)
}

fn rpc_loopback_app_with_session(uri: &str, ephemeral: bool) -> (tempfile::TempDir, App) {
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let session_path = if ephemeral {
        let store = crate::session_store::SessionStore::new(
            &directory.path().join("rpc-ephemeral"),
            directory.path(),
        );
        store.write_workspace_marker().unwrap();
        store.new_path("rpc-live")
    } else {
        directory.path().join("rpc-live.jsonl")
    };
    let mut model = app.model.clone();
    Arc::make_mut(&mut model.spec).protocol = Protocol::OpenAiChat;
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.base_url = format!("{uri}/").parse().unwrap();
    endpoint.auth = octet_ai::Auth::None;
    endpoint.default_headers.clear();
    endpoint.transport = octet_ai::EndpointTransport::Http;
    endpoint.runtime = octet_ai::RequestRuntime::default();
    app.agent = octet_agent::Agent::new(octet_agent::AgentConfig {
        client: app.client.clone(),
        model: model.clone(),
        session: octet_agent::Session::create(session_path).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(directory.path()),
        effect_broker: octet_agent::EffectBroker::new(octet_agent::EffectPolicy::Controlled),
        extensions: octet_agent::ExtensionHost::new(),
        max_turns: Some(128),
        reasoning: octet_ai::ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    app.model = model;
    (directory, app)
}

#[test]
fn rpc_branch_summary_projection_retains_real_identity_and_provenance() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let from = app
        .agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("source branch".into())],
        })))
        .unwrap();
    let id = app
        .agent
        .session_mut()
        .branch_with_summary(None, "actual handoff".into(), Default::default())
        .unwrap();
    let entry = app.agent.session().entry(&id).unwrap();
    let value = RpcSessionProjection::new(&app).entry(entry);
    assert_eq!(value["id"], id.0);
    assert_eq!(value["parentId"], Value::Null);
    assert_eq!(value["type"], "branch_summary");
    assert_eq!(value["summary"], "actual handoff");
    assert_eq!(value["fromId"], from.0);
    assert_eq!(
        value["details"],
        json!({"readFiles": [], "modifiedFiles": []})
    );
    assert!(value.get("message").is_none());
    assert!(value.get("firstKeptEntryId").is_none());
}

#[test]
fn rpc_message_count_matches_the_projected_history_for_every_shape() {
    let directory = tempfile::tempdir().unwrap();
    let (_fixture, app) = crate::compaction::tests::app_for_estimate();
    let model = app.model.clone();
    let mut session =
        octet_agent::Session::create(directory.path().join("rpc-count.jsonl")).unwrap();
    let image = || {
        UserPart::Media(octet_ai::Media::image_bytes(
            bytes::Bytes::from_static(b"png"),
            "image/png".parse().unwrap(),
        ))
    };
    let result = |id: &str| {
        UserPart::ToolResult(octet_ai::ToolResult {
            tool_call_id: octet_ai::ToolCallId(id.into()),
            content: vec![octet_ai::ToolResultPart::Text(format!("{id} done"))],
            is_error: false,
            added_tool_names: None,
            tool_name: None,
        })
    };
    let user_turns = vec![
        vec![UserPart::Text("hello".into())],
        vec![],
        vec![result("a")],
        // Consecutive tool-result turns coalesce, media moving behind results.
        vec![result("b"), image()],
        vec![image(), result("c")],
        vec![result("d")],
        vec![
            UserPart::Text("mixed".into()),
            result("e"),
            image(),
            result("f"),
        ],
        vec![image(), UserPart::Text("trailing".into())],
        vec![result("g")],
    ];
    assert_eq!(rpc_message_count(&session), 0);
    for (index, content) in user_turns.into_iter().enumerate() {
        session
            .append(EntryValue::Message(Message::User(UserMessage { content })))
            .unwrap();
        assert_eq!(
            rpc_message_count(&session),
            rpc_messages_from_session(&session, &model).len(),
            "after user turn {index}"
        );
        if index % 3 == 0 {
            session
                .append_assistant_turn(
                    AssistantMessage {
                        model: model.spec.id.clone(),
                        protocol: model.spec.protocol,
                        content: vec![AssistantPart::Text("answer".into())],
                    },
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    Usage::default(),
                    None,
                    StopReason::EndTurn,
                    None,
                )
                .unwrap();
            assert_eq!(
                rpc_message_count(&session),
                rpc_messages_from_session(&session, &model).len(),
                "after assistant turn {index}"
            );
        }
    }
}

#[test]
fn rpc_messages_keep_durable_identity_across_turns_reopen_and_branching() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rpc-identity.jsonl");
    let (_fixture, app) = crate::compaction::tests::app_for_estimate();
    let model = app.model.clone();
    let mut session = octet_agent::Session::create(&path).unwrap();
    let first_prompt = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("first prompt".into())],
        })))
        .unwrap();
    let tool_call = octet_ai::ToolCall {
        async_execution: false,
        id: octet_ai::ToolCallId("call-1".into()),
        name: "probe".into(),
        arguments_json: "{}".into(),
        argument_error: None,
    };
    let first_assistant = session
        .append_assistant_turn(
            AssistantMessage {
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
                content: vec![AssistantPart::ToolCall(tool_call)],
            },
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                input_tokens: 3,
                output_tokens: 2,
                total_tokens: 5,
                ..Usage::default()
            },
            None,
            StopReason::ToolUse,
            None,
        )
        .unwrap();
    let tool_result = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(octet_ai::ToolResult {
                tool_call_id: octet_ai::ToolCallId("call-1".into()),
                content: vec![octet_ai::ToolResultPart::Text("effect retained".into())],
                is_error: false,
                added_tool_names: None,
                tool_name: None,
            })],
        })))
        .unwrap();
    let first = rpc_messages_from_session(&session, &model);
    assert_eq!(rpc_messages_from_session(&session, &model), first);
    assert_eq!(first.len(), 3);
    assert_eq!(
        first[0]["timestamp"],
        json!(session
            .entry(&first_prompt)
            .unwrap()
            .timestamp_unix_ms
            .unwrap())
    );
    assert_eq!(
        first[1]["timestamp"],
        json!(session
            .entry(&first_assistant)
            .unwrap()
            .timestamp_unix_ms
            .unwrap())
    );
    assert_eq!(first[1]["content"][0]["type"], "toolCall");
    assert!(first[1]["usage"]["cost"].is_null());
    assert_eq!(first[2]["toolName"], "probe");
    assert_eq!(first[2]["content"][0]["text"], "effect retained");
    assert_eq!(
        first[2]["timestamp"],
        json!(session
            .entry(&tool_result)
            .unwrap()
            .timestamp_unix_ms
            .unwrap())
    );

    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("second prompt".into())],
        })))
        .unwrap();
    session
        .append_assistant_turn(
            AssistantMessage {
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
                content: vec![AssistantPart::Text("second answer".into())],
            },
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage::default(),
            None,
            StopReason::EndTurn,
            None,
        )
        .unwrap();
    let after_second_turn = rpc_messages_from_session(&session, &model);
    assert_eq!(&after_second_turn[..first.len()], first.as_slice());
    drop(session);

    let mut reopened = octet_agent::Session::open(&path).unwrap();
    assert_eq!(
        rpc_messages_from_session(&reopened, &model),
        after_second_turn
    );
    reopened.checkout(first_prompt).unwrap();
    reopened
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("branched prompt".into())],
        })))
        .unwrap();
    let branched = rpc_messages_from_session(&reopened, &model);
    assert_eq!(branched.len(), 2);
    assert_eq!(branched[0]["content"][0]["text"], "first prompt");
    assert_eq!(branched[1]["content"][0]["text"], "branched prompt");
}

#[test]
fn rpc_messages_omit_missing_legacy_timestamps_after_append_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rpc-legacy.jsonl");
    let (_fixture, app) = crate::compaction::tests::app_for_estimate();
    let model = app.model.clone();
    let mut session = octet_agent::Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("legacy prompt".into())],
        })))
        .unwrap();
    drop(session);

    let source = std::fs::read(&path).unwrap();
    let mut legacy = Vec::new();
    for line in source
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut record: Value = serde_json::from_slice(line).unwrap();
        if record["type"] == "entry" {
            record.as_object_mut().unwrap().remove("timestamp_unix_ms");
        }
        serde_json::to_writer(&mut legacy, &record).unwrap();
        legacy.push(b'\n');
    }
    std::fs::write(&path, legacy).unwrap();

    let mut reopened = octet_agent::Session::open(&path).unwrap();
    let legacy_projection = rpc_messages_from_session(&reopened, &model);
    assert_eq!(legacy_projection.len(), 1);
    assert!(legacy_projection[0].get("timestamp").is_none());
    reopened
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("new prompt".into())],
        })))
        .unwrap();
    let projection = rpc_messages_from_session(&reopened, &model);
    assert!(projection[0].get("timestamp").is_none());
    assert!(projection[1].get("timestamp").is_some());
    drop(reopened);
    let resumed = octet_agent::Session::open(&path).unwrap();
    assert_eq!(rpc_messages_from_session(&resumed, &model), projection);
}

#[test]
fn rpc_recovered_output_does_not_enter_the_current_turn_or_history() {
    let (_directory, app) = crate::compaction::tests::app_for_estimate();
    let capture = RpcCapture::default();
    let mut output = capture.output(false);
    let mut translator = EventTranslator::new(&app, json!({"role": "user", "content": []}));
    let history = translator.messages.clone();
    let run_messages = translator.run_messages.clone();
    let mut queue = QueueState::default();

    for channel in [OutputChannel::Text, OutputChannel::Reasoning] {
        translator
            .observe(
                AgentEvent::RecoveredOutput {
                    channel,
                    text: "interrupted prior attempt".into(),
                },
                &mut output,
                &mut queue,
            )
            .unwrap();
    }
    assert!(capture.frames().is_empty());
    assert_eq!(translator.messages, history);
    assert_eq!(translator.run_messages, run_messages);
    assert!(translator.partial_text.is_empty());
    assert!(translator.partial_reasoning.is_empty());
    assert!(!translator.message_started);

    translator
        .observe(
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "current answer".into(),
            },
            &mut output,
            &mut queue,
        )
        .unwrap();
    assert_eq!(translator.partial_text, "current answer");
    assert!(capture
        .frames()
        .iter()
        .all(|frame| !frame.to_string().contains("interrupted prior attempt")));
}

#[test]
fn rpc_cache_warming_events_are_session_only_and_preserve_exact_cost() {
    let (_directory, app) = crate::compaction::tests::app_for_estimate();
    let capture = RpcCapture::default();
    let mut output = capture.output(false);
    let mut translator = EventTranslator::new(&app, json!({"role": "user", "content": []}));
    translator.partial_text = "real answer in progress".into();
    let before = translator.run_messages.clone();
    let mut queue = QueueState::default();
    let event = AgentEvent::CacheWarmed {
        usage: Usage {
            cache_read_tokens: 30_000,
            output_tokens: 1,
            total_tokens: 30_001,
            ..Default::default()
        },
        cost: Some(Cost {
            total: 7,
            total_picodollars_remainder: 5,
            ..Default::default()
        }),
        extension_override: true,
    };
    assert!(translator
        .observe(event, &mut output, &mut queue)
        .unwrap()
        .is_none());
    let frames = capture.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["type"], "cache_warmed");
    assert_eq!(frames[0]["cost"]["total"], 7);
    assert_eq!(frames[0]["cost"]["total_picodollars_remainder"], 5);
    assert_eq!(frames[0]["extensionOverride"], true);
    assert_eq!(translator.run_messages, before);
    assert_eq!(translator.partial_text, "real answer in progress");
    assert!(!translator.message_started);
    translator
        .observe(
            AgentEvent::CacheWarmed {
                usage: Default::default(),
                cost: None,
                extension_override: false,
            },
            &mut output,
            &mut queue,
        )
        .unwrap();
    assert!(translator.usage_uncertain);
}

#[tokio::test]
async fn rpc_active_cache_warming_state_uses_live_control_diagnostics() {
    let (_directory, mut app) = rpc_loopback_app("http://127.0.0.1:9");
    let mut translator = EventTranslator::new(&app, json!({"role": "user", "content": []}));
    // No stream poll or provider I/O is needed to inspect a host selection.
    let run = app.agent.prompt("inspect the active policy").await.unwrap();
    let control = run.control();
    control
        .set_cache_warming_mode(octet_agent::CacheWarmMode::Off)
        .unwrap();
    translator.cache_warming_control = Some(control);
    let state = active_state_value(
        &json!({"cacheWarmingMode": "streaming", "cacheWarmingStatus": {"state": "scheduled"}}),
        &translator,
        &QueueState::default(),
    );
    assert_eq!(state["cacheWarmingMode"], "off");
    assert_eq!(state["cacheWarmingStatus"]["state"], "inactive");
    assert_eq!(
        state["cacheWarmingStatus"]["reason"],
        "cache warming disabled"
    );
    drop(run);
}

struct BrokenPipe;

impl std::io::Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn rpc_output_failure_accounts_and_discards_each_ephemeral_run() {
    let _exclusive_ephemeral = crate::session_store::EPHEMERAL_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Keep the process-wide fixture lock outside the async body. The test
    // still exercises the same single-threaded Tokio scheduling.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let accounting_root = tempfile::tempdir().unwrap();
        for index in 0..2 {
            let (directory, mut app) = rpc_loopback_app_with_session("http://127.0.0.1:9", true);
            let workspace = app.config.workspace.clone();
            let transcript_root = directory.path().join("rpc-ephemeral");
            let transcript = app.agent.session().path().to_path_buf();
            app.agent
                .session_mut()
                .record_terminal_gate_usage(
                    octet_ai::EndpointId("custom".into()),
                    ModelId("probe".into()),
                    Usage {
                        input_tokens: 10 + index,
                        output_tokens: 2,
                        total_tokens: 12 + index,
                        ..Usage::default()
                    },
                    Some(Cost {
                        total: 7 + index,
                        ..Cost::default()
                    }),
                    Some(true),
                )
                .unwrap();
            crate::session_store::begin_ephemeral_run(
                transcript_root.clone(),
                accounting_root.path().to_path_buf(),
                workspace.clone(),
            );
            let (tx, input) = mpsc::channel(1);
            tx.send(RpcInput::Value(json!({"type": "get_state"})))
                .await
                .unwrap();
            drop(tx);

            let result = if index == 0 {
                finish_rpc_accounting(
                    run_rpc_loop(
                        app,
                        input,
                        RpcOutput {
                            stdout: Box::new(BrokenPipe),
                            delta_only: false,
                        },
                    )
                    .await,
                )
            } else {
                let capture = RpcCapture::default();
                let result =
                    finish_rpc_accounting(run_rpc_loop(app, input, capture.output(false)).await);
                assert_eq!(capture.frames()[0]["success"], true);
                result
            };
            if index == 0 {
                let error = result.unwrap_err();
                assert!(
                    error.chain().any(|cause| {
                        cause
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
                            || cause
                                .downcast_ref::<serde_json::Error>()
                                .is_some_and(|json| {
                                    json.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe)
                                })
                    }),
                    "unexpected RPC error: {error:#}"
                );
            } else {
                result.unwrap();
            }
            assert!(!transcript.exists(), "ephemeral transcript must be removed");
            assert!(!transcript_root.exists(), "temporary root must be removed");
            assert!(crate::session_store::finish_ephemeral_run()
                .unwrap()
                .is_none());
            let store = crate::session_store::SessionStore::new(accounting_root.path(), &workspace);
            let summary = store.ephemeral_accounting_summary().unwrap();
            assert_eq!(summary.runs, 1);
            assert_eq!(summary.usage_records, 1);
            assert_eq!(summary.input_tokens, 10 + index);
            assert_eq!(summary.total_cost_microdollars, 7 + index);
        }
    });
}

#[test]
fn rpc_startup_failure_still_discards_ephemeral_transcript_and_accounts() {
    let _exclusive_ephemeral = crate::session_store::EPHEMERAL_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = tempfile::tempdir().unwrap();
    let accounting_root = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let transcript_root = directory.path().join("rpc-ephemeral");
    let store = crate::session_store::SessionStore::new(&transcript_root, &workspace);
    store.write_workspace_marker().unwrap();
    let transcript = store.new_path("startup");
    let mut session = octet_agent::Session::create(&transcript).unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("custom".into()),
            ModelId("probe".into()),
            "startup-test",
        )
        .unwrap();
    drop(session);
    crate::session_store::begin_ephemeral_run(
        transcript_root.clone(),
        accounting_root.path().to_path_buf(),
        workspace.clone(),
    );

    let error = finish_rpc_accounting(Err(anyhow::anyhow!("startup failed"))).unwrap_err();
    assert_eq!(error.to_string(), "startup failed");
    assert!(!transcript.exists());
    assert!(!transcript_root.exists());
    assert!(crate::session_store::finish_ephemeral_run()
        .unwrap()
        .is_none());
    let summary = crate::session_store::SessionStore::new(accounting_root.path(), &workspace)
        .ephemeral_accounting_summary()
        .unwrap();
    assert_eq!(summary.runs, 1);
    assert_eq!(summary.uncertainty_records, 1);
    assert!(summary.has_uncertain_usage);
}

fn rpc_test_queued(text: String) -> QueuedInput {
    let input = UserInput::from(text.clone());
    QueuedInput {
        message: user_input_value(&input),
        input,
        text,
    }
}

const RPC_TEST_SSE: &str = concat!(
    "data: {\"id\":\"fixture\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"fixture\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn rpc_liveness_replays_queues_beyond_control_capacity_in_order() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(RPC_TEST_SSE, "text/event-stream"))
        .mount(&server)
        .await;
    for mode in ["all", "one-at-a-time"] {
        let (_directory, mut app) = rpc_loopback_app(&server.uri());
        let capture = RpcCapture::default();
        let mut output = capture.output(false);
        let mut translator = EventTranslator::new(&app, user_value("initial"));
        let mut queue = QueueState::default();
        for index in 0..16 {
            queue
                .steering
                .push_back(rpc_test_queued(format!("steer-{index}")));
            queue
                .follow_up
                .push_back(rpc_test_queued(format!("follow-{index}")));
        }
        let mut settings = RpcSettings {
            steering_mode: mode.into(),
            follow_up_mode: mode.into(),
            ..RpcSettings::default()
        };
        let (_tx, mut input) = mpsc::channel(64);
        let mut run = app.agent.prompt("initial").await.unwrap();
        let (deferred, eof, outcome) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drive_run(
                &mut run,
                &mut input,
                &mut output,
                &json!({}),
                &json!({}),
                app.skills.clone(),
                app.prompts.clone(),
                app.config.workspace.clone(),
                &mut translator,
                &mut queue,
                &mut settings,
            ),
        )
        .await
        .expect("replay must poll Run while admitting more than eight controls")
        .unwrap();
        assert_eq!(outcome, HostRunOutcome::Completed);
        assert!(!eof);
        assert!(deferred.is_empty());
        assert_eq!(queue.len(), 0);
        let delivered: Vec<_> = translator
            .run_messages
            .iter()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"][0]["text"].as_str())
            .collect();
        for prefix in ["steer", "follow"] {
            let actual: Vec<_> = delivered
                .iter()
                .filter(|text| text.starts_with(prefix))
                .copied()
                .collect();
            let expected: Vec<_> = (0..16).map(|index| format!("{prefix}-{index}")).collect();
            assert_eq!(actual, expected);
        }
    }
}

#[tokio::test]
async fn rpc_liveness_sustained_active_controls_and_abort_settle() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Send headers and a delta immediately, then keep the SSE body open.
    // A delayed-header fixture would test provider opening instead of an
    // active stream (the agent does not consume controls while opening).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        let first = RPC_TEST_SSE.split("\n\n").next().unwrap().to_owned() + "\n\n";
        socket
            .write_all(format!("{:x}\r\n{}\r\n", first.len(), first).as_bytes())
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let (_directory, mut app) = rpc_loopback_app(&uri);
    let capture = RpcCapture::default();
    let mut output = capture.output(false);
    let mut translator = EventTranslator::new(&app, user_value("initial"));
    let mut queue = QueueState::default();
    let mut settings = RpcSettings::default();
    let (tx, mut input) = mpsc::channel(64);
    let producer_capture = capture.clone();
    let producer = async move {
        while !producer_capture
            .frames()
            .iter()
            .any(|frame| frame["type"] == "message_update")
        {
            tokio::task::yield_now().await;
        }

        for index in 0..128 {
            let command = match index % 4 {
                0 => json!({"type": "set_steering_mode", "mode": "all"}),
                1 => json!({"type": "set_follow_up_mode", "mode": "one-at-a-time"}),
                2 => json!({"type": "steer", "message": format!("steer-{index}")}),
                _ => json!({"type": "follow_up", "message": format!("follow-{index}")}),
            };
            let mut command = command;
            command["id"] = json!(format!("control-{index}"));
            tx.send(RpcInput::Value(command)).await.unwrap();
        }
        tx.send(RpcInput::Value(
            json!({"type": "get_state", "id": "barrier"}),
        ))
        .await
        .unwrap();
        loop {
            if producer_capture
                .frames()
                .iter()
                .any(|frame| frame["id"] == "barrier")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        tx.send(RpcInput::Value(json!({"type": "abort", "id": "abort"})))
            .await
            .unwrap();
        // Keep stdin open: settlement must be caused by abort, not EOF.
        tx
    };
    let mut run = app.agent.prompt("initial").await.unwrap();
    let state = json!({});
    let commands = json!({});
    let drive = drive_run(
        &mut run,
        &mut input,
        &mut output,
        &state,
        &commands,
        app.skills.clone(),
        app.prompts.clone(),
        app.config.workspace.clone(),
        &mut translator,
        &mut queue,
        &mut settings,
    );
    let ((deferred, eof, outcome), _tx) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (result, tx) = tokio::join!(drive, producer);
            (result.unwrap(), tx)
        })
        .await
        .expect("active controls must not stop Run polling or abort settlement");
    provider.abort();
    let _ = provider.await;
    assert_eq!(outcome, HostRunOutcome::Aborted);
    assert!(!eof);
    assert!(deferred.is_empty());
    let frames = capture.frames();
    for index in 0..128 {
        let responses: Vec<_> = frames
            .iter()
            .filter(|frame| frame["id"] == format!("control-{index}"))
            .collect();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], true);
    }
    assert!(frames
        .iter()
        .any(|frame| frame["id"] == "abort" && frame["success"] == true));
}

#[test]
fn json_delta_schema_matches_compacted_rpc_without_prefix_snapshots() {
    let (_directory, app) = crate::compaction::tests::app_for_estimate();
    let rpc_capture = RpcCapture::default();
    let json_capture = RpcCapture::default();
    let mut rpc_output = rpc_capture.output(false);
    let mut json_output = json_capture.output(true);
    let mut rpc = EventTranslator::new(&app, user_value("initial"));
    let mut json = EventTranslator::new(&app, user_value("initial"));
    rpc.message_timestamp = 1;
    json.message_timestamp = 1;
    for (channel, text) in [
        (OutputChannel::Reasoning, "理由"),
        (OutputChannel::Text, "answer"),
        (OutputChannel::Text, " tail"),
    ] {
        rpc.emit_delta(&mut rpc_output, channel, text.into())
            .unwrap();
        json.emit_delta(&mut json_output, channel, text.into())
            .unwrap();
    }
    let final_message = rpc.partial_message();
    rpc.emit_content_ends(&mut rpc_output, &final_message)
        .unwrap();
    json.emit_content_ends(&mut json_output, &final_message)
        .unwrap();
    let assistant = AssistantMessage {
        model: app.model.spec.id.clone(),
        protocol: app.model.spec.protocol,
        content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
            async_execution: false,
            id: octet_ai::ToolCallId("call-1".into()),
            name: "read".into(),
            arguments_json: r#"{"path":"file"}"#.into(),
            argument_error: None,
        })],
    };
    let tool_message = assistant_value(
        &assistant,
        "fixture",
        &Usage::default(),
        None,
        &StopReason::ToolUse,
        Some(1),
    );
    rpc.emit_tool_call_updates(&mut rpc_output, &assistant, &tool_message)
        .unwrap();
    json.emit_tool_call_updates(&mut json_output, &assistant, &tool_message)
        .unwrap();
    for output in [&mut rpc_output, &mut json_output] {
        output
            .send(json!({"type": "message_end", "message": tool_message}))
            .unwrap();
    }
    let rpc_frames = rpc_capture.frames();
    for frame in rpc_frames
        .iter()
        .filter(|frame| frame["type"] == "message_update")
    {
        assert_eq!(frame["message"], frame["assistantMessageEvent"]["partial"]);
        assert!(frame.get("message").is_some());
    }
    let expected: Vec<_> = rpc_frames
        .into_iter()
        .map(|mut frame| {
            compact_json_event(&mut frame);
            frame
        })
        .collect();
    assert_eq!(json_capture.frames(), expected);
    PARTIAL_SNAPSHOTS.with(|count| count.set(0));
    json.partial_text = "retained prefix".repeat(100_000);
    for _ in 0..128 {
        json.emit_delta(&mut json_output, OutputChannel::Text, "x".into())
            .unwrap();
    }
    PARTIAL_SNAPSHOTS.with(|count| {
        assert_eq!(
            count.get(),
            0,
            "JSON chunks must not build cumulative snapshots"
        )
    });
}

#[test]
fn rpc_progress_retention_is_bounded_utf8_with_honest_omission() {
    let mut progress = RpcToolProgress::default();
    progress.push_str(&"x".repeat(MAX_RPC_TOOL_PROGRESS_BYTES - 1));
    progress.push_str("é");
    progress.push_str("z");
    assert_eq!(progress.text.len(), MAX_RPC_TOOL_PROGRESS_BYTES - 1);
    assert_eq!(progress.omitted, 3);
    for _ in 0..32 {
        progress.push_str(&"界".repeat(32_000));
    }
    assert!(progress.text.len() <= MAX_RPC_TOOL_PROGRESS_BYTES);
    assert_eq!(progress.omitted, 3 + 32 * 96_000);
    assert!(progress
        .snapshot()
        .ends_with("[3072003 UTF-8 display bytes omitted from live progress]"));
}

#[test]
fn rpc_failed_admission_does_not_mutate_queue_or_settings() {
    let capture = RpcCapture::default();
    let mut output = capture.output(false);
    let mut queue = QueueState::default();
    let mut settings = RpcSettings::default();
    for request in [
        RpcControlRequest::Steer(rpc_test_queued("not admitted".into())),
        RpcControlRequest::SteeringMode("all".into()),
    ] {
        RpcAdmission {
            id: Some("refused".into()),
            command: "test".into(),
            request,
            replay: false,
        }
        .complete(
            Err(AgentError::RunEnded),
            &mut queue,
            &mut settings,
            &mut output,
        )
        .unwrap();
    }
    assert_eq!(queue.len(), 0);
    assert_eq!(settings.steering_mode, "one-at-a-time");
    assert!(capture
        .frames()
        .iter()
        .all(|frame| frame["success"] == false));
}

#[tokio::test]
async fn changelog_rpc_prompt_and_queue_reject_without_session_mutation() {
    let (_workspace, mut app) = crate::compaction::tests::app_for_estimate();
    let head = app.agent.session().head();
    for invocation in ["/changelog", "/chang"] {
        let command = serde_json::json!({"type": "prompt", "message": invocation});
        let error = super::prepare_prompt(&mut app, &command)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("interactive TUI"));
        let error = super::queued_input(
            &command,
            invocation,
            app.skills.as_ref(),
            app.prompts.as_ref(),
            &app.config.workspace,
            &app.agent.registered_tool_names(),
        )
        .err()
        .expect("TUI command must not be queued");
        assert!(error.to_string().contains("interactive TUI"));
        assert_eq!(app.agent.session().head(), head);
    }
}
use super::*;

#[test]
fn resumed_session_stats_mark_known_subtotals_without_changing_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("accounting.jsonl");
    let mut session = octet_agent::Session::create(&path).unwrap();
    session
        .record_compaction_usage(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            Usage {
                input_tokens: 80,
                output_tokens: 20,
                cache_read_tokens: 10,
                cache_write_tokens: 5,
                total_tokens: 115,
                ..Usage::default()
            },
            Some(Cost {
                total: 123,
                ..Cost::default()
            }),
        )
        .unwrap();
    drop(session);
    let mut session = octet_agent::Session::open(&path).unwrap();
    let certain = session_stats_for_session(&session);
    assert_eq!(certain["usageUncertain"], false);
    assert_eq!(certain["tokens"]["total"], 115);
    assert_eq!(certain["cost"], dollars(123));
    let known_records = serde_json::to_value(session.usage_records()).unwrap();
    let head = session.head();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("openai".into()),
            ModelId("gpt-4o-mini".into()),
            "assistant_turn",
        )
        .unwrap();
    let current = session_stats_for_session(&session);
    drop(session);
    let session = octet_agent::Session::open(&path).unwrap();
    let resumed = session_stats_for_session(&session);
    assert_eq!(current, resumed);
    assert_eq!(resumed["usageUncertain"], true);
    let mut expected = certain;
    expected["usageUncertain"] = json!(true);
    assert_eq!(resumed, expected);
    assert_eq!(
        serde_json::to_value(session.usage_records()).unwrap(),
        known_records
    );
    assert_eq!(session.head(), head);
}

#[test]
fn resumed_stats_do_not_claim_an_exact_bill_for_unpriced_completed_usage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unpriced.jsonl");
    let mut session = octet_agent::Session::create(&path).unwrap();
    session
        .record_compaction_usage(
            octet_ai::EndpointId("codex".into()),
            ModelId("codex/gpt-5.5".into()),
            Usage {
                input_tokens: 10,
                total_tokens: 10,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    assert!(!session.has_uncertain_usage());
    assert!(session.has_unpriced_usage());
    let current = session_stats_for_session(&session);
    assert_eq!(current["usageUncertain"], true);
    assert_eq!(current["tokens"]["total"], 10);
    assert_eq!(current["cost"], dollars(0)); // known subtotal, not a whole-bill assertion
    drop(session);
    let resumed = octet_agent::Session::open(&path).unwrap();
    assert_eq!(session_stats_for_session(&resumed), current);
}

#[test]
fn rpc_settled_turn_cost_beats_mapper_catalog_and_matches_durable_replay() {
    #[derive(Clone, Default)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let settled = Cost {
        input: 5,
        output: 2,
        total: 7,
        total_picodollars_remainder: 250_123,
        ..Cost::default()
    };
    for turn_cost in [Some(settled), None, Some(Cost::default())] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rpc-cost.jsonl");
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let pricing = Arc::make_mut(&mut model.spec).pricing.as_mut().unwrap();
        pricing.input = octet_ai::TokenRate(900_000_000);
        pricing.output = octet_ai::TokenRate(900_000_000);
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Usage::default()
        };
        assert_ne!(octet_ai::pricing::cost_of(pricing, &usage).ok(), turn_cost);
        let message = AssistantMessage {
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
            content: vec![AssistantPart::Text("settled".into())],
        };
        let mut session = octet_agent::Session::create(&path).unwrap();
        session
            .append_assistant_turn(
                message.clone(),
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                usage,
                turn_cost,
                StopReason::EndTurn,
                None,
            )
            .unwrap();
        drop(session);
        let reopened = octet_agent::Session::open(&path).unwrap();
        let record = &reopened.usage_records()[0];
        assert_eq!(record.cost, turn_cost);
        let durable = assistant_value(
            &message,
            &model.endpoint.id.0,
            &record.usage,
            record.cost,
            &StopReason::EndTurn,
            record.completed_at_unix_ms,
        );
        let mut translator = EventTranslator {
            endpoint: model.endpoint.id.0.clone(),
            api: protocol_name(&model.spec.protocol).into(),
            model,
            partial_text: String::new(),
            partial_reasoning: String::new(),
            channels: Vec::new(),
            message_started: false,
            message_timestamp: 0,
            turn_open: true,
            pending_turn: None,
            pending_tool_results: Vec::new(),
            expected_tools: 0,
            tools: HashMap::new(),
            messages: Vec::new(),
            run_messages: Vec::new(),
            last_assistant_text: String::new(),
            retry_attempt: None,
            pending_retry_end: None,
            cache_warming_control: None,
            usage_uncertain: false,
        };
        let capture = Capture::default();
        let mut output = RpcOutput {
            delta_only: false,
            stdout: Box::new(capture.clone()),
        };
        translator
            .observe(
                AgentEvent::TurnFinished {
                    message,
                    stop_reason: StopReason::EndTurn,
                    turn_usage: usage,
                    turn_cost,
                    usage,
                    session_cost_microdollars: turn_cost.map(|cost| cost.total),
                    // Deliberately includes unrelated auxiliary spend; never derive a turn cost from it.
                    run_cost_microdollars: 999_999,
                },
                &mut output,
                &mut QueueState::default(),
            )
            .unwrap();
        let bytes = capture.0.lock().unwrap();
        let frames: Vec<Value> = serde_json::Deserializer::from_slice(&bytes)
            .into_iter()
            .collect::<Result<_, _>>()
            .unwrap();
        let live = &frames
            .iter()
            .find(|value| value["type"] == "message_end")
            .unwrap()["message"];
        assert_eq!(live["usage"], durable["usage"]);
        assert_eq!(translator.usage_uncertain, turn_cost.is_none());
        match turn_cost {
            Some(cost) => {
                let expected =
                    dollars(cost.total) + f64::from(cost.total_picodollars_remainder) / 1e12;
                assert_eq!(live["usage"]["cost"]["total"], json!(expected));
                if cost.total_picodollars_remainder != 0 {
                    assert_ne!(live["usage"]["cost"]["total"], json!(dollars(cost.total)));
                }
            }
            None => assert!(live["usage"]["cost"].is_null()),
        }
    }
}

#[test]
fn repeated_network_waits_preserve_rpc_history_without_finite_retry_budget() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let mut output = RpcOutput {
        delta_only: false,
        stdout: Box::new(capture.clone()),
    };
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let committed = vec![
        json!({"role": "assistant", "content": "COMMITTED"}),
        json!({"role": "toolResult", "toolCallId": "call", "content": "DONE"}),
    ];
    let mut translator = EventTranslator {
        endpoint: model.endpoint.id.0.clone(),
        api: protocol_name(&model.spec.protocol).into(),
        model,
        partial_text: String::new(),
        partial_reasoning: String::new(),
        channels: Vec::new(),
        message_started: false,
        message_timestamp: 0,
        turn_open: true,
        pending_turn: None,
        pending_tool_results: Vec::new(),
        expected_tools: 0,
        tools: HashMap::new(),
        messages: committed.clone(),
        run_messages: committed.clone(),
        last_assistant_text: "COMMITTED".into(),
        retry_attempt: None,
        pending_retry_end: None,
        cache_warming_control: None,
        usage_uncertain: false,
    };
    let mut queue = QueueState::default();
    translator
        .observe(
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "STALE".into(),
            },
            &mut output,
            &mut queue,
        )
        .unwrap();
    translator
        .observe(
            AgentEvent::ProviderRetry {
                attempt: 1,
                max_attempts: 5,
                delay: std::time::Duration::ZERO,
                error: "disconnect".into(),
            },
            &mut output,
            &mut queue,
        )
        .unwrap();
    capture.0.lock().unwrap().clear();
    for attempt in 1..=32 {
        assert!(translator
            .observe(
                AgentEvent::ProviderWaitingForNetwork {
                    attempt,
                    delay: std::time::Duration::from_millis(1234),
                    error: "offline".into(),
                },
                &mut output,
                &mut queue
            )
            .unwrap()
            .is_none());
        assert_eq!(translator.messages, committed);
        assert_eq!(translator.run_messages, committed);
        assert_eq!(translator.last_assistant_text, "COMMITTED");
        assert!(translator.partial_text.is_empty());
        assert!(!translator.message_started);
        assert!(translator.turn_open);
        assert_eq!(translator.retry_attempt, Some(1));
    }
    let bytes = capture.0.lock().unwrap();
    let frames: Vec<Value> = serde_json::Deserializer::from_slice(&bytes)
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(frames.len(), 32);
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            *frame,
            json!({"type": "provider_waiting_for_network",
            "attempt": index + 1, "delayMs": 1234, "errorMessage": "offline"})
        );
    }
    drop(bytes);
    assert_eq!(
        active_state_value(&json!({"usageUncertain": false}), &translator, &queue)
            ["usageUncertain"],
        false
    );
    translator.partial_text = "candidate awaiting gate".into();
    let timestamp = translator.message_timestamp;
    capture.0.lock().unwrap().clear();
    assert!(translator
        .observe(AgentEvent::ProviderUsageUncertain, &mut output, &mut queue)
        .unwrap()
        .is_none());
    let frame: Value = serde_json::from_slice(&capture.0.lock().unwrap()).unwrap();
    assert_eq!(frame, json!({"type": "provider_usage_uncertain"}));
    assert_eq!(
        active_state_value(&json!({"usageUncertain": false}), &translator, &queue)
            ["usageUncertain"],
        true
    );
    assert_eq!(translator.message_timestamp, timestamp);
    assert_eq!(translator.partial_text, "candidate awaiting gate");
    assert_eq!(translator.messages, committed);
    assert!(translator.turn_open);
    for (operation, name) in [
        (
            octet_agent::ProviderOperation::LocalCompaction,
            "local_compaction",
        ),
        (
            octet_agent::ProviderOperation::NativeCompaction,
            "native_compaction",
        ),
        (
            octet_agent::ProviderOperation::TerminalGate,
            "terminal_gate",
        ),
    ] {
        for max_attempts in [None, Some(5)] {
            capture.0.lock().unwrap().clear();
            assert!(translator
                .observe(
                    AgentEvent::ProviderOperationRetry {
                        operation,
                        attempt: 8,
                        max_attempts,
                        delay: std::time::Duration::from_millis(1234),
                        error: "offline".into(),
                    },
                    &mut output,
                    &mut queue
                )
                .unwrap()
                .is_none());
            assert_eq!(translator.partial_text, "candidate awaiting gate");
            assert_eq!(translator.messages, committed);
            assert_eq!(translator.run_messages, committed);
            assert!(translator.turn_open);
            assert_eq!(translator.retry_attempt, Some(1));
            let frame: Value = serde_json::from_slice(&capture.0.lock().unwrap()).unwrap();
            assert_eq!(
                frame,
                json!({"type": "provider_operation_retry", "operation": name,
                "attempt": 8, "maxAttempts": max_attempts, "delayMs": 1234, "errorMessage": "offline"})
            );
        }
    }
}

#[test]
fn rpc_failure_diagnostic_includes_safe_operational_details() {
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let error = AgentError::Ai(octet_ai::AiError::Http(octet_ai::HttpError {
        status: http::StatusCode::BAD_REQUEST,
        request_id: Some("req-400".into()),
        retry_after: None,
        provider_code: Some("invalid_request".into()),
        body_snippet: Some(r#"{"error":{"message":"model does not support this request"}}"#.into()),
        retryable: false,
    }));

    let diagnostic = rpc_error_diagnostic(&model, &error);
    assert!(diagnostic.contains("status=400 (bad request)"));
    assert!(diagnostic.contains("code=invalid_request"));
    assert!(diagnostic.contains("request_id=req-400"));
}

#[test]
fn lf_framing_keeps_unicode_line_separators_inside_json() {
    let (tx, mut rx) = mpsc::channel(2);
    let mut line = br#"{"type":"prompt","message":"a\u2028b"}"#.to_vec();
    dispatch_line(&tx, &mut line);
    let RpcInput::Value(value) = rx.try_recv().unwrap() else {
        panic!("expected parsed value");
    };
    assert_eq!(value["message"], "a\u{2028}b");
}

#[test]
fn response_shape_omits_id_when_request_did() {
    let mut response = Map::new();
    response.insert("type".into(), Value::String("response".into()));
    response.insert("command".into(), Value::String("get_state".into()));
    response.insert("success".into(), Value::Bool(true));
    assert!(response.get("id").is_none());
}

#[test]
fn rpc_tool_execution_end_carries_details_and_structured_content_like_pi() {
    let (_directory, app) = crate::compaction::tests::app_for_estimate();
    let capture = RpcCapture::default();
    let mut output = capture.output(false);
    let mut queue = QueueState::default();
    let mut finish = |id: &str, result| {
        let mut translator = EventTranslator::new(&app, json!({"role": "user", "content": []}));
        capture.0.lock().unwrap().clear();
        for event in [
            AgentEvent::ToolStarted {
                id: octet_ai::ToolCallId(id.into()),
                name: "bench_tool".into(),
                args: json!({}),
            },
            AgentEvent::ToolFinished {
                id: octet_ai::ToolCallId(id.into()),
                result,
                duration: std::time::Duration::ZERO,
            },
        ] {
            translator.observe(event, &mut output, &mut queue).unwrap();
        }
        let frames = capture.frames();
        let end = frames
            .iter()
            .find(|frame| frame["type"] == "tool_execution_end")
            .unwrap()
            .clone();
        let message = frames
            .iter()
            .find(|frame| frame["type"] == "message_end")
            .unwrap()["message"]
            .clone();
        (end, message)
    };

    // A native extension's metadata is the Pi `details`; structured content
    // rides along, and the text stays the single content part.
    let native = octet_agent::ToolOutput::new("ok")
        .try_with_details(
            Some(json!({"value": 1})),
            Some(json!({"events": ["a", "b"]})),
        )
        .unwrap();
    let (end, message) = finish("native", Ok(native));
    assert_eq!(
        end["result"],
        json!({"content": [{"type": "text", "text": "ok"}], "details": {"events": ["a", "b"]},
            "structuredContent": {"value": 1}})
    );
    assert_eq!(end["isError"], false);
    assert_eq!(message["details"], json!({"events": ["a", "b"]}));
    assert_eq!(message["content"], end["result"]["content"]);

    // A Pi-compatible tool's own `details` wins over the rest of the metadata.
    let pi = octet_agent::ToolOutput::new("pi")
        .try_with_metadata(json!({"pi_details": {"n": 2}, "other": true}))
        .unwrap();
    let (end, _) = finish("pi", Ok(pi));
    assert_eq!(end["result"]["details"], json!({"n": 2}));

    // Plain text results and failures project exactly as before.
    let (end, message) = finish("plain", Ok(octet_agent::ToolOutput::new("plain")));
    assert_eq!(
        end["result"],
        json!({"content": [{"type": "text", "text": "plain"}]})
    );
    assert!(message.get("details").is_none());
    let (end, _) = finish("failed", Err(octet_agent::ToolError::new("boom")));
    assert_eq!(end["isError"], true);
    assert!(end["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("boom"));
}

#[cfg(unix)]
fn rpc_input_hook_app(uri: &str) -> (tempfile::TempDir, App) {
    use std::os::unix::fs::PermissionsExt as _;
    let (directory, mut app) = rpc_loopback_app(uri);
    let root = directory.path().join("extensions/rpc-input");
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("input.py");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import json, os, sys
for line in sys.stdin:
    message = json.loads(line)
    method, params = message.get('method'), message.get('params', {})
    if method == 'initialize':
        result = dict(api_version='0.4', tools=[], commands=[], protocol=dict(
            version='0.4', features=params['protocol']['required_features'] + ['input_transform_v1'],
            limits=dict(max_concurrent_requests=4)))
    elif method == 'hook/run':
        payload = params['payload']
        with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'rpc-input-trace.jsonl'), 'a') as trace:
            trace.write(json.dumps(payload) + '\n')
        result = dict(disposition=dict(action='continue'), notifications=[], context=[])
        if payload.get('phase') == 'input':
            text = payload['text']
            if text == 'cancelled':
                print(json.dumps(dict(jsonrpc='2.0', id=message['id'], error=dict(code=-32800, message='cancelled'))), flush=True)
                continue
            result['input_event'] = dict(action='handled') if text == 'handled' else dict(
                action='transform', text='/rpc-expanded argument')
            if text == 'strip-images':
                result['input_event']['images'] = []
    elif method == 'shutdown':
        print(json.dumps(dict(jsonrpc='2.0', id=message['id'], result={})), flush=True)
        break
    else:
        continue
    print(json.dumps(dict(jsonrpc='2.0', id=message['id'], result=result)), flush=True)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        root.join("extension.toml"),
        r#"name = "rpc-input"
version = "0.4.0"
api_version = "0.4"
[entrypoint]
command = "input.py"
[contributes]
hooks = ["before_prompt"]
"#,
    )
    .unwrap();
    app.config.extension_paths = vec![root.parent().unwrap().to_owned()];
    app.config.enabled_extensions = vec!["rpc-input".into()];
    app.config.invocation_trusted_extensions = vec!["rpc-input".into()];
    app.config.mode = crate::config::Mode::Rpc;
    app = rebuild_app_for_reload(app).unwrap();
    assert!(
        app.executable_extensions
            .summaries()
            .iter()
            .any(|summary| summary.name == "rpc-input" && summary.running),
        "{}",
        app.executable_extensions.inspect_text()
    );
    let template = directory.path().join("rpc-expanded.md");
    std::fs::write(&template, "RPC-TEMPLATE $1\n").unwrap();
    app.prompts = Arc::new(PromptRegistry::from_sources(&[
        crate::prompts::PromptSource {
            path: template,
            trust: crate::prompts::PromptTrust::ExplicitExternal,
        },
    ]));
    (directory, app)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_raw_input_precedes_templates_and_preserves_or_replaces_images() {
    let (directory, mut app) = rpc_input_hook_app("http://127.0.0.1:1");
    for (text, image_count) in [("omitted-images", 1), ("strip-images", 0)] {
        let (prepared, _, display) = prepare_prompt(
            &mut app,
            &json!({
                "message": text, "streamingBehavior": "followUp",
                "images": [{"data": "aW1hZ2U=", "mimeType": "image/png"}]
            }),
        )
        .await
        .unwrap()
        .expect("transformed input");
        let crate::app::PreparedAppSubmission::Ordinary(input) = prepared else {
            panic!("native model uses ordinary submission");
        };
        assert!(
            matches!(&input.parts[0], InputPart::Text(text) if text.contains("RPC-TEMPLATE argument"))
        );
        assert_eq!(
            input
                .parts
                .iter()
                .filter(|part| matches!(part, InputPart::Media(_)))
                .count(),
            image_count
        );
        assert_eq!(display["content"][0]["text"], "/rpc-expanded argument");
    }
    let trace: Vec<Value> = std::fs::read_to_string(directory.path().join("rpc-input-trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        trace.len(),
        4,
        "one raw input and one composition hook per prompt"
    );
    for (pair, text) in trace
        .chunks_exact(2)
        .zip(["omitted-images", "strip-images"])
    {
        assert_eq!(pair[0]["phase"], "input");
        assert_eq!(pair[0]["text"], text);
        assert_eq!(pair[0]["source"], "rpc");
        // An idle prompt has no delivery mode, even when the RPC command
        // includes a hint that would apply only while streaming.
        assert!(pair[0]["streaming_behavior"].is_null());
        assert_eq!(pair[0]["images"].as_array().unwrap().len(), 1);
        assert!(pair[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("RPC-TEMPLATE argument"));
    }
    assert!(app.agent.session().usage_records().is_empty());
    app.executable_extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_handled_input_acknowledges_without_provider_persistence_or_queue_replay() {
    let server = wiremock::MockServer::start().await;
    let (_directory, app) = rpc_input_hook_app(&server.uri());
    let path = app.agent.session().path().to_owned();
    let head = app.agent.session().head();
    let capture = RpcCapture::default();
    let (tx, input) = mpsc::channel(4);
    for command in [
        json!({"type": "steer", "message": "keep queued", "id": "queue"}),
        json!({"type": "follow_up", "message": "keep follow-up", "id": "follow-up"}),
        json!({"type": "prompt", "message": "handled", "id": "handled"}),
        json!({"type": "get_state", "id": "state"}),
    ] {
        tx.send(RpcInput::Value(command)).await.unwrap();
    }
    drop(tx);
    run_rpc_loop(app, input, capture.output(false))
        .await
        .unwrap();
    let frames = capture.frames();
    assert_eq!(
        frames
            .iter()
            .find(|frame| frame["id"] == "handled")
            .unwrap(),
        &json!({"type": "response", "command": "prompt", "id": "handled", "success": true,
            "data": {"disposition": "handled"}})
    );
    assert_eq!(
        frames.iter().find(|frame| frame["id"] == "state").unwrap()["data"]["pendingMessageCount"],
        2
    );
    assert!(!frames.iter().any(|frame| matches!(
        frame["type"].as_str(),
        Some("agent_start" | "turn_start" | "message_start" | "agent_end" | "agent_settled")
    )));
    assert_eq!(octet_agent::Session::open(path).unwrap().head(), head);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_cancelled_raw_input_does_not_compose_or_persist() {
    let (directory, mut app) = rpc_input_hook_app("http://127.0.0.1:1");
    let head = app.agent.session().head();
    let system = app.agent.system_prompt().to_owned();
    let error = prepare_prompt(&mut app, &json!({"message": "cancelled"}))
        .await
        .err()
        .expect("cancellation refuses prompt");
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(app.agent.session().head(), head);
    assert_eq!(app.agent.system_prompt(), system);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("rpc-input-trace.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    app.executable_extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_reload_prompt_reconciles_the_selected_route() {
    let directory = tempfile::tempdir().unwrap();
    let app = crate::app::bootstrap::provider_reload_test_app(directory.path());
    let old = app.model.endpoint.id.clone();
    let mut app = reload_resources(app).await.unwrap();
    prepare_prompt(&mut app, &json!({"message": "local scripted request"}))
        .await
        .unwrap()
        .expect("input was not handled");
    let current = app.catalog.resolve(&app.model.spec.id).unwrap();
    assert_ne!(current.endpoint.id, old);
    assert_eq!(app.model.endpoint.id, current.endpoint.id);
    assert_eq!(app.agent.model().endpoint.id, current.endpoint.id);
    app.executable_extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_reload_streams_only_from_the_replacement_process() {
    let directory = tempfile::tempdir().unwrap();
    let app = crate::app::bootstrap::provider_reload_test_app(directory.path());
    let runtime = app.executable_extensions.provider_runtime();
    let selected = app.model.spec.id.clone();
    let initial_route = runtime.registered_route_for_test(&selected).unwrap();
    assert_eq!(initial_route.1, app.model.endpoint.id);
    let routes = Arc::new(std::sync::Mutex::new(vec![initial_route]));
    let observed_routes = Arc::clone(&routes);
    struct TimedCapture {
        capture: RpcCapture,
        arrivals: Arc<std::sync::Mutex<Vec<(std::time::Instant, Value)>>>,
    }
    impl std::io::Write for TimedCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.capture, bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            let frames = self.capture.frames();
            let mut arrivals = self.arrivals.lock().unwrap();
            let seen = arrivals.len();
            arrivals.extend(
                frames
                    .into_iter()
                    .skip(seen)
                    .map(|frame| (std::time::Instant::now(), frame)),
            );
            Ok(())
        }
    }
    let capture = RpcCapture::default();
    let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_arrivals = Arc::clone(&arrivals);
    let output = RpcOutput {
        stdout: Box::new(TimedCapture {
            capture: capture.clone(),
            arrivals,
        }),
        delta_only: false,
    };
    let wire = directory
        .path()
        .join("extensions/rebind-provider/variant.wire");
    let (tx, input) = mpsc::channel(4);
    let mut producer = tokio::spawn(async move {
        for (index, command) in [
            json!({"type": "prompt", "message": "before reload", "id": "before"}),
            json!({"type": "reload", "id": "reload"}),
            json!({"type": "prompt", "message": "after reload", "id": "after"}),
        ]
        .into_iter()
        .enumerate()
        {
            let id = command["id"].as_str().unwrap().to_owned();
            tx.send(RpcInput::Value(command)).await.unwrap();
            let started = std::time::Instant::now();
            let deadline = started + std::time::Duration::from_secs(5);
            loop {
                let frames = observed_arrivals
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(_, frame)| frame.clone())
                    .collect::<Vec<_>>();
                assert!(
                    !frames
                        .iter()
                        .any(|frame| frame["id"] == id && frame["success"] == false),
                    "RPC phase {id}: error response: {frames:?}"
                );
                let ready_at = {
                    let arrivals = observed_arrivals.lock().unwrap();
                    if index == 1 {
                        arrivals
                            .iter()
                            .find(|(_, frame)| frame["id"] == "reload" && frame["success"] == true)
                    } else {
                        arrivals
                            .iter()
                            .filter(|(_, frame)| frame["type"] == "agent_settled")
                            .nth(if index == 0 { 0 } else { 1 })
                    }
                    .map(|(at, _)| *at)
                };
                if let Some(at) = ready_at {
                    assert!(
                        at <= deadline,
                        "RPC phase {id}: response arrived late at {:?}",
                        at.duration_since(started)
                    );
                    if index == 2 {
                        observed_routes
                            .lock()
                            .unwrap()
                            .push(runtime.registered_route_for_test(&selected).unwrap());
                    }
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "RPC phase {id}: timed out, sender_closed={}; wire={:?}; {frames:?}",
                    tx.is_closed(),
                    std::fs::read_to_string(&wire)
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let rpc = run_rpc_loop(app, input, output);
        tokio::pin!(rpc);
        tokio::select! {
            biased;
            completed = &mut producer => {
                completed.expect("RPC command producer failed");
                rpc.await.expect("RPC failed after the command producer completed");
            }
            result = &mut rpc => {
                producer.abort();
                let _ = (&mut producer).await;
                panic!("RPC finished before the command producer: {result:?}; {:?}", capture.frames());
            }
        }
    }).await;
    if !producer.is_finished() {
        producer.abort();
        let _ = producer.await;
    }
    result.unwrap_or_else(|error| {
        panic!(
            "scripted RPC reload/prompt did not settle: {error}; {:?}",
            capture.frames()
        )
    });
    let frames = capture.frames();
    for id in ["before", "reload", "after"] {
        assert!(
            frames
                .iter()
                .any(|frame| frame["id"] == id && frame["success"] == true),
            "{frames:?}"
        );
    }
    let streams = std::fs::read_to_string(
        directory
            .path()
            .join("extensions/rebind-provider/variant.streams"),
    )
    .unwrap();
    let calls = streams.lines().collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0], calls[1],
        "the retired process handled a later prompt"
    );
    let routes = routes.lock().unwrap();
    assert_eq!(routes.len(), 2);
    assert_ne!(routes[0].0.owner, routes[1].0.owner);
    assert_ne!(routes[0].1, routes[1].1);
    assert_eq!(routes[0].0.provider, routes[1].0.provider);
    assert_eq!(routes[0].0.model, routes[1].0.model);
    let completed = frames
        .iter()
        .filter(|frame| frame["type"] == "agent_end")
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 2);
    for (index, frame) in completed.iter().enumerate() {
        assert_eq!(frame["willRetry"], false);
        let messages = frame["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 1, "{frame:?}");
        let message = messages[0];
        assert_eq!(message["provider"], routes[index].1 .0);
        calls[index]
            .strip_prefix("LOCAL-PROVIDER-")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["stopReason"], "stop");
        assert!(message.get("errorMessage").is_none());
        assert_eq!(
            message["content"],
            json!([{"type": "text", "text": calls[index]}])
        );
    }
    assert!(
        !frames
            .iter()
            .any(|frame| frame.get("errorMessage").is_some()
                || frame["success"] == false
                || frame["type"] == "auto_retry_start"),
        "{frames:?}"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["type"] == "agent_settled")
            .count(),
        2
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_preentry_aba_retains_submission_authority() {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    struct SignaledCapture {
        capture: RpcCapture,
        frames: mpsc::UnboundedSender<Value>,
        seen: usize,
    }
    impl std::io::Write for SignaledCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.capture, bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            let frames = self.capture.frames();
            for frame in frames.iter().skip(self.seen) {
                self.frames.send(frame.clone()).unwrap();
            }
            self.seen = frames.len();
            Ok(())
        }
    }
    async fn settled(frames: &mut mpsc::UnboundedReceiver<Value>) -> Vec<Value> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut observed = Vec::new();
            loop {
                let frame = frames
                    .recv()
                    .await
                    .expect("RPC output closed before settlement");
                let done = frame["type"] == "agent_settled";
                observed.push(frame);
                if done {
                    return observed;
                }
            }
        })
        .await
        .expect("RPC request did not settle")
    }
    fn exact_success(frames: &[Value], id: &str, text: &str, endpoint: &str) -> bool {
        let ends = frames
            .iter()
            .filter(|frame| frame["type"] == "agent_end")
            .collect::<Vec<_>>();
        if ends.len() != 1 || ends[0]["willRetry"] != false {
            return false;
        }
        let assistants = ends[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "assistant")
            .collect::<Vec<_>>();
        assistants.len() == 1
            && assistants[0]["stopReason"] == "stop"
            && assistants[0]["provider"] == endpoint
            && assistants[0].get("errorMessage").is_none()
            && assistants[0]["content"] == json!([{"type": "text", "text": text}])
            && frames
                .iter()
                .filter(|frame| frame["id"] == id && frame["success"] == true)
                .count()
                == 1
            && !frames
                .iter()
                .any(|frame| frame["success"] == false || frame["type"] == "auto_retry_start")
    }
    let mut behavioral_failures = Vec::new();
    for reload in [false, true] {
        for mutation in [
            "unchanged",
            "identical",
            "unrelated",
            "selected-aba",
            "auth-aba",
        ] {
            // macOS's default TMPDIR makes the provider's Unix socket path
            // exceed sockaddr_un.sun_path before turn_started can be delivered.
            let directory = tempfile::Builder::new()
                .prefix("rpc009-")
                .tempdir_in("/tmp")
                .unwrap()
                .keep();
            std::fs::write(directory.join("preentry-evidence"), "enabled").unwrap();
            std::fs::write(directory.join("rpc-evidence"), "enabled").unwrap();
            let mut app = crate::app::bootstrap::provider_reload_hook_test_app(&directory);
            if reload {
                app = reload_resources(app).await.unwrap();
            }
            app.synchronize_extension_provider_catalog_for_request()
                .unwrap();
            let runtime = app.executable_extensions.provider_runtime();
            let selected = app.model.spec.id.clone();
            let installed = runtime.registered_route_for_test(&selected).unwrap();
            assert_eq!(installed.1, app.model.endpoint.id);
            assert_eq!(installed.1, app.agent.model().endpoint.id);
            let control = Arc::new(crate::extensions::LifecycleDeliveryTestControl::default());
            control.gate_turn_started();
            app.executable_extensions.lifecycle_delivery_test_control = Some(control.clone());
            let prefix = directory.join("extensions/rebind-provider/variant");
            let capture = RpcCapture::default();
            let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
            let output = RpcOutput {
                stdout: Box::new(SignaledCapture {
                    capture: capture.clone(),
                    frames: frame_tx,
                    seen: 0,
                }),
                delta_only: false,
            };
            let (tx, input) = mpsc::channel(4);
            let producer = async {
                tx.send(RpcInput::Value(
                    json!({"type": "prompt", "message": "prepared D0", "id": "prepared-r0"}),
                ))
                .await
                .unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    control.turn_started_entered(),
                )
                .await
                .unwrap();
                let captured = runtime.registered_route_for_test(&selected).unwrap();
                assert_eq!(captured, installed);
                assert!(!prefix.with_extension("streams").exists());
                let phases: &[&str] = match mutation {
                    "unchanged" => &[],
                    "identical" => &["identical"],
                    "unrelated" => &["unrelated"],
                    "selected-aba" => &["selected", "restore"],
                    "auth-aba" => &["revoke", "ready"],
                    _ => unreachable!(),
                };
                let mut chronology = Vec::new();
                for (index, phase) in phases.iter().enumerate() {
                    let acknowledgment: Value =
                        tokio::time::timeout(std::time::Duration::from_secs(5), async {
                            let mut socket =
                                tokio::net::UnixStream::connect(prefix.with_extension("control"))
                                    .await
                                    .unwrap();
                            let command =
                                json!({"mutation": phase, "id": format!("control-{}", index + 1)});
                            socket
                                .write_all(format!("{command}\n").as_bytes())
                                .await
                                .unwrap();
                            let mut line = String::new();
                            tokio::io::BufReader::new(socket)
                                .read_line(&mut line)
                                .await
                                .unwrap();
                            let acknowledgment: Value = serde_json::from_str(&line).unwrap();
                            assert_eq!(acknowledgment["command"], command);
                            assert_eq!(
                                acknowledgment["host_response"]["id"],
                                format!("rpc-update-{}", index + 1)
                            );
                            assert!(acknowledgment["host_response"].get("result").is_some());
                            acknowledgment
                        })
                        .await
                        .unwrap();
                    assert!(!prefix.with_extension("streams").exists());
                    let route = runtime.registered_route_for_test(&selected);
                    if *phase == "revoke" {
                        assert!(route.is_none());
                        assert_eq!(
                            acknowledgment["host_response"]["result"]["status"],
                            "revoked"
                        );
                    } else {
                        let route = route.as_ref().unwrap();
                        assert_eq!(route.0.owner, captured.0.owner);
                        assert!(route.0.registration_revision > captured.0.registration_revision);
                        if *phase == "selected" {
                            assert_eq!(
                                route.0.model.context_window,
                                captured.0.model.context_window + 1
                            );
                            assert_ne!(route.0.model, captured.0.model);
                        }
                        if *phase == "ready" {
                            assert_eq!(
                                acknowledgment["host_response"]["result"]["status"],
                                "ready"
                            );
                        }
                    }
                    let entry = json!({"phase": phase, "acknowledgment": acknowledgment, "route": format!("{route:?}"), "callbacks": 0});
                    chronology.push(entry);
                }
                let restored = runtime.registered_route_for_test(&selected).unwrap();
                assert_eq!(restored.0.owner, captured.0.owner);
                assert_eq!(restored.0.provider, captured.0.provider);
                assert_eq!(restored.0.model, captured.0.model);
                assert_eq!(restored.0.model_metadata, captured.0.model_metadata);
                assert_eq!(restored.1, captured.1);
                if mutation != "unchanged" {
                    assert!(restored.0.registration_revision > captured.0.registration_revision);
                }
                std::fs::write(
                    prefix.with_extension("dispatch"),
                    "all mutation ACKs received; release first Run poll",
                )
                .unwrap();
                control.release_turn_started();
                let first = settled(&mut frame_rx).await;
                let first_callbacks =
                    std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default();
                let pids = std::fs::read_to_string(prefix.with_extension("pids")).unwrap();
                let live_pid: Value = serde_json::from_str(pids.lines().last().unwrap()).unwrap();
                let expected = format!("LOCAL-PROVIDER-{}", live_pid["fixture"]);
                let positive = matches!(mutation, "unchanged" | "identical" | "unrelated");
                let first_success = exact_success(&first, "prepared-r0", &expected, &captured.1 .0)
                    && first_callbacks == format!("{expected}\n");
                let failed = first
                    .iter()
                    .filter(|frame| frame["type"] == "agent_end")
                    .count()
                    == 1
                    && first
                        .iter()
                        .filter(|frame| frame["type"] == "agent_settled")
                        .count()
                        == 1
                    && first.iter().any(|frame| {
                        frame["type"] == "agent_end"
                            && frame["willRetry"] == false
                            && frame["messages"].as_array().unwrap().iter().any(|message| {
                                message["role"] == "assistant"
                                    && message["stopReason"] == "error"
                                    && message["errorMessage"]
                                        .as_str()
                                        .is_some_and(|error| !error.is_empty())
                            })
                    })
                    && !first
                        .iter()
                        .any(|frame| frame["type"] == "auto_retry_start");
                let old_valid = if positive {
                    first_success
                } else {
                    failed && first_callbacks.is_empty()
                };
                tx.send(RpcInput::Value(json!({"type": "prompt", "message": "genuinely fresh restored D0", "id": "fresh-r1"}))).await.unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    control.turn_started_entered(),
                )
                .await
                .unwrap();
                let fresh_route = runtime.registered_route_for_test(&selected).unwrap();
                assert_eq!(fresh_route, restored);
                assert_eq!(
                    std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default(),
                    first_callbacks
                );
                control.release_turn_started();
                let fresh = settled(&mut frame_rx).await;
                let callbacks =
                    std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default();
                let fresh_valid = exact_success(&fresh, "fresh-r1", &expected, &fresh_route.1 .0)
                    && callbacks == format!("{first_callbacks}{expected}\n");
                drop(tx);
                json!({"reload": reload, "mutation": mutation, "directory": directory,
                    "captured": format!("{captured:?}"), "restored": format!("{restored:?}"),
                    "fresh_route": format!("{fresh_route:?}"), "chronology": chronology,
                    "positive": positive, "old_valid": old_valid, "first_success": first_success,
                    "failed_settled": failed, "first_callbacks": first_callbacks,
                    "fresh_valid": fresh_valid, "callbacks": callbacks, "live_pid": live_pid,
                    "first_frames": first, "fresh_frames": fresh})
            };
            let (rpc_result, observation) = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                Box::pin(async { tokio::join!(run_rpc_loop(app, input, output), producer) }),
            )
            .await
            .expect("invalid RPC/barrier/shutdown timeout, not a behavioral red");
            rpc_result.expect("invalid RPC dispatcher failure, not a behavioral red");
            let exits = std::fs::read_to_string(prefix.with_extension("exits")).unwrap();
            let exits = exits
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let pids = std::fs::read_to_string(prefix.with_extension("pids")).unwrap();
            let protocol = std::fs::read_to_string(prefix.with_extension("protocol")).unwrap();
            let acknowledgments = protocol
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|entry| {
                    entry["direction"] == "send"
                        && entry["value"]["result"]["terminal"] == "shutdown"
                })
                .count();
            let expected = if reload { 2 } else { 1 };
            assert_eq!(
                exits.len(),
                expected,
                "invalid shutdown, not behavioral red"
            );
            assert_eq!(pids.lines().count(), expected);
            assert_eq!(acknowledgments, expected);
            assert!(exits.iter().all(|exit| exit["exit"] == 0));
            assert_eq!(
                std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default(),
                observation["callbacks"].as_str().unwrap()
            );
            assert!(!prefix.with_extension("control").exists());
            assert_eq!(
                capture
                    .frames()
                    .iter()
                    .filter(|frame| frame["type"] == "agent_settled")
                    .count(),
                2
            );
            std::fs::write(
                directory.join("observation.json"),
                serde_json::to_vec_pretty(&observation).unwrap(),
            )
            .unwrap();
            if observation["old_valid"] != true || observation["fresh_valid"] != true {
                behavioral_failures.push(json!({"reload": reload, "mutation": mutation, "old_valid": observation["old_valid"], "fresh_valid": observation["fresh_valid"]}));
            }
        }
    }
    assert!(
        behavioral_failures.is_empty(),
        "prepared RPC authority predicates failed after strict shutdown: {behavioral_failures:?}"
    );
}

#[cfg(unix)]
async fn dispatch_prepared_test_prompt(
    agent: &mut octet_agent::Agent,
    prompt: crate::app::PreparedAppSubmission,
) -> Result<(String, octet_agent::FinishReason), Box<octet_agent::AgentError>> {
    let mut run = prompt
        .start(agent, octet_agent::PreparedPromptMode::Normal)
        .await?;
    let mut text = String::new();
    let mut terminal = None;
    while let Some(event) = run.next().await {
        match event {
            octet_agent::AgentEvent::OutputDelta {
                channel: octet_agent::OutputChannel::Text,
                text: delta,
            } => text.push_str(&delta),
            octet_agent::AgentEvent::ProviderRetry { .. } => {
                panic!("prepared continuity failure must not retry")
            }
            octet_agent::AgentEvent::RunFinished { reason, .. } => {
                assert!(terminal.is_none());
                terminal = Some(reason);
            }
            _ => {}
        }
    }
    match terminal.expect("one settled terminal") {
        octet_agent::FinishReason::Failed(error) => Err(Box::new(error)),
        reason => Ok((text, reason)),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_reload_preentry_aba_rejects_prepared_native_dispatch() {
    let mut cases = Vec::new();
    for fresh_first in [false, true] {
        for reload in [false, true] {
            for mutation in [
                "unchanged",
                "identical",
                "unrelated",
                "selected-aba",
                "auth-aba",
            ] {
                let directory = tempfile::Builder::new()
                    .prefix("preentry-")
                    .tempdir()
                    .unwrap()
                    .keep();
                std::fs::write(directory.join("preentry-evidence"), "enabled").unwrap();
                let mut app = crate::app::bootstrap::provider_reload_hook_test_app(&directory);
                if reload {
                    app = reload_resources(app).await.unwrap();
                }
                let runtime = app.executable_extensions.provider_runtime();
                let installed = runtime
                    .registered_route_for_test(&app.model.spec.id)
                    .unwrap();
                let (prompt, _, _) = prepare_prompt(&mut app, &json!({"message": "prepared D0"}))
                    .await
                    .unwrap()
                    .expect("input was not handled");
                let captured = runtime
                    .registered_route_for_test(&app.model.spec.id)
                    .unwrap();
                assert_eq!(captured, installed);
                assert_eq!(captured.1, app.model.endpoint.id);
                assert_eq!(captured.1, app.agent.model().endpoint.id);
                let prefix = directory.join("extensions/rebind-provider/variant");
                assert!(!prefix.with_extension("streams").exists());
                let phases: &[&str] = match mutation {
                    "unchanged" => &[],
                    "identical" => &["preentry-identical"],
                    "unrelated" => &["unrelated"],
                    "selected-aba" => &["update", "preentry-restore"],
                    "auth-aba" => &["preentry-revoke", "preentry-ready"],
                    _ => unreachable!(),
                };
                let mut phase_routes = Vec::new();
                for (index, phase) in phases.iter().enumerate() {
                    std::fs::write(prefix.with_extension("hook"), phase).unwrap();
                    app.executable_extensions
                        .compose_prompt(&app.system, format!("preentry phase {phase}"))
                        .await
                        .unwrap();
                    assert_eq!(
                        std::fs::read_to_string(prefix.with_extension("hooked")).unwrap(),
                        *phase
                    );
                    assert!(!prefix.with_extension("streams").exists());
                    let current = runtime.registered_route_for_test(&app.model.spec.id);
                    if mutation == "auth-aba" && index == 0 {
                        assert!(current.is_none());
                        let summaries = app.executable_extensions.summaries();
                        assert!(summaries
                            .iter()
                            .flat_map(|summary| &summary.providers)
                            .any(|provider| provider.id == "fixture-provider"
                                && provider.authorization == "revoked"));
                    } else {
                        let current = current.as_ref().unwrap();
                        assert_eq!(current.0.owner, captured.0.owner);
                        assert!(current.0.registration_revision > captured.0.registration_revision);
                        if mutation == "selected-aba" && index == 0 {
                            assert_eq!(
                                current.0.model.context_window,
                                captured.0.model.context_window + 1
                            );
                            assert_ne!(current.0.model, captured.0.model);
                        }
                    }
                    phase_routes.push(format!("{current:?}"));
                }
                let restored = runtime
                    .registered_route_for_test(&app.model.spec.id)
                    .unwrap();
                assert_eq!(restored.0.owner, captured.0.owner);
                assert_eq!(restored.0.provider, captured.0.provider);
                assert_eq!(restored.0.model, captured.0.model);
                assert_eq!(restored.0.model_metadata, captured.0.model_metadata);
                assert_eq!(restored.1, captured.1);
                if mutation != "unchanged" {
                    assert!(restored.0.registration_revision > captured.0.registration_revision);
                }
                if mutation == "selected-aba" {
                    let protocol =
                        std::fs::read_to_string(prefix.with_extension("protocol")).unwrap();
                    let updates = protocol
                        .lines()
                        .map(|line| serde_json::from_str::<Value>(line).unwrap())
                        .filter(|entry| {
                            entry["direction"] == "send"
                                && entry["value"]["id"]
                                    .as_str()
                                    .is_some_and(|id| id.starts_with("hook-update-"))
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(updates.len(), 2);
                    assert_eq!(
                        updates[1]["value"]["params"]["models"][0]["context_window"],
                        captured.0.model.context_window
                    );
                }
                std::fs::remove_file(prefix.with_extension("hook")).ok();
                std::fs::write(
                    prefix.with_extension("dispatch"),
                    "all mutation ACKs observed; entering native complete",
                )
                .unwrap();
                let agent_address = &app.agent as *const _;
                let pids = std::fs::read_to_string(prefix.with_extension("pids")).unwrap();
                let live_pid: Value = serde_json::from_str(pids.lines().last().unwrap()).unwrap();
                let expected_callback = format!("LOCAL-PROVIDER-{}", live_pid["fixture"]);
                if fresh_first {
                    let fresh = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        app.agent.complete("prepared D0"),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_eq!(fresh.text, expected_callback);
                    assert!(matches!(fresh.reason, octet_agent::FinishReason::Completed));
                    let ledger = std::fs::read_to_string(prefix.with_extension("streams")).unwrap();
                    assert_eq!(
                        ledger.lines().collect::<Vec<_>>(),
                        vec![expected_callback.as_str()]
                    );
                }
                let baseline = usize::from(fresh_first);
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    dispatch_prepared_test_prompt(&mut app.agent, prompt),
                )
                .await
                .unwrap();
                let callbacks =
                    std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default();
                let positive = matches!(mutation, "unchanged" | "identical" | "unrelated");
                let exact_success = result.as_ref().is_ok_and(|output| {
                    callbacks.lines().count() == baseline + 1
                        && callbacks.lines().all(|line| line == expected_callback)
                        && output.0 == expected_callback
                        && matches!(output.1, octet_agent::FinishReason::Completed)
                });
                let old_callback_count = callbacks.lines().count();
                let old_error_is_continuity = result.as_ref().err().is_some_and(|error| {
                    matches!(&**error, octet_agent::AgentError::Ai(octet_ai::AiError::Transport(octet_ai::TransportError {
                        phase: octet_ai::TransportPhase::ResponseHeaders, timeout: false, message,
                    })) if message == "extension provider stream request failed")
                });
                if !fresh_first {
                    let fresh = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        app.agent.complete("prepared D0"),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_eq!(fresh.text, expected_callback);
                    assert!(matches!(fresh.reason, octet_agent::FinishReason::Completed));
                    assert_eq!(
                        std::fs::read_to_string(prefix.with_extension("streams"))
                            .unwrap()
                            .lines()
                            .count(),
                        old_callback_count + 1
                    );
                }
                assert_eq!(&app.agent as *const _, agent_address);
                let (fresh_app, _, _) =
                    prepare_prompt(&mut app, &json!({"message": "prepared D0"}))
                        .await
                        .unwrap()
                        .expect("input was not handled");
                let fresh_app = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    dispatch_prepared_test_prompt(&mut app.agent, fresh_app),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(fresh_app.0, expected_callback);
                assert!(matches!(fresh_app.1, octet_agent::FinishReason::Completed));
                let final_callbacks =
                    std::fs::read_to_string(prefix.with_extension("streams")).unwrap();
                assert_eq!(
                    final_callbacks.lines().count(),
                    old_callback_count + usize::from(!fresh_first) + 1
                );
                assert!(final_callbacks
                    .lines()
                    .all(|line| line == expected_callback));
                let observation = json!({
                    "fresh_first": fresh_first, "baseline": baseline, "old_callback_count": old_callback_count,
                    "old_error_is_continuity": old_error_is_continuity, "callbacks": final_callbacks,
                    "reload": reload, "mutation": mutation, "directory": directory,
                    "captured": format!("{captured:?}"), "restored": format!("{restored:?}"),
                    "phase_routes": phase_routes, "result": format!("{result:?}"),
                    "error": result.is_err(), "old_callbacks": callbacks, "live_pid": live_pid,
                    "positive": positive, "exact_success": exact_success,
                });
                std::fs::write(
                    directory.join("observation.json"),
                    serde_json::to_vec_pretty(&observation).unwrap(),
                )
                .unwrap();
                cases.push((app, prefix, observation));
            }
        }
    }
    let mut shutdown_failures = Vec::new();
    let mut behavioral_failures = Vec::new();
    for (mut app, prefix, observation) in cases {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.executable_extensions.shutdown(),
        )
        .await
        .unwrap();
        let exits = std::fs::read_to_string(prefix.with_extension("exits")).unwrap_or_default();
        let exits = exits
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let pids = std::fs::read_to_string(prefix.with_extension("pids")).unwrap();
        let protocol = std::fs::read_to_string(prefix.with_extension("protocol")).unwrap();
        let acknowledgments = protocol
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|entry| {
                entry["direction"] == "send" && entry["value"]["result"]["terminal"] == "shutdown"
            })
            .count();
        let expected = if observation["reload"] == true { 2 } else { 1 };
        if exits.len() != expected
            || acknowledgments != expected
            || pids.lines().count() != expected
            || exits.iter().any(|exit| exit["exit"] != 0)
        {
            shutdown_failures
                .push(json!({"case": observation, "exits": exits, "acks": acknowledgments}));
            continue;
        }
        let callbacks =
            std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default();
        assert_eq!(callbacks, observation["callbacks"].as_str().unwrap());
        let valid = if observation["positive"] == true {
            observation["exact_success"] == true
                && observation["old_callback_count"].as_u64().unwrap()
                    == observation["baseline"].as_u64().unwrap() + 1
        } else {
            observation["error"] == true
                && observation["old_error_is_continuity"] == true
                && observation["old_callback_count"] == observation["baseline"]
        };
        if !valid {
            behavioral_failures.push(observation);
        }
    }
    assert!(
        shutdown_failures.is_empty(),
        "invalid shutdowns, not behavioral red: {shutdown_failures:?}"
    );
    assert!(
        behavioral_failures.is_empty(),
        "prepared native D0 admission predicates failed: {behavioral_failures:?}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_reload_prepared_route_rejects_mutation_before_dispatch() {
    let mut followed = Vec::new();
    for reload in [false, true] {
        for mutation in [
            "update",
            "withdraw",
            "base_url",
            "input",
            "pricing",
            "api_name",
            "label",
            "unrelated",
            "generation",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::bootstrap::provider_reload_hook_test_app(directory.path());
            if reload {
                app = reload_resources(app).await.unwrap();
            }
            prepare_prompt(&mut app, &json!({"message": "prepared D0"}))
                .await
                .unwrap()
                .expect("input was not handled");
            let runtime = app.executable_extensions.provider_runtime();
            let original = runtime
                .registered_route_for_test(&app.model.spec.id)
                .unwrap();
            let prefix = directory.path().join("extensions/rebind-provider/variant");
            if mutation == "generation" {
                let outcomes = app
                    .executable_extensions
                    .runtime_manager()
                    .unwrap()
                    .reload("rebind-provider")
                    .await;
                assert!(outcomes.iter().all(Result::is_ok));
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        if runtime
                            .registered_route_for_test(&app.model.spec.id)
                            .is_some_and(|route| route.0.owner != original.0.owner)
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            } else {
                std::fs::write(prefix.with_extension("hook"), mutation).unwrap();
                app.executable_extensions
                    .compose_prompt(&app.system, "publish D1 after preparation".into())
                    .await
                    .unwrap();
                assert_eq!(
                    std::fs::read_to_string(prefix.with_extension("hooked")).unwrap(),
                    mutation
                );
            }
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.agent.complete("dispatch prepared D0"),
            )
            .await
            .unwrap();
            if mutation == "unrelated" {
                let output = result.unwrap();
                let callbacks = std::fs::read_to_string(prefix.with_extension("streams")).unwrap();
                assert_eq!(callbacks.lines().count(), 1);
                assert_eq!(output.text, callbacks.trim());
                assert_eq!(
                    runtime
                        .registered_route_for_test(&app.model.spec.id)
                        .unwrap()
                        .1,
                    original.1
                );
            } else if result.is_ok() || prefix.with_extension("streams").exists() {
                followed.push((reload, mutation));
            }
            app.executable_extensions.shutdown().await;
        }
    }
    assert!(
        followed.is_empty(),
        "prepared D0 followed a changed live route: {followed:?}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_reload_composition_mutation_refuses_before_admission() {
    let mut admitted = Vec::new();
    for reload in [false, true] {
        for mutation in [
            "update", "withdraw", "base_url", "input", "pricing", "api_name", "label",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::bootstrap::provider_reload_hook_test_app(directory.path());
            if reload {
                app = reload_resources(app).await.unwrap();
            }
            app.synchronize_extension_provider_catalog_for_request()
                .unwrap();
            let head = app.agent.session().head();
            let prefix = directory.path().join("extensions/rebind-provider/variant");
            std::fs::write(prefix.with_extension("hook"), mutation).unwrap();
            app.agent.set_system_prompt("idle context sentinel");
            app.agent
                .set_prompt_display_text(Some("idle display sentinel".into()));
            let idle_system = app.agent.system_prompt().to_owned();
            let result = prepare_prompt(&mut app, &json!({"message": "must not dispatch"})).await;
            assert_eq!(
                app.agent.system_prompt(),
                idle_system,
                "failed preparation installed composed system for {mutation}"
            );
            assert_eq!(
                std::fs::read_to_string(prefix.with_extension("hooked")).unwrap(),
                mutation
            );
            if result.is_ok() {
                admitted.push((reload, mutation));
            }
            assert_eq!(app.agent.session().head(), head);
            assert!(!prefix.with_extension("streams").exists());
            let entries_before = app.agent.session().entries().len();
            let unpolled = app
                .agent
                .prompt("direct after failed preparation")
                .await
                .unwrap();
            drop(unpolled);
            let appended = &app.agent.session().entries()[entries_before..];
            assert_eq!(appended.len(), 1);
            assert!(matches!(
                appended[0].value,
                EntryValue::Message(Message::User(_))
            ));
            assert_eq!(
                appended[0]
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.display_text.as_deref()),
                Some("idle display sentinel"),
                "failed preparation overwrote display metadata for {mutation}"
            );
            assert!(!prefix.with_extension("streams").exists());
            app.executable_extensions.shutdown().await;
        }
    }
    assert!(
        admitted.is_empty(),
        "composition mutations admitted: {admitted:?}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_provider_reload_withdrawn_or_changed_route_refuses_before_prompt_mutation() {
    for variant in ["withdrawn", "limits", "label"] {
        let directory = tempfile::tempdir().unwrap();
        let app = crate::app::bootstrap::provider_reload_test_app(directory.path());
        let old = app.model.endpoint.id.clone();
        crate::app::bootstrap::set_provider_reload_test_variant(directory.path(), variant);
        let mut app = reload_resources(app).await.unwrap();
        let head = app.agent.session().head();
        let result = prepare_prompt(&mut app, &json!({"message": "must not dispatch"})).await;
        assert!(
            result.is_err(),
            "{variant} admitted a stale provider endpoint"
        );
        assert_eq!(app.agent.session().head(), head);
        assert_eq!(app.agent.model().endpoint.id, old);
        app.executable_extensions.shutdown().await;
    }
}
