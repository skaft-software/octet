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
        let error = super::prepare_prompt(&mut app, &command).await.unwrap_err();
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
