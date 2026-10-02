use super::*;
use crate::tool::DEFAULT_PREVIEW_MIN_EMIT_INTERVAL;
use base64::Engine as _;

fn image_input_fixture(large: bool) -> InputPart {
    let encoded = if large {
        // Valid opaque 4002x2 PNG: the fallback resizes it to <=4000px.
        "iVBORw0KGgoAAAANSUhEUgAAD6IAAAACCAYAAABIFvMzAAAAPUlEQVR4nO3OoQEAAAgDoP3/9EzeoIFAJ00KAAAAAAAAAAAAAAAAAAAAK9cBAAAAAAAAAAAAAAAAAAAAfhkmlU0biXxThgAAAABJRU5ErkJggg=="
    } else {
        "iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC"
    };
    InputPart::Media(Media::image_bytes(
        bytes::Bytes::from(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        ),
        "image/png".parse().unwrap(),
    ))
}

#[tokio::test]
async fn explicit_model_image_limits_override_host_fallback() {
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).preset.image_input_limits = Some(ImageInputLimits {
        max_width: 2,
        max_height: 2,
        max_bytes: 1_000,
    });
    let prepared = prepare_user_images(
        UserInput::from(vec![image_input_fixture(false)]),
        &model,
        None,
    )
    .await
    .unwrap();
    let InputPart::Media(Media::Image(image)) = &prepared.parts[0] else {
        panic!("image expected")
    };
    let ImageSource::Inline(bytes) = &image.source else {
        panic!("inline image expected")
    };
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 2);
}

#[tokio::test]
async fn user_images_are_prepared_before_history_and_invalid_batches_are_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        Session::create(directory.path().join("images.jsonl")).unwrap(),
        ExtensionHost::new(),
    );
    let invalid = UserInput::from(vec![
        image_input_fixture(false),
        InputPart::Media(Media::image_bytes(
            bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\ntruncated"),
            "image/png".parse().unwrap(),
        )),
    ]);
    assert!(matches!(
        agent.prompt(invalid).await,
        Err(AgentError::ImageInput(ImageInputError::InvalidImage))
    ));
    assert!(agent.session().entries().is_empty());
    let run = agent
        .prompt(UserInput::from(vec![
            InputPart::Text("describe".into()),
            image_input_fixture(true),
        ]))
        .await
        .unwrap();
    drop(run);
    let context = agent.session().context().unwrap();
    let Message::User(user) = &context[0] else {
        panic!("user history expected")
    };
    assert!(matches!(&user.content[0], UserPart::Text(text) if text == "describe"));
    let UserPart::Media(Media::Image(image)) = &user.content[1] else {
        panic!("image history expected")
    };
    let ImageSource::Inline(bytes) = &image.source else {
        panic!("inline image expected")
    };
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    assert!(width <= FALLBACK_IMAGE_LIMITS.max_width);
    assert_eq!(
        image.media_type.as_ref().map(octet_ai::Mime::essence_str),
        Some("image/png")
    );
}

#[tokio::test]
async fn image_batches_are_bounded_before_decode_and_abort_before_append() {
    let directory = tempfile::tempdir().unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        Session::create(directory.path().join("bounded-images.jsonl")).unwrap(),
        ExtensionHost::new(),
    );
    let many = UserInput::from(
        (0..9)
            .map(|_| image_input_fixture(false))
            .collect::<Vec<_>>(),
    );
    assert!(matches!(
        agent.prompt(many).await,
        Err(AgentError::ImageInputBatchLimit)
    ));
    assert!(agent.session().entries().is_empty());
    let bytes = UserInput::from(
        (0..5)
            .map(|_| {
                InputPart::Media(Media::image_bytes(
                    bytes::Bytes::from(vec![0; 4 * 1024 * 1024 + 1]),
                    "image/png".parse().unwrap(),
                ))
            })
            .collect::<Vec<_>>(),
    );
    assert!(matches!(
        agent.prompt(bytes).await,
        Err(AgentError::ImageInputBatchLimit)
    ));
    assert!(agent.session().entries().is_empty());

    let abort = AbortFlag::default();
    abort.set();
    let bounded = UserInput::from(
        (0..8)
            .map(|_| image_input_fixture(true))
            .collect::<Vec<_>>(),
    );
    assert!(matches!(
        prepare_user_images(bounded, &agent.model, Some(&abort)).await,
        Err(AgentError::Cancelled)
    ));
    assert!(agent.session().entries().is_empty());
}

#[tokio::test]
async fn invalid_queued_image_never_enters_history_and_releases_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("queued-images.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let tracker = ContextTracker::default();
    let observation = ContextObservation {
        tracker: &tracker,
        model: &model,
        system: "",
        tools: &[],
    };
    let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    control
        .try_steer(UserInput::from(vec![
            image_input_fixture(false),
            InputPart::Media(Media::image_bytes(
                bytes::Bytes::from_static(b"invalid"),
                "image/png".parse().unwrap(),
            )),
        ]))
        .unwrap();
    let Control::Steer(input) = rx.recv().await.unwrap() else {
        panic!("steering expected")
    };
    let result = deliver_control_inputs(
        vec![input],
        ControlDeliveryKind::Steering,
        &mut session,
        &EntryMetadata::default(),
        &mut None,
        &observation,
        None,
    )
    .await;
    assert!(matches!(
        result,
        ControlDelivery::Interrupted {
            event: None,
            finish: FinishReason::Failed(AgentError::ImageInput(ImageInputError::InvalidImage))
        }
    ));
    assert!(session.entries().is_empty());
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
    assert_eq!(
        control.pending_bytes.available_permits(),
        MAX_PENDING_CONTROL_BYTES
    );
}

fn test_run_control(byte_limit: usize) -> (RunControl, mpsc::Receiver<Control>) {
    let (tx, rx) = mpsc::channel(8);
    (
        RunControl {
            reasoning_model: None,
            ultra_observed: false,
            admission: Arc::new(Mutex::new(true)),
            tx,
            pending_count: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONTROL_INPUTS)),
            pending_bytes: Arc::new(tokio::sync::Semaphore::new(byte_limit)),
            abort: Arc::new(AbortFlag::default()),
        },
        rx,
    )
}

fn gate_candidate(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantPart::Text(text.to_owned())],
        model: octet_ai::ModelId("test".into()),
        protocol: Protocol::OpenAiChat,
    }
}

#[test]
fn incomplete_terminal_response_diagnostic_is_content_free_and_bounded() {
    let hostile = "private-payload-\u{1b}[31m\n".repeat(512);
    let assistant = AssistantMessage {
        content: vec![
            AssistantPart::Text(hostile.clone()),
            AssistantPart::Reasoning(octet_ai::ReasoningPart {
                text: Some(hostile.clone()),
                state: None,
            }),
            AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: octet_ai::ToolCallId(hostile.clone()),
                name: hostile.clone(),
                arguments_json: hostile.clone(),
                argument_error: None,
            }),
        ],
        model: octet_ai::ModelId(hostile.clone()),
        protocol: Protocol::OpenAiChat,
    };
    let usage = Usage {
        output_tokens: u64::MAX,
        reasoning_tokens: u64::MAX,
        ..Usage::default()
    };
    let mut diagnostics = vec![
        octet_ai::Diagnostic {
            code: "chat_defaulted_stop_reason".to_owned(),
            message: hostile.clone(),
        },
        octet_ai::Diagnostic {
            code: "chat_usage_missing-untrusted-code".to_owned(),
            message: hostile.clone(),
        },
    ];
    for usage_missing in [false, true] {
        if usage_missing {
            diagnostics.push(octet_ai::Diagnostic {
                code: "chat_usage_missing".to_owned(),
                message: hostile.clone(),
            });
        }
        let reason = incomplete_terminal_response_reason(
            &assistant,
            &StopReason::Other(hostile.clone()),
            &usage,
            &diagnostics,
            u64::MAX,
        );
        assert!(reason.starts_with("provider returned reasoning but no answer text"));
        assert!(reason.contains("stop=other; chat_stop_defaulted=true"));
        assert_eq!(reason.contains("usage=not_reported"), usage_missing);
        assert_eq!(reason.contains("usage=canonical"), !usage_missing);
        assert_eq!(
            reason.contains("; output_tokens=18446744073709551615;"),
            !usage_missing
        );
        assert_eq!(
            reason.contains("reasoning_tokens=18446744073709551615;"),
            !usage_missing
        );
        assert!(reason.contains("request_max_output_tokens=18446744073709551615"));
        assert!(reason.len() < 320);
        let public = public_error_diagnostic(
            &AgentError::IncompleteResponse {
                stop_reason: reason,
            },
            "test",
            "test",
        );
        assert!(public.contains("not automatically retried"));
        for forbidden in ["private-payload", "untrusted-code", "\u{1b}", "\n"] {
            assert!(!public.contains(forbidden));
        }
    }
}

#[test]
fn compaction_summary_parts_reject_empty_whitespace_and_oversize() {
    for invalid in [
        "".to_owned(),
        " \n\t ".to_owned(),
        "x".repeat(MAX_COMPACTION_HANDOFF_BYTES + 1),
    ] {
        assert!(matches!(
            validate_compaction_summary_part(&invalid),
            Err(AgentError::IncompleteResponse { .. })
        ));
    }
    validate_compaction_summary_part("## Goal\ncontinue").expect("normal summaries remain valid");

    let mut main = "## Goal\ncontinue".to_owned();
    let original = main.clone();
    assert!(matches!(
        append_compaction_turn_prefix(&mut main, " \n\t "),
        Err(AgentError::IncompleteResponse { .. })
    ));
    assert_eq!(main, original, "an invalid split prefix is never merged");
}

#[test]
fn glm_sized_default_threshold_does_not_compact_a_120k_request() {
    let context_window = 1_310_720u64;
    let estimate = 120_000u64;
    let reserve = DEFAULT_COMPACTION_RESERVE_TOKENS;
    let over_capacity = estimate > context_window.saturating_sub(reserve);
    assert!(!over_capacity, "this case is not Overflow recovery");
    for (fraction, expected_threshold) in [(1.0, false), (0.09, true)] {
        let threshold = ((context_window as f64) * fraction).floor() as u64;
        let over_threshold = estimate.saturating_add(reserve) > threshold;
        assert_eq!(over_threshold, expected_threshold, "fraction={fraction}");
    }
}

struct CompactionSummaryScript {
    responses: Mutex<VecDeque<String>>,
    requests: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for CompactionSummaryScript {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        assert!(
            request.tools.is_empty(),
            "compaction summaries are tool-free"
        );
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let text = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted response");
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text(text)],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: None,
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

fn compaction_test_agent(
    directory: &std::path::Path,
    script: Arc<CompactionSummaryScript>,
) -> Agent {
    let mut session = Session::create(directory.join("compaction-guard.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("original user context".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("original assistant context".into())],
            model: octet_ai::ModelId("test".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    let mut agent = active_tool_test_agent(directory, session, ExtensionHost::new());
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), script);
    // The tiny threshold forces the actual autonomous compaction path while
    // retaining enough context budget for the normal successful case.
    agent
        .set_compaction_token_policy(true, 0.000_01, 1)
        .unwrap();
    agent
}

struct ScriptedBitmapRenderer {
    calls: std::sync::atomic::AtomicUsize,
    frames: Vec<Vec<u8>>,
}

#[async_trait::async_trait]
impl CompactionStrategy for Arc<ScriptedBitmapRenderer> {
    async fn render(&self, _: &str, text: &str, _: &str) -> Result<Vec<Vec<u8>>, String> {
        assert!(text.contains("original user context"));
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.frames.clone())
    }
}

fn bitmap_compaction_test_agent(
    directory: &std::path::Path,
    strategy: impl CompactionStrategy + 'static,
    script: Arc<CompactionSummaryScript>,
) -> Agent {
    let mut session = Session::create(directory.join("bitmap-compaction.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("original user context".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("original assistant context".into())],
            model: octet_ai::ModelId("test".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    let mut extensions = ExtensionHost::new();
    extensions.compaction_strategy(strategy);
    let mut agent = active_tool_test_agent(directory, session, extensions);
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), script);
    agent
        .set_compaction_token_policy(true, 0.000_01, 1)
        .unwrap();
    agent
}

#[tokio::test]
async fn vision_compaction_bypasses_parent_summary_and_bad_frames_keep_history() {
    let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC")
            .unwrap();
    for frames in [
        vec![png],
        vec![b"\x89PNG\r\n\x1a\ntruncated".to_vec()],
        vec![],
    ] {
        let directory = tempfile::tempdir().unwrap();
        let valid = frames.first().is_some_and(|frame| frame.len() > 30);
        let renderer = Arc::new(ScriptedBitmapRenderer {
            calls: std::sync::atomic::AtomicUsize::new(0),
            frames,
        });
        let script = Arc::new(CompactionSummaryScript {
            responses: Mutex::new(VecDeque::from(["normal answer".into()])),
            requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut agent = bitmap_compaction_test_agent(
            directory.path(),
            Arc::clone(&renderer),
            Arc::clone(&script),
        );
        let result = agent.complete("new task").await;
        assert_eq!(renderer.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            script.requests.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(valid),
            "the parent model must not receive a compaction summary request"
        );
        assert_eq!(result.is_ok(), valid);
        assert_eq!(agent.session().has_snapcompact_context().unwrap(), valid);
        if valid {
            assert!(agent.session().context().unwrap().iter().any(|message| matches!(message,
                    Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Media(Media::Image(_))))
                )));
            Arc::make_mut(&mut agent.model.spec)
                .capabilities
                .input_modalities = octet_ai::ModalitySet::none();
            assert!(matches!(
                agent.complete("text-only follow-up").await,
                Err(AgentError::InvalidCompactionPolicy(_))
            ));
            assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        } else {
            assert!(agent
                .session()
                .entries()
                .iter()
                .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
            let original = format!("{:?}", agent.session().context().unwrap());
            assert!(original.contains("original user context"));
            assert!(original.contains("original assistant context"));
        }
    }
}

struct SlowBitmapRenderer(watch::Sender<usize>);

#[async_trait::async_trait]
impl CompactionStrategy for SlowBitmapRenderer {
    async fn render(&self, _: &str, _: &str, _: &str) -> Result<Vec<Vec<u8>>, String> {
        self.0.send_modify(|calls| *calls += 1);
        tokio::time::sleep(Duration::from_secs(70)).await;
        Ok(vec![base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC")
                .unwrap()])
    }
}

#[tokio::test(start_paused = true)]
async fn slow_sequential_bitmap_chunks_share_one_deadline_and_leave_history() {
    let directory = tempfile::tempdir().unwrap();
    let (tx, mut calls) = watch::channel(0usize);
    let script = Arc::new(CompactionSummaryScript {
        responses: Mutex::new(VecDeque::new()),
        requests: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut agent = bitmap_compaction_test_agent(directory.path(), SlowBitmapRenderer(tx), script);
    agent
        .session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("x".repeat(3_000))],
        })))
        .unwrap();
    let task = tokio::spawn(async move {
        let result = agent.complete("new task").await;
        (agent, result)
    });
    calls.changed().await.unwrap();
    tokio::time::advance(Duration::from_secs(70)).await;
    for _ in 0..10_000 {
        if *calls.borrow() >= 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(*calls.borrow(), 2, "source must span at least two chunks");
    tokio::time::advance(Duration::from_secs(50)).await;
    let (agent, result) = task.await.unwrap();
    assert!(
        matches!(result, Err(AgentError::InvalidCompactionPolicy(ref error)) if error.contains("deadline"))
    );
    assert!(!agent.session().has_snapcompact_context().unwrap());
    assert!(agent
        .session()
        .entries()
        .iter()
        .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
    assert!(format!("{:?}", agent.session().context().unwrap()).contains("original user context"));
}

#[tokio::test]
async fn two_bitmap_compactions_preserve_source_and_separate_transcript_sections() {
    let directory = tempfile::tempdir().unwrap();
    let renderer = Arc::new(ScriptedBitmapRenderer {
            calls: std::sync::atomic::AtomicUsize::new(0),
            frames: vec![base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC")
                .unwrap()],
        });
    let script = Arc::new(CompactionSummaryScript {
        responses: Mutex::new(VecDeque::from([
            "first answer".into(),
            "second answer".into(),
        ])),
        requests: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut agent = bitmap_compaction_test_agent(directory.path(), Arc::clone(&renderer), script);
    agent.complete("first new task").await.unwrap();
    let first = agent
        .session()
        .entries()
        .iter()
        .find_map(|entry| match &entry.value {
            EntryValue::Compaction {
                snapcompact: Some(checkpoint),
                ..
            } => Some(checkpoint.source_text.clone()),
            _ => None,
        })
        .expect("first bitmap checkpoint");
    agent.complete("second new task").await.unwrap();
    let sources = agent
        .session()
        .entries()
        .iter()
        .filter_map(|entry| match &entry.value {
            EntryValue::Compaction {
                snapcompact: Some(checkpoint),
                ..
            } => Some(&checkpoint.source_text),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert!(sources[1].starts_with(&first));
    assert!(
        sources[1].contains("\n\n[User]: first new task"),
        "{}",
        sources[1]
    );
    assert_eq!(renderer.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn bitmap_source_separates_previous_full_turn_and_split_turn_prefix() {
    let user = |text: &str| {
        Message::User(UserMessage {
            content: vec![UserPart::Text(text.into())],
        })
    };
    let preparation = HandoffPreparation {
        previous_summary: Some("[User]: previous".into()),
        messages: vec![user("new full turn")],
        turn_prefix_messages: vec![user("split prefix")],
        details: Default::default(),
    };
    assert_eq!(
        snapcompact_source(&preparation),
        "[User]: previous\n\n[User]: new full turn\n\n[User]: split prefix"
    );
}

#[tokio::test]
async fn text_only_compaction_keeps_parent_summary_path() {
    let directory = tempfile::tempdir().unwrap();
    let renderer = Arc::new(ScriptedBitmapRenderer {
        calls: std::sync::atomic::AtomicUsize::new(0),
        frames: Vec::new(),
    });
    let script = Arc::new(CompactionSummaryScript {
        responses: Mutex::new(VecDeque::from([
            "## Goal\nvalid checkpoint".into(),
            "normal answer".into(),
        ])),
        requests: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut agent =
        bitmap_compaction_test_agent(directory.path(), Arc::clone(&renderer), Arc::clone(&script));
    Arc::make_mut(&mut agent.model.spec)
        .capabilities
        .input_modalities = octet_ai::ModalitySet::none();
    assert!(agent.complete("new task").await.is_ok());
    assert_eq!(renderer.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(!agent.session().has_snapcompact_context().unwrap());
}

#[tokio::test]
async fn provider_compaction_refuses_invalid_summary_without_discarding_context() {
    for invalid in [
        "".to_owned(),
        " \n\t ".to_owned(),
        "x".repeat(MAX_COMPACTION_HANDOFF_BYTES + 1),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let script = Arc::new(CompactionSummaryScript {
            responses: Mutex::new(VecDeque::from([invalid])),
            requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut agent = compaction_test_agent(directory.path(), Arc::clone(&script));
        let error = agent.complete("new task").await.unwrap_err();
        assert!(matches!(error, AgentError::IncompleteResponse { .. }));
        assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(agent
            .session()
            .entries()
            .iter()
            .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
        let retained = format!("{:?}", agent.session().context().unwrap());
        assert!(retained.contains("original user context"));
        assert!(retained.contains("original assistant context"));
    }

    let directory = tempfile::tempdir().unwrap();
    let script = Arc::new(CompactionSummaryScript {
        responses: Mutex::new(VecDeque::from([
            "## Goal\nvalid checkpoint".into(),
            "normal answer".into(),
        ])),
        requests: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut agent = compaction_test_agent(directory.path(), Arc::clone(&script));
    let output = agent.complete("new task").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(
        script.requests.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "one summary request then one normal turn"
    );
    assert!(format!("{:?}", agent.session().context().unwrap()).contains("normal answer"));
    assert!(agent
        .session()
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

struct ToolBudgetTransport {
    calls: std::sync::atomic::AtomicUsize,
    expected_tool_count: usize,
}

#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for ToolBudgetTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        assert_eq!(request.tools.len(), self.expected_tool_count);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text("accepted".into())],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: None,
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

#[test]
fn tool_schema_budget_counts_exact_json_and_refuses_without_tool_rewriting() {
    require_tool_schema_budget(&[], 0).expect("zero budget permits no provider tools");
    let tools = vec![ToolDef {
        async_execution: false,
        name: "schema-tool".into(),
        description: "private description must not enter the diagnostic".into(),
        parameters: serde_json::json!({"type": "object", "properties": {"payload": {"type": "string"}}}),
        constrained_sampling: None,
    }];
    let bytes = tool_schema_bytes(&tools);
    require_tool_schema_budget(&tools, bytes).expect("exactly at budget is accepted");
    let error = require_tool_schema_budget(&tools, bytes - 1).unwrap_err();
    assert!(matches!(
        &error,
        AgentError::ToolSchemaBudgetExceeded {
            actual_bytes,
            tool_count: 1,
            max_bytes,
        } if *actual_bytes == bytes && *max_bytes == bytes - 1
    ));
    let diagnostic = error.to_string();
    assert!(diagnostic.len() < 256);
    assert!(!diagnostic.contains("private description"));
    assert_eq!(tools[0].name, "schema-tool", "refusal never rewrites tools");
}

#[tokio::test]
async fn tool_schema_budget_preflight_refuses_without_persisting_the_prompt_or_calling_provider() {
    let directory = tempfile::tempdir().unwrap();
    let extensions = active_tool_test_extensions(&["schema"]);
    let mut agent = active_tool_test_agent(
        directory.path(),
        Session::create(directory.path().join("schema-budget.jsonl")).unwrap(),
        extensions,
    );
    let tool_count = agent.registered_tool_definitions().len();
    let budget = tool_schema_bytes(&agent.registered_tool_definitions());
    let transport = Arc::new(ToolBudgetTransport {
        calls: std::sync::atomic::AtomicUsize::new(0),
        expected_tool_count: tool_count,
    });
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), transport.clone());

    agent.set_tool_schema_budget_bytes(budget - 1);
    assert!(matches!(
        agent.prompt("retryable draft").await,
        Err(AgentError::ToolSchemaBudgetExceeded { .. })
    ));
    assert!(agent.session().entries().is_empty());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);

    agent.set_tool_schema_budget_bytes(budget);
    assert!(matches!(
        agent.complete("accepted draft").await.unwrap().reason,
        FinishReason::Completed
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_gate_receipts_retain_first_and_last_twelve_online() {
    for total in [0usize, 24, 25, 10_000] {
        let mut evidence = TerminalGateEvidence::default();
        TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(0));
        for index in 0..total {
            evidence.record_action(
                &format!("tool-{index}"),
                &format!("args-{index}"),
                index % 2 == 0,
                &format!("result-{index}"),
            );
            assert_eq!(evidence.receipts.len(), (index + 1).min(24));
            assert_eq!(evidence.actions_omitted, (index + 1).saturating_sub(24));
        }
        assert_eq!(
            TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.get()),
            3 * total
        );
        let expected = if total <= 24 {
            (0..total).collect::<Vec<_>>()
        } else {
            (0..12).chain(total - 12..total).collect::<Vec<_>>()
        };
        let capsule = terminal_gate_capsule(&evidence, &gate_candidate("done"));
        let capsule: serde_json::Value = serde_json::from_str(&capsule).unwrap();
        assert_eq!(capsule["actions_omitted"], total.saturating_sub(24));
        let actions = capsule["actions"].as_array().unwrap();
        assert_eq!(actions.len(), expected.len());
        for (action, index) in actions.iter().zip(expected) {
            assert_eq!(action["tool"], format!("tool-{index}"));
            assert_eq!(action["arguments"], format!("args-{index}"));
            assert_eq!(action["result"], format!("result-{index}"));
            assert_eq!(
                action["status"],
                if index % 2 == 0 { "error" } else { "ok" }
            );
        }
    }
}

#[test]
fn terminal_gate_projects_unicode_receipts_at_insertion_not_at_each_attempt() {
    let text = "界🙂e\u{301}".repeat(2_000);
    let mut evidence = TerminalGateEvidence::default();
    evidence.record_action("read", &text, false, &text);
    let receipt = &evidence.receipts[0];
    for (projected, limit) in [
        (&receipt.arguments, TERMINAL_GATE_ARGUMENT_LIMIT),
        (&receipt.result, TERMINAL_GATE_RESULT_LIMIT),
    ] {
        let chars = text.chars().collect::<Vec<_>>();
        let half = (limit - 32) / 2;
        let head = chars[..half].iter().collect::<String>();
        let tail = chars[chars.len() - half..].iter().collect::<String>();
        assert_eq!(
            projected,
            &format!("{head}\n[… 8000 chars total …]\n{tail}")
        );
        assert!(projected.chars().count() <= limit);
        assert!(projected.len() <= limit * 4);
    }
    let candidate = gate_candidate("done");
    TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(0));
    let first = terminal_gate_capsule(&evidence, &candidate);
    for _ in 0..10 {
        assert_eq!(terminal_gate_capsule(&evidence, &candidate), first);
    }
    // Repeated gate attempts project only the new candidate, never all
    // already-projected action arguments/results or retained requests.
    assert_eq!(TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.get()), 11);
    let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(parsed["actions"][0]["arguments"], receipt.arguments);
    assert_eq!(parsed["actions"][0]["result"], receipt.result);
    assert_eq!(text.chars().count(), 8_000);
}

#[test]
fn terminal_gate_requests_bound_count_bytes_and_preserve_initial_and_latest() {
    for unit in ["x", "🙂"] {
        let body = unit.repeat(4_000);
        let request = |index| format!("request-{index}: {body}");
        let mut evidence = TerminalGateEvidence::default();
        for index in 0..1_000 {
            evidence.record_request(&request(index));
            assert!(evidence.requests.len() <= TERMINAL_GATE_REQUEST_LIMIT);
            assert!(evidence.request_bytes <= TERMINAL_GATE_REQUEST_BYTES);
            assert_eq!(
                evidence.request_bytes,
                evidence.requests.iter().map(String::len).sum::<usize>()
            );
            assert_eq!(
                evidence.requests_omitted + evidence.requests.len(),
                index + 1
            );
        }
        assert_eq!(evidence.requests[0], bounded_gate_text(&request(0), 3_000));
        let retained_suffix = evidence.requests.len() - 1;
        for (summary, index) in evidence
            .requests
            .iter()
            .skip(1)
            .zip(1_000 - retained_suffix..1_000)
        {
            assert_eq!(summary, &bounded_gate_text(&request(index), 3_000));
        }
        if unit == "🙂" {
            assert!(evidence.requests.len() < TERMINAL_GATE_REQUEST_LIMIT);
        } else {
            assert_eq!(evidence.requests.len(), TERMINAL_GATE_REQUEST_LIMIT);
        }
    }
    let mut empty = TerminalGateEvidence::default();
    for _ in 0..10_000 {
        empty.record_request("");
    }
    assert_eq!(empty.requests.len(), TERMINAL_GATE_REQUEST_LIMIT);
    assert_eq!(empty.requests_omitted, 10_000 - TERMINAL_GATE_REQUEST_LIMIT);
    assert_eq!(empty.request_bytes, 0);
}

#[test]
fn terminal_gate_capsule_stays_bounded_across_repeated_requests_and_decisions() {
    // NUL takes six JSON bytes per character, worse than UTF-8 or quotes.
    let text = "\0".repeat(TERMINAL_GATE_TEXT_LIMIT);
    let candidate = gate_candidate(&text);
    let mut evidence = TerminalGateEvidence {
        prior_context: text.clone(),
        ..TerminalGateEvidence::default()
    };
    for index in 0..512usize {
        evidence.record_request(&text);
        evidence.record_action(&text, &text, index % 2 == 0, &text);
        if [23, 24, 255, 511].contains(&index) {
            let capsule = terminal_gate_capsule(&evidence, &candidate);
            assert!(capsule.len() <= TERMINAL_GATE_CAPSULE_BYTES);
            let parsed: serde_json::Value = serde_json::from_str(&capsule).unwrap();
            assert_eq!(
                parsed["requests_omitted"],
                index + 1 - evidence.requests.len()
            );
            assert_eq!(parsed["actions_omitted"], (index + 1).saturating_sub(24));
            assert_eq!(
                parsed["requests"].as_array().unwrap().len(),
                evidence.requests.len()
            );
            assert_eq!(terminal_gate_capsule(&evidence, &candidate), capsule);
        }
    }
}

#[tokio::test]
async fn natural_run_has_no_terminal_gate_summary_projection_or_evidence_collection() {
    struct Script(Mutex<VecDeque<Vec<AssistantPart>>>);
    #[async_trait::async_trait]
    impl octet_ai::HostStreamTransport for Script {
        async fn stream(
            &self,
            model: octet_ai::HostStreamModel,
            _: Request,
            _: Vec<octet_ai::Diagnostic>,
        ) -> Result<octet_ai::ResponseStream, AiError> {
            let content = self.0.lock().unwrap().pop_front().expect("scripted turn");
            let stop_reason = if content
                .iter()
                .any(|part| matches!(part, AssistantPart::ToolCall(_)))
            {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            };
            Ok(Box::pin(futures_util::stream::iter([
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::Finished(octet_ai::Response {
                    message: AssistantMessage {
                        content,
                        model: model.id,
                        protocol: model.protocol,
                    },
                    stop_reason,
                    usage: Usage::default(),
                    cost: None,
                    response_id: None,
                    responses_output: None,
                    deferred: None,
                    inference: None,
                    diagnostics: Vec::new(),
                })),
            ])))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        Session::create(directory.path().join("natural-evidence.jsonl")).unwrap(),
        ExtensionHost::new(),
    );
    agent.max_turns = Some(3);
    let arguments = serde_json::json!({"payload": "x".repeat(16_000)}).to_string();
    let script = Arc::new(Script(Mutex::new(VecDeque::from([
        vec![AssistantPart::ToolCall(ToolCall {
            async_execution: false,
            id: octet_ai::ToolCallId("unknown-call".into()),
            name: "unregistered".into(),
            arguments_json: arguments.clone(),
            argument_error: None,
        })],
        vec![AssistantPart::Text("done".into())],
    ]))));
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), script.clone());
    let initial = UserInput::from("initial 🙂".repeat(2_000));
    TERMINAL_GATE_INITIAL_SUMMARIES.with(|count| count.set(0));
    TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(0));
    assert!(
        TerminalGateEvidence::for_run(CompletionPolicy::Natural, agent.session(), &initial)
            .unwrap()
            .is_none()
    );
    let mut run = agent.prompt(initial).await.unwrap();
    run.control().steer("steer 🙂".repeat(2_000)).await.unwrap();
    let mut delivered = false;
    let mut tool_finished = false;
    let mut completed = false;
    while let Some(event) = run.next().await {
        match event {
            AgentEvent::SteeringDelivered { messages } => {
                assert_eq!(messages, vec!["steer 🙂".repeat(2_000)]);
                delivered = true;
            }
            AgentEvent::ToolFinished { .. } => tool_finished = true,
            AgentEvent::RunFinished { reason, .. } => {
                assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                completed = true;
            }
            _ => {}
        }
    }
    drop(run);
    assert!(delivered && tool_finished && completed);
    assert!(script.0.lock().unwrap().is_empty());
    let context = agent.session().context().unwrap();
    assert_eq!(
        message_visible_text(&context[0]).unwrap(),
        "initial 🙂".repeat(2_000)
    );
    assert!(context.iter().any(|message| matches!(message,
        Message::Assistant(assistant) if assistant.content.iter().any(|part| matches!(part,
            AssistantPart::ToolCall(call) if call.arguments_json == arguments
        ))
    )));
    assert_eq!(TERMINAL_GATE_INITIAL_SUMMARIES.with(|count| count.get()), 0);
    assert_eq!(TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.get()), 0);
}

#[tokio::test]
async fn terminal_gate_control_evidence_preserves_delivery_and_reservations() {
    for policy in [CompletionPolicy::Natural, CompletionPolicy::TerminalGate] {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("control-evidence.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let tracker = ContextTracker::default();
        let observation = ContextObservation {
            tracker: &tracker,
            model: &model,
            system: "",
            tools: &[],
        };
        let initial = UserInput::from("initial request");
        let mut evidence = TerminalGateEvidence::for_run(policy, &session, &initial).unwrap();
        session.append(user_message(initial)).unwrap();
        let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        let mut expected = vec!["initial request".to_owned()];
        for index in 0..24 {
            let text = format!("control-{index}: {}", "🙂".repeat(4_000));
            let input = UserInput::from(text.clone());
            let bytes = control_input_bytes(&input);
            let kind = match index % 3 {
                0 => {
                    control.steer(input).await.unwrap();
                    ControlDeliveryKind::Steering
                }
                1 => {
                    control.follow_up(input).await.unwrap();
                    ControlDeliveryKind::FollowUp
                }
                _ => {
                    control.finish_now(input).await.unwrap();
                    ControlDeliveryKind::Steering
                }
            };
            let reserved = match rx.recv().await.unwrap() {
                Control::Steer(input) | Control::FollowUp(input) | Control::FinishNow(input) => {
                    input
                }
                _ => panic!("semantic control"),
            };
            assert_eq!(
                control.pending_count.available_permits(),
                MAX_PENDING_CONTROL_INPUTS - 1
            );
            assert_eq!(
                control.pending_bytes.available_permits(),
                MAX_PENDING_CONTROL_BYTES - bytes
            );
            let delivered = deliver_control_inputs(
                vec![reserved],
                kind,
                &mut session,
                &EntryMetadata::default(),
                &mut evidence,
                &observation,
                None,
            )
            .await;
            let ControlDelivery::Completed { event: Some(event) } = delivered else {
                panic!("durable delivery must succeed")
            };
            let messages = match event {
                AgentEvent::SteeringDelivered { messages }
                | AgentEvent::FollowUpDelivered { messages } => messages,
                _ => panic!("delivery acknowledgement"),
            };
            assert_eq!(messages, vec![text.clone()]);
            expected.push(text);
            assert_eq!(
                control.pending_count.available_permits(),
                MAX_PENDING_CONTROL_INPUTS
            );
            assert_eq!(
                control.pending_bytes.available_permits(),
                MAX_PENDING_CONTROL_BYTES
            );
        }
        let persisted = session
            .context()
            .unwrap()
            .iter()
            .map(|message| message_visible_text(message).expect("complete delivered input"))
            .collect::<Vec<_>>();
        assert_eq!(persisted, expected);
        if let Some(evidence) = evidence {
            assert_eq!(evidence.requests.front().unwrap(), "initial request");
            assert_eq!(
                evidence.requests.back().unwrap(),
                &bounded_gate_text(expected.last().unwrap(), 3_000)
            );
            assert_eq!(
                evidence.requests_omitted,
                expected.len() - evidence.requests.len()
            );
            assert!(evidence.request_bytes <= TERMINAL_GATE_REQUEST_BYTES);
            assert!(evidence.requests_omitted > 0);
        } else {
            assert_eq!(policy, CompletionPolicy::Natural);
        }
    }
}

#[tokio::test]
async fn steering_receipt_claim_linearizes_before_persistence() {
    let (control, _rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    let (prepared, receipt) = control
        .prepare_steer("claimed but not yet persisted")
        .unwrap();
    let payload = ReservedInput::Retractable(prepared).claim().unwrap();
    // The durable append has not happened, but delivery already owns this
    // exact payload. Neither receipt clone may recall it now.
    assert!(!receipt.is_pending());
    assert!(!receipt.clone().try_retract());
    assert!(!receipt.try_retract());
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS - 1
    );
    drop(payload);
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
}

#[tokio::test]
async fn steering_receipt_racing_recall_and_claim_have_exactly_one_winner() {
    let (control, _rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    for _ in 0..64 {
        let (prepared, receipt) = control
            .prepare_steer("same text, separate authority")
            .unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let recalled = scope.spawn(|| {
                barrier.wait();
                receipt.try_retract()
            });
            barrier.wait();
            let payload = ReservedInput::Retractable(prepared).claim();
            assert_ne!(payload.is_some(), recalled.join().unwrap());
            drop(payload);
        });
        assert!(!receipt.is_pending());
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
        assert_eq!(
            control.pending_bytes.available_permits(),
            MAX_PENDING_CONTROL_BYTES
        );
    }
}

#[tokio::test]
async fn pending_controls_stay_reserved_after_ingress_drain_until_durable_delivery() {
    let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    let mut pending = Vec::new();
    for index in 0..MAX_PENDING_CONTROL_INPUTS {
        control.follow_up(format!("input-{index}")).await.unwrap();
        let Control::FollowUp(input) = rx.recv().await.unwrap() else {
            panic!("follow-up")
        };
        pending.push(input);
    }
    assert_eq!(control.pending_count.available_permits(), 0);
    assert!(matches!(
        control.steer("rejected").await,
        Err(AgentError::ControlQueueFull)
    ));
    assert!(matches!(
        control.finish_now("rejected").await,
        Err(AgentError::ControlQueueFull)
    ));
    // Mode/cancellation controls do not spend semantic-input reservations.
    control
        .set_follow_up_mode(QueueDeliveryMode::OneAtATime)
        .await
        .unwrap();
    assert!(matches!(rx.recv().await, Some(Control::SetFollowUpMode(_))));

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("controls.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let tracker = ContextTracker::default();
    let observation = ContextObservation {
        tracker: &tracker,
        model: &model,
        system: "",
        tools: &[],
    };
    let mut gate = None;
    let delivered = deliver_control_inputs(
        pending,
        ControlDeliveryKind::FollowUp,
        &mut session,
        &EntryMetadata::default(),
        &mut gate,
        &observation,
        None,
    )
    .await;
    let ControlDelivery::Completed {
        event: Some(AgentEvent::FollowUpDelivered { messages }),
    } = delivered
    else {
        panic!("durable delivery must be acknowledged")
    };
    assert_eq!(
        messages,
        (0..MAX_PENDING_CONTROL_INPUTS)
            .map(|i| format!("input-{i}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(session.entries().len(), MAX_PENDING_CONTROL_INPUTS);
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
    assert_eq!(
        control.pending_bytes.available_permits(),
        MAX_PENDING_CONTROL_BYTES
    );
    control.try_steer("accepted again").unwrap();
}

#[tokio::test]
async fn control_byte_and_ingress_saturation_are_typed_and_rollback_reservations() {
    let input = UserInput::from(vec![InputPart::Media(Media::audio_bytes(
        bytes::Bytes::from_static(b"audio-payload"),
        octet_ai::AudioFormat::Wav,
    ))]);
    let bytes = control_input_bytes(&input);
    assert!(bytes >= b"audio-payload".len() + std::mem::size_of::<InputPart>());
    let (control, mut rx) = test_run_control(bytes);
    control.try_follow_up(input).unwrap();
    let held = rx.recv().await.unwrap();
    assert_eq!(control.pending_bytes.available_permits(), 0);
    assert!(matches!(
        control.try_steer("x"),
        Err(AgentError::ControlQueueFull)
    ));
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS - 1
    );
    control.abort();
    assert!(control.abort.is_set());
    drop(held);
    assert_eq!(control.pending_bytes.available_permits(), bytes);

    let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    for _ in 0..8 {
        control.try_steer("queued").unwrap();
    }
    let reserved = control.pending_bytes.available_permits();
    assert!(matches!(
        control.try_follow_up("full ingress"),
        Err(AgentError::ControlQueueFull)
    ));
    assert_eq!(control.pending_bytes.available_permits(), reserved);
    // A cancelled async admission was never accepted and frees its permit.
    use futures_util::FutureExt;
    assert!(control.steer("waiting").now_or_never().is_none());
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS - 8
    );
    rx.close();
    assert!(matches!(
        control.try_steer("ended"),
        Err(AgentError::RunEnded)
    ));
    drop(rx);
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
}

#[tokio::test]
async fn pending_control_reservations_release_on_failed_persistence_and_run_abort_or_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-delivery.jsonl");
    let mut session = Session::create(&path).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let tracker = ContextTracker::default();
    let observation = ContextObservation {
        tracker: &tracker,
        model: &model,
        system: "",
        tools: &[],
    };
    let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    control.try_steer("accepted before failure").unwrap();
    let Control::Steer(input) = rx.recv().await.unwrap() else {
        panic!("steer")
    };
    // A concurrent writer makes the session's observed-length fence fail.
    let mut other = Session::open(&path).unwrap();
    other.append(user_message("other writer".into())).unwrap();
    let result = deliver_control_inputs(
        vec![input],
        ControlDeliveryKind::Steering,
        &mut session,
        &EntryMetadata::default(),
        &mut None,
        &observation,
        None,
    )
    .await;
    assert!(matches!(
        result,
        ControlDelivery::Interrupted {
            event: None,
            finish: FinishReason::Failed(_)
        }
    ));
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
    assert!(session.entries().is_empty());

    for abort in [false, true] {
        let path = directory.path().join(format!("run-{abort}.jsonl"));
        let mut agent = active_tool_test_agent(
            directory.path(),
            Session::create(path).unwrap(),
            ExtensionHost::new(),
        );
        let mut run = agent.prompt("start").await.unwrap();
        let control = run.control();
        for _ in 0..8 {
            control.try_follow_up("accepted").unwrap();
        }
        if abort {
            control.abort();
            let mut finished = 0;
            while let Some(event) = run.next().await {
                if let AgentEvent::RunFinished { reason, .. } = event {
                    assert!(matches!(reason, FinishReason::Aborted));
                    finished += 1;
                    assert_eq!(
                        control.pending_count.available_permits(),
                        MAX_PENDING_CONTROL_INPUTS
                    );
                }
            }
            assert_eq!(finished, 1);
        }
        drop(run);
        assert!(matches!(
            control.try_follow_up("after termination"),
            Err(AgentError::RunEnded)
        ));
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
    }
}

#[test]
fn bash_owner_retirement_waits_for_overlapping_agents() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owner.jsonl");
    let first = active_tool_test_agent(
        directory.path(),
        Session::create(&path).unwrap(),
        ExtensionHost::new(),
    );
    let owner = first.resource_owner.clone();
    let mut second = active_tool_test_agent(
        directory.path(),
        Session::open(&path).unwrap(),
        ExtensionHost::new(),
    );
    assert_eq!(BASH_OWNER_LEASES.lock().unwrap()[&owner], 2);
    drop(first);
    assert_eq!(BASH_OWNER_LEASES.lock().unwrap()[&owner], 1);
    second
        .replace_session_at_idle(Session::open(&path).unwrap())
        .unwrap();
    assert_eq!(BASH_OWNER_LEASES.lock().unwrap()[&owner], 1);
    second
        .replace_session_at_idle(Session::create(directory.path().join("other.jsonl")).unwrap())
        .unwrap();
    assert!(!BASH_OWNER_LEASES.lock().unwrap().contains_key(&owner));
    let next_owner = second.resource_owner.clone();
    drop(second);
    assert!(!BASH_OWNER_LEASES.lock().unwrap().contains_key(&next_owner));
}

struct PromptTool {
    name: &'static str,
    snippet: Option<String>,
    guidelines: &'static [&'static str],
}

#[async_trait::async_trait]
impl Tool for PromptTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            async_execution: false,
            name: self.name.to_owned(),
            description: format!("{} tool", self.name),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        }
    }

    fn prompt_snippet(&self) -> Option<&str> {
        self.snippet.as_deref()
    }

    fn prompt_guidelines(&self) -> &[&str] {
        self.guidelines
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("executed"))
    }
}

#[test]
fn tool_prompt_section_renders_snippets_and_guidelines_and_skips_silent_tools() {
    let listed = PromptTool {
        name: "listed",
        snippet: Some("run the thing".to_owned()),
        guidelines: &["prefer flags", "report failures"],
    };
    let silent = PromptTool {
        name: "silent",
        snippet: None,
        guidelines: &["never rendered"],
    };
    let section = render_tool_prompt_section([&listed as &dyn Tool, &silent as &dyn Tool])
        .expect("one contributing tool renders a section");
    assert_eq!(
        section,
        "Available tools:\n- listed: run the thing\n  - prefer flags\n  - report failures"
    );
    // A registration that contributes nothing must not enlarge a prompt.
    assert!(render_tool_prompt_section([&silent as &dyn Tool]).is_none());
    assert!(render_tool_prompt_section(Vec::<&dyn Tool>::new()).is_none());
}

#[test]
fn tool_prompt_section_is_bounded_on_a_character_boundary() {
    let oversized = "é".repeat(MAX_TOOL_PROMPT_SECTION_BYTES);
    let huge = PromptTool {
        name: "huge",
        snippet: Some(oversized),
        guidelines: &[],
    };
    let section = render_tool_prompt_section([&huge as &dyn Tool]).unwrap();
    assert!(
        section.len() <= MAX_TOOL_PROMPT_SECTION_BYTES,
        "the section must respect its byte budget: {}",
        section.len()
    );
    assert!(
        section.ends_with('…'),
        "truncation is marked: {:?}",
        &section[section.len().saturating_sub(8)..]
    );
    assert!(
        section.is_char_boundary(section.len()),
        "a truncated section stays valid UTF-8"
    );
    // Truncation happens past the header and first entry, so the model still
    // sees which tool the elided detail belongs to.
    assert!(section.starts_with("Available tools:\n- huge: é"));
}

#[test]
fn prepared_turn_rejects_stale_durable_head_system_and_tool_generation() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("prepared-turn.jsonl")).unwrap();
    let request = Request {
        system: Some("system".into()),
        messages: Vec::new(),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(16),
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::default(),
        session_id: Some("session".into()),
    };
    let prepared = PreparedTurn::new(session.head(), "system".into(), 7, request, 3);

    assert!(prepared.is_current(&session, "system", 7));
    assert!(!prepared.is_current(&session, "changed", 7));
    assert!(!prepared.is_current(&session, "system", 8));

    session
        .append(user_message(UserInput::from("new durable input")))
        .unwrap();
    assert!(!prepared.is_current(&session, "system", 7));
}

#[test]
fn assistant_persistence_context_ignores_provider_metadata() {
    use octet_ai::{ModelId, ProviderPartMetadata};

    let assistant = AssistantMessage {
        content: vec![
            AssistantPart::Text("visible".into()),
            AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
                signature: "opaque-continuation".into(),
            }),
        ],
        model: ModelId("gemini-test".into()),
        protocol: Protocol::GoogleGenerativeAi,
    };

    let context = assistant_persistence_context("run", "owner", &assistant, StopReason::EndTurn);
    assert_eq!(context.text_bytes, "visible".len());
    assert_eq!(context.tool_call_count, 0);
    assert_eq!(context.reasoning_part_count, 0);
    assert_eq!(context.media_part_count, 0);
}

#[test]
fn idle_session_replacement_updates_the_durable_owner() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.jsonl");
    let replacement_path = directory.path().join("replacement.jsonl");
    let first = Session::create(&first_path).unwrap();
    let replacement = Session::create(&replacement_path).unwrap();
    let replacement_owner = replacement.resource_owner_key();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: first,
        system: "system".into(),
        sandbox: SandboxConfig::new(directory.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let first_owner = agent.resource_owner.clone();
    agent.set_prompt_display_text(Some("old session draft".into()));

    agent.replace_session_at_idle(replacement).unwrap();

    assert_eq!(agent.session().path(), replacement_path);
    assert_eq!(agent.resource_owner, replacement_owner);
    assert_eq!(agent.session_id, replacement_owner);
    assert_eq!(agent.prompt_display_text, None);
    assert_ne!(agent.resource_owner, first_owner);
}

#[test]
fn request_output_uses_provider_ceiling_then_clamps_to_remaining_context() {
    assert_eq!(
        resolve_request_max_output_tokens(200_000, 20_000, 65_536),
        65_536
    );
    // The remaining window is 30_000; the reserved headroom is 1% of the
    // window (2_000), so the request never sits on the boundary.
    assert_eq!(
        resolve_request_max_output_tokens(200_000, 170_000, 65_536),
        28_000
    );
}

/// Regression for a real local vLLM rejection: the model's window is
/// 131_072, octet's estimate was one token below the provider's count, and
/// the requested output filled the gap exactly, so the provider refused with
/// `prompt + requested > window`. A provider that counts one token more than
/// the estimate must still fit.
#[test]
fn request_output_keeps_estimator_slack_for_a_locally_served_model() {
    let window = 131_072;
    let estimated_input = 100_176;
    let requested = resolve_request_max_output_tokens(window, estimated_input, window);
    assert_eq!(requested, 29_586);
    // Provider-side counting differences of this size are covered.
    for provider_input in [
        estimated_input + 1,
        estimated_input + 512,
        estimated_input + 1_310,
    ] {
        assert!(
            provider_input + requested <= window,
            "provider input {provider_input} + requested {requested} exceeded {window}"
        );
    }
    // A prompt that already fills or exceeds the window reserves nothing and
    // cannot fabricate a negative cap.
    assert_eq!(resolve_request_max_output_tokens(window, window, window), 0);
    assert_eq!(
        resolve_request_max_output_tokens(window, window + 5_000, window),
        0
    );
    // Small windows keep a proportionate reserve rather than a fixed bite.
    assert_eq!(request_output_headroom(8_192), 256);
    assert_eq!(request_output_headroom(131_072), 1_310);
    assert_eq!(request_output_headroom(1_048_576), 4_096);
}

#[test]
fn provisional_delivery_rolls_back_when_generic_tool_output_limiting_truncates_it() {
    use std::sync::atomic::{AtomicI8, Ordering};

    let resolution = Arc::new(AtomicI8::new(0));
    let committed = Arc::clone(&resolution);
    let rolled_back = Arc::clone(&resolution);
    let result: Result<ToolOutput, ToolError> = Ok(ToolOutput::new("x".repeat(128))
        .with_delivery_commit(
            move || committed.store(1, Ordering::SeqCst),
            move || rolled_back.store(-1, Ordering::SeqCst),
        ));
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let (_, _, persisted_text, _, _) = lower_tool_result(
        octet_ai::ToolCallId("delivery".into()),
        &result,
        &model,
        32,
        Vec::new(),
    );
    assert_ne!(persisted_text, result.as_ref().unwrap().text);
    assert!(persisted_text.len() <= 32);

    resolve_tool_delivery_after_persistence(&result, 32);
    assert_eq!(resolution.load(Ordering::SeqCst), -1);
}

#[test]
fn repeated_tool_annotation_is_bounded_and_model_visible() {
    let result = annotate_repeated_tool_result(Ok(ToolOutput::new("result")), 2).unwrap();
    assert!(result.text.contains("exact call repeated 3x"));
    assert_eq!(
        result
            .content_parts()
            .iter()
            .filter_map(|part| match part {
                ToolOutputContentPart::Text(text) => Some(text.as_str()),
                ToolOutputContentPart::Media(_) => None,
            })
            .collect::<String>(),
        result.text
    );
    assert!(
        !annotate_repeated_tool_result(Ok(ToolOutput::new("result")), 1)
            .unwrap()
            .text
            .contains("diagnostic")
    );
}

#[test]
fn repeated_tool_annotation_preserves_machine_readable_output() {
    let original = r#"{"timed_out":false,"messages":[]}"#;
    let result = annotate_repeated_tool_result(Ok(ToolOutput::new(original)), 2).unwrap();
    assert_eq!(result.text, original);
}

#[test]
fn malformed_registered_arguments_create_a_secret_safe_policy_denial() {
    let workspace = tempfile::tempdir().unwrap();
    let sandbox = SandboxConfig::new(workspace.path());
    let broker = EffectBroker::new(crate::effect::EffectPolicy::Controlled);
    let sensitive_argument = "sensitive-argument-marker";
    let call = ToolCall {
        async_execution: false,
        id: octet_ai::ToolCallId("call_malformed".into()),
        name: "bash".into(),
        arguments_json: format!(r#"{{"command":"{sensitive_argument}""#),
        argument_error: None,
    };
    assert!(call.arguments_value().is_err());

    let (error, decision) = invalid_tool_arguments_denial(&sandbox, &broker);

    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::InvalidToolArguments)
    );
    assert_eq!(error.message, "invalid tool arguments");
    assert_eq!(decision.effect, None);
    assert!(!decision.allowed);
    assert_eq!(decision.authorization, None);
    assert_eq!(
        decision.denial_code,
        Some(ToolPolicyDenialCode::InvalidToolArguments)
    );
    let diagnostic = serde_json::to_string(&decision).unwrap();
    assert!(!diagnostic.contains(sensitive_argument));
    assert!(!error.message.contains(sensitive_argument));
}

#[test]
fn reservation_commit_rejection_keeps_final_policy_denied() {
    let workspace = tempfile::tempdir().unwrap();
    let sandbox = SandboxConfig::new(workspace.path());
    let broker = EffectBroker::new(crate::effect::EffectPolicy::Controlled);
    let (error, decision) = effect_reservation_commit_denial(
        &sandbox,
        &broker,
        ToolEffect::WorkspaceMutation,
        &crate::effect::EffectBrokerError::GrantRejected,
    );

    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::EffectReservationCommitDenied)
    );
    assert_eq!(error.message, "effect reservation could not be committed");
    assert_eq!(decision.effect, Some(ToolEffect::WorkspaceMutation));
    assert!(!decision.allowed);
    assert_eq!(decision.authorization, None);
    assert_eq!(
        decision.denial_code,
        Some(ToolPolicyDenialCode::EffectReservationCommitDenied)
    );
}

#[test]
fn execution_policy_denial_replaces_prior_admission() {
    let workspace = tempfile::tempdir().unwrap();
    let mut decision = Some(ToolPolicyDecision {
        effect: Some(ToolEffect::WorkspaceRead),
        allowed: true,
        authorization: Some(crate::effect::EffectAuthorization::Policy),
        denial_code: None,
        policy: SandboxConfig::new(workspace.path())
            .effective_tool_policy(crate::effect::EffectPolicy::Controlled),
    });
    let result: Result<ToolOutput, ToolError> = Err(ToolError::policy_denied(
        ToolPolicyDenialCode::WorkspaceConfinement,
        "path escapes the workspace",
    ));

    apply_execution_policy_denial(&mut decision, &result);

    let decision = decision.unwrap();
    assert!(!decision.allowed);
    assert_eq!(decision.authorization, None);
    assert_eq!(
        decision.denial_code,
        Some(ToolPolicyDenialCode::WorkspaceConfinement)
    );
}

#[test]
fn repeated_tool_annotation_preserves_policy_denial_code() {
    let error = annotate_repeated_tool_result(
        Err(ToolError::policy_denied(
            ToolPolicyDenialCode::WorkspaceConfinement,
            "path escapes the workspace",
        )),
        REPEATED_TOOL_CALL_THRESHOLD,
    )
    .unwrap_err();
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::WorkspaceConfinement)
    );
}

#[test]
fn response_header_failures_are_not_automatically_replayed() {
    for timeout in [false, true] {
        let error = AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::ResponseHeaders,
            timeout,
            message: "response headers unavailable".into(),
        });
        assert!(!retryable_before_generation(&error));
        assert!(!retryable_stream_start(&error));
        assert_eq!(provider_retry_limit(&error), 0);
    }
}

#[test]
fn body_timeout_is_not_automatically_retried() {
    let error = AiError::Transport(octet_ai::TransportError {
        phase: octet_ai::TransportPhase::Body,
        timeout: true,
        message: "stream idle deadline reached".into(),
    });
    assert!(!retryable_stream_start(&error));
    assert_eq!(provider_retry_limit(&error), 0);
}

#[test]
fn context_deadline_and_throttling_are_not_misclassified_as_overflow() {
    let deadline = AiError::Transport(octet_ai::TransportError {
        phase: octet_ai::TransportPhase::Body,
        timeout: true,
        message: "context deadline exceeded".into(),
    });
    assert!(!looks_like_context_error(&deadline));

    let throttled = AiError::Provider(octet_ai::ProviderError {
        code: Some("rate_limit_exceeded".into()),
        kind: Some("throttled".into()),
        message: "context window exceeded in shared capacity".into(),
        request_id: None,
    });
    assert!(!looks_like_context_error(&throttled));
}

/// A strict local/self-hosted server rejects an over-long request with a
/// plain 400. That is a request-size condition compaction can repair, not a
/// permanent policy/auth/quota rejection, so every shape it can take must
/// reach the compaction path.
#[test]
fn request_size_rejections_reach_the_compaction_path_in_every_server_shape() {
    const VLLM: &str = "This model's maximum context length is 131072 tokens. However, \
you requested 30896 output tokens and your prompt contains at least 100177 input tokens, \
for a total of at least 131073 tokens. Please reduce the length of the input prompt or the \
number of requested output tokens. (parameter=input_tokens, value=100177)";
    let shapes = [
        // Bare status with no machine-readable provider code.
        AiError::Http(octet_ai::HttpError {
            status: "400".parse().unwrap(),
            request_id: None,
            retry_after: None,
            provider_code: None,
            body_snippet: Some(format!(
                r#"{{"object":"error","message":"{VLLM}","type":"BadRequestError","code":400}}"#
            )),
            retryable: false,
        }),
        // Body carried a numeric machine-readable code.
        AiError::Http(octet_ai::HttpError {
            status: "400".parse().unwrap(),
            request_id: None,
            retry_after: None,
            provider_code: Some("400".into()),
            body_snippet: Some(VLLM.into()),
            retryable: false,
        }),
        // Canonical provider envelope: code 400, kind BadRequestError.
        AiError::Provider(octet_ai::ProviderError {
            code: Some("400".into()),
            kind: Some("BadRequestError".into()),
            message: VLLM.into(),
            request_id: None,
        }),
        // Some servers answer 413/422 for the same condition.
        AiError::Http(octet_ai::HttpError {
            status: "413".parse().unwrap(),
            request_id: None,
            retry_after: None,
            provider_code: Some("413".into()),
            body_snippet: Some(VLLM.into()),
            retryable: false,
        }),
    ];
    for (index, error) in shapes.into_iter().enumerate() {
        assert!(
            looks_like_context_error(&error),
            "shape {index} did not reach the compaction path: {error:?}"
        );
    }
    // A genuine policy/auth/quota/not-found rejection in the same envelope
    // still vetoes, because compaction cannot repair it.
    for (code, kind) in [
        (Some("invalid_prompt"), None),
        (Some("cyber_policy"), None),
        (Some("invalid_api_key"), None),
        (Some("insufficient_quota"), None),
        (Some("401"), None),
        (Some("403"), None),
        (Some("404"), None),
    ] {
        let error = AiError::Provider(octet_ai::ProviderError {
            code: code.map(str::to_owned),
            kind: kind.map(str::to_owned),
            message: VLLM.into(),
            request_id: None,
        });
        assert!(
            !looks_like_context_error(&error),
            "{code:?}/{kind:?} must stay vetoed"
        );
    }
    // 408/429 are excluded from the permanent set by design (they are
    // connectivity/rate conditions), so a bare numeric 429 carrying an
    // explicit context-overflow message still reaches the compaction path:
    // the server stated the request no longer fits.
    let throttled_with_context_text = AiError::Provider(octet_ai::ProviderError {
        code: Some("429".into()),
        kind: None,
        message: VLLM.into(),
        request_id: None,
    });
    assert!(looks_like_context_error(&throttled_with_context_text));
    // A named rate-limit rejection still never destroys context.
    let throttled = AiError::Provider(octet_ai::ProviderError {
        code: Some("rate_limit_exceeded".into()),
        kind: None,
        message: VLLM.into(),
        request_id: None,
    });
    assert!(!looks_like_context_error(&throttled));
}

#[test]
fn provider_validation_errors_do_not_retry_but_transient_failures_do() {
    let validation = AiError::Provider(octet_ai::ProviderError {
        code: Some("400".into()),
        kind: Some("Bad Request".into()),
        message: "reasoning_effort is invalid".into(),
        request_id: None,
    });
    assert!(!retryable_stream_start(&validation));

    for transient in [
        octet_ai::ProviderError {
            code: Some("503".into()),
            kind: Some("server_error".into()),
            message: "temporarily unavailable".into(),
            request_id: None,
        },
        octet_ai::ProviderError {
            code: Some("rate_limit_exceeded".into()),
            kind: Some("overloaded".into()),
            message: "try again".into(),
            request_id: None,
        },
    ] {
        assert!(retryable_stream_start(&AiError::Provider(transient)));
    }
}

#[test]
fn provider_retry_diagnostics_include_bounded_operational_details() {
    let model = tool_media_model(Protocol::OpenAiChat, octet_ai::ModalitySet::none());
    let errors = [
        AiError::Http(octet_ai::HttpError {
            status: http::StatusCode::TOO_MANY_REQUESTS,
            request_id: Some("req-429".into()),
            retry_after: Some(Duration::from_secs(3)),
            provider_code: Some("rate_limit_exceeded".into()),
            body_snippet: Some(r#"{"error":{"message":"temporarily rate limited"}}"#.into()),
            retryable: true,
        }),
        AiError::Provider(octet_ai::ProviderError {
            code: Some("upstream_error".into()),
            kind: Some("server_error".into()),
            message: "upstream temporarily unavailable".into(),
            request_id: Some("req-stream".into()),
        }),
        AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Connect,
            timeout: false,
            message: "connection reset by peer".into(),
        }),
    ];

    let retry = provider_retry_diagnostic(&model, &errors[0]);
    assert!(retry.contains("status=429 (rate limited)"), "{retry}");
    assert!(retry.contains("code=rate_limit_exceeded"), "{retry}");
    assert!(retry.contains("retry_after=3s"), "{retry}");
    assert!(retry.contains("request_id=req-429"), "{retry}");

    for error in &errors[1..] {
        let diagnostic = provider_retry_diagnostic(&model, error);
        assert!(diagnostic.contains("provider="), "{diagnostic}");
        assert!(diagnostic.contains("model="), "{diagnostic}");
        assert!(diagnostic.contains("phase="), "{diagnostic}");
    }

    let credential_error = AiError::Http(octet_ai::HttpError {
        status: http::StatusCode::UNAUTHORIZED,
        request_id: Some("req-auth".into()),
        retry_after: None,
        provider_code: Some("invalid_api_key".into()),
        body_snippet: Some(r#"{"error":{"message":"invalid api key: sk-secret"}}"#.into()),
        retryable: false,
    });
    let diagnostic = provider_retry_diagnostic(&model, &credential_error);
    assert!(diagnostic.contains("status=401 (authentication failed)"));
    assert!(!diagnostic.contains("sk-secret"));
}

#[test]
fn provider_context_limit_variants_are_classified_as_overflow() {
    for message in [
        "model_context_window_exceeded",
        "prompt is too long",
        "request_too_large",
        "context window exceeds limit",
    ] {
        let error = AiError::Provider(octet_ai::ProviderError {
            code: None,
            kind: None,
            message: message.into(),
            request_id: None,
        });
        assert!(looks_like_context_error(&error), "{message}");
    }

    let request_too_large = AiError::Http(octet_ai::HttpError {
        status: "413".parse().unwrap(),
        request_id: None,
        retry_after: None,
        provider_code: Some("request_too_large".into()),
        body_snippet: Some("request exceeds the context window".into()),
        retryable: false,
    });
    assert!(looks_like_context_error(&request_too_large));

    let media_too_large = AiError::Http(octet_ai::HttpError {
        status: "413".parse().unwrap(),
        request_id: None,
        retry_after: None,
        provider_code: Some("image_too_large".into()),
        body_snippet: Some("uploaded image payload exceeds 20 MB".into()),
        retryable: false,
    });
    assert!(!looks_like_context_error(&media_too_large));
}

#[test]
fn non_timeout_network_failure_gets_five_retries_and_friendly_failure() {
    let error = AiError::Transport(octet_ai::TransportError {
        phase: octet_ai::TransportPhase::Connect,
        timeout: false,
        message: "connection refused".into(),
    });
    assert!(retryable_before_generation(&error));
    assert!(retryable_stream_start(&error));
    assert_eq!(provider_retry_limit(&error), 5);

    let failure = provider_failure(error, 5).to_string();
    assert!(failure.contains("Are you connected to the internet?"));
    assert!(failure.contains("connection"));
    assert!(!failure.contains("connection refused"));
}

#[test]
fn public_provider_failures_include_safe_operational_details() {
    let errors = [
            (
                AgentError::Ai(AiError::Http(octet_ai::HttpError {
                    status: http::StatusCode::BAD_REQUEST,
                    request_id: Some("req-400".into()),
                    retry_after: None,
                    provider_code: Some("invalid_request".into()),
                    body_snippet: Some(
                        r#"{"error":{"message":"model does not support this request"}}"#.into(),
                    ),
                    retryable: false,
                })),
                "status=400 (bad request) code=invalid_request detail=model does not support this request request_id=req-400",
            ),
            (
                AgentError::Ai(AiError::Provider(octet_ai::ProviderError {
                    code: Some("upstream_error".into()),
                    kind: Some("server_error".into()),
                    message: "upstream temporarily unavailable".into(),
                    request_id: Some("req-stream".into()),
                })),
                "phase=response body (provider error) code=upstream_error kind=server_error detail=upstream temporarily unavailable request_id=req-stream",
            ),
            (
                AgentError::Ai(AiError::Transport(octet_ai::TransportError {
                    phase: octet_ai::TransportPhase::Body,
                    timeout: true,
                    message: "stream idle beyond its timeout".into(),
                })),
                "phase=response body timeout hint=Provider acceptance and failed-attempt usage are uncertain. Inspect provider state before retrying explicitly. detail=stream idle beyond its timeout",
            ),
            (
                AgentError::IncompleteResponse {
                    stop_reason: "refusal".to_owned(),
                },
                "phase=response completion reason=refusal",
            ),
        ];

    for (error, suffix) in errors {
        let diagnostic = public_error_diagnostic(&error, "openai", "gpt-test");
        assert!(diagnostic.ends_with(suffix), "{diagnostic}");
        assert!(diagnostic.starts_with("provider=openai model=gpt-test "));
    }

    let error = AgentError::Ai(AiError::Http(octet_ai::HttpError {
        status: http::StatusCode::UNAUTHORIZED,
        request_id: Some("req-auth".into()),
        retry_after: None,
        provider_code: Some("invalid_api_key".into()),
        body_snippet: Some(r#"{"error":{"message":"invalid api key: sk-secret"}}"#.into()),
        retryable: false,
    }));
    let diagnostic = public_error_diagnostic(&error, "openrouter", "openrouter/test");
    assert!(diagnostic.contains("status=401 (authentication failed)"));
    assert!(diagnostic.contains("code=invalid_api_key"));
    assert!(diagnostic.contains("request_id=req-auth"));
    assert!(!diagnostic.contains("sk-secret"));

    assert_eq!(
        public_error_diagnostic(&AgentError::RunEnded, "openai", "gpt-test"),
        "the run has already finished"
    );
}

#[test]
fn connect_timeout_is_not_automatically_retried() {
    let error = AiError::Transport(octet_ai::TransportError {
        phase: octet_ai::TransportPhase::Connect,
        timeout: true,
        message: "connection timed out".into(),
    });
    assert!(!retryable_before_generation(&error));
    assert!(!retryable_stream_start(&error));
    assert_eq!(provider_retry_limit(&error), 0);
}

#[test]
fn stream_failure_delegates_classification_to_inner_failure() {
    let progress = octet_ai::StreamProgress {
        provider_events: 412,
        decoded_events: 38,
        content_bytes: 18_204,
        buffered_bytes: 96,
        first_body_seen: true,
        elapsed_ms: 97_321,
        last_event_ms: Some(97_000),
    };

    // Body disconnects remain ambiguous, even before visible generation.
    let disconnect = AiError::StreamFailure {
        inner: Box::new(AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: false,
            message: "connection reset by peer".into(),
        })),
        progress,
    };
    assert_eq!(ai_error_phase(&disconnect), "response body");
    assert!(!retryable_before_generation(&disconnect));
    assert!(!retryable_stream_start(&disconnect));
    assert!(!is_replayable_network_failure(&disconnect));
    assert!(!looks_like_context_error(&disconnect));
    assert_eq!(provider_retry_limit(&disconnect), 0);

    // A stream that ended on a provider 503 frame keeps that frame's
    // retry budget instead of being demoted to the wrapper's behavior.
    let server_error = AiError::StreamFailure {
        inner: Box::new(AiError::Provider(octet_ai::ProviderError {
            code: Some("503".into()),
            kind: Some("server_error".into()),
            message: "temporarily unavailable".into(),
            request_id: None,
        })),
        progress,
    };
    assert_eq!(
        ai_error_phase(&server_error),
        "response body (provider error)"
    );
    assert!(retryable_stream_start(&server_error));
    assert!(!is_replayable_network_failure(&server_error));
    assert_eq!(provider_retry_limit(&server_error), MAX_PROVIDER_RETRIES);

    // A transport timeout with a context-flavoured message must still
    // never be classified as context overflow, wrapped or bare.
    let deadline = AiError::StreamFailure {
        inner: Box::new(AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: true,
            message: "context deadline exceeded".into(),
        })),
        progress,
    };
    assert!(!looks_like_context_error(&deadline));
    assert!(!retryable_stream_start(&deadline));
    assert_eq!(provider_retry_limit(&deadline), 0);

    // A post-send heartbeat deadline has ambiguous provider acceptance, so
    // it is terminal even if no generation was decoded.
    let heartbeat = AiError::StreamFailure {
        inner: Box::new(AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: true,
            message: "Responses WebSocket heartbeat acknowledgement timed out".into(),
        })),
        progress,
    };
    assert!(!retryable_before_generation(&heartbeat));
    assert!(!retryable_stream_start(&heartbeat));
    assert!(!is_replayable_network_failure(&heartbeat));
    assert_eq!(provider_retry_limit(&heartbeat), 0);

    // And a provider context-error frame inside a 2xx stream must still
    // be detected through the wrapper, so compaction still triggers.
    let overflow = AiError::StreamFailure {
        inner: Box::new(AiError::Provider(octet_ai::ProviderError {
            code: None,
            kind: None,
            message: "prompt is too long".into(),
            request_id: None,
        })),
        progress,
    };
    assert!(looks_like_context_error(&overflow));

    // Even the unit variant keeps its exact phase label through the
    // wrapper.
    let canceled = AiError::StreamFailure {
        inner: Box::new(AiError::Canceled),
        progress,
    };
    assert_eq!(ai_error_phase(&canceled), "request cancellation");
}

#[test]
fn websocket_connection_limit_is_retried_before_generation() {
    let error = octet_ai::ProviderError {
        code: Some("websocket_connection_limit_reached".into()),
        kind: None,
        message: "create a new websocket connection".into(),
        request_id: None,
    };
    assert!(provider_requests_connection_refresh(&error));
    assert!(retryable_stream_start(&AiError::Provider(error)));
    assert_eq!(
        provider_retry_limit(&AiError::Provider(octet_ai::ProviderError {
            code: Some("websocket_connection_limit_reached".into()),
            kind: None,
            message: "create a new websocket connection".into(),
            request_id: None,
        })),
        MAX_PROVIDER_RETRIES
    );
}

#[test]
fn stream_failure_diagnostic_appends_wire_progress_inside_the_public_bound() {
    let progress = octet_ai::StreamProgress {
        provider_events: 412,
        decoded_events: 38,
        content_bytes: 18_204,
        buffered_bytes: 96,
        first_body_seen: true,
        elapsed_ms: 97_321,
        last_event_ms: Some(97_000),
    };
    let suffix = "stream_progress=frames=412 events=38 content=18204B buffered=96B first_byte=seen elapsed=97321ms last_event=97000ms";

    let inner = AiError::Provider(octet_ai::ProviderError {
        // Four oversized fields push the bare diagnostic past the public
        // bound, so the wrapper must reserve room for its progress field
        // instead of letting truncation clip the progress off the end.
        code: Some("x".repeat(600)),
        kind: Some("y".repeat(600)),
        message: "z".repeat(600),
        request_id: Some("w".repeat(600)),
    });
    let bare = public_ai_error_diagnostic(&inner, "openai", "gpt-test");
    assert_eq!(
        bare.len(),
        MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES,
        "fixture must overflow the public bound"
    );
    assert!(bare.ends_with('…'));

    let wrapped = public_ai_error_diagnostic(
        &AiError::StreamFailure {
            inner: Box::new(inner),
            progress,
        },
        "openai",
        "gpt-test",
    );
    assert!(
        wrapped.len() <= MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES,
        "wrapped diagnostic must stay inside the public bound: {}",
        wrapped.len()
    );
    assert!(
        wrapped.ends_with(suffix),
        "truncation must not clip the progress field: {wrapped}"
    );
    assert!(wrapped.contains("phase=response body (provider error)"));
}

#[test]
fn bare_ai_error_variants_surface_bounded_detail() {
    let errors = [
        AiError::Config(octet_ai::ConfigError::Parse(
            "malformed endpoint file".into(),
        )),
        AiError::Auth(octet_ai::AuthError::Resolve),
        AiError::Validation(octet_ai::ValidationError::OrphanToolResult(
            octet_ai::ToolCallId("call_orphan".into()),
        )),
        AiError::Unsupported(octet_ai::UnsupportedError::Image),
        AiError::Decode(octet_ai::DecodeError::Json("unterminated string".into())),
        AiError::Pricing(octet_ai::PricingError::ArithmeticOverflow),
        AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish),
    ];
    for error in &errors {
        let diagnostic = public_ai_error_diagnostic(error, "openai", "gpt-test");
        assert!(diagnostic.contains("detail="), "{diagnostic}");
        assert!(
            diagnostic.starts_with("provider=openai model=gpt-test phase="),
            "{diagnostic}"
        );
    }
    let config = public_ai_error_diagnostic(
        &AiError::Config(octet_ai::ConfigError::Parse(
            "malformed endpoint file".into(),
        )),
        "openai",
        "gpt-test",
    );
    assert_eq!(
            config,
            "provider=openai model=gpt-test phase=request preparation detail=Parse error: malformed endpoint file"
        );
    let canceled = public_ai_error_diagnostic(&AiError::Canceled, "openai", "gpt-test");
    assert_eq!(
        canceled,
        "provider=openai model=gpt-test phase=request cancellation"
    );
}

#[test]
fn request_estimator_counts_inline_media_semantically_not_as_base64_text() {
    let image = Media::image_bytes(
        bytes::Bytes::from(vec![7u8; 1024 * 1024]),
        "image/png".parse().unwrap(),
    );
    let messages = vec![Message::User(UserMessage {
        content: vec![UserPart::Media(image)],
    })];

    let estimate = estimate_request_tokens("system", &messages, &[]);
    assert!(estimate >= ESTIMATED_IMAGE_TOKENS, "{estimate}");
    assert!(
        estimate < 10_000,
        "inline image bytes were miscounted as text tokens: {estimate}"
    );
}

#[test]
fn responses_capacity_estimates_only_new_opaque_items_and_rebuilds_on_compaction() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("responses-capacity.jsonl")).unwrap();
    let model = tool_media_model(Protocol::OpenAiResponses, octet_ai::ModalitySet::none());
    session.append(user_message("prefix".into())).unwrap();
    let baseline = context_breakdown(&session, &model, "system", &session.context().unwrap(), &[]);
    let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);
    cache.estimate(&session, &model, "system", &[], 1).unwrap();
    for turn in 0..32 {
        session.append(user_message("request".into())).unwrap();
        let output =
            octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({
                "type": "message", "id": format!("message-{turn}"),
                "role": "assistant", "content": [{"type": "output_text", "text": "answer"}],
                "opaque_future_field": "large payload".repeat(1024)
            }))
            .unwrap()]);
        session
            .append_assistant_turn(
                AssistantMessage {
                    content: vec![AssistantPart::Text("answer".into())],
                    model: model.spec.id.clone(),
                    protocol: Protocol::OpenAiResponses,
                },
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage::default(),
                None,
                StopReason::EndTurn,
                Some(output),
            )
            .unwrap();
        let incremental = cache.estimate(&session, &model, "system", &[], 1).unwrap();
        let full = reconcile_context_estimate(
            &session,
            &model,
            "system",
            &session.context().unwrap(),
            &[],
        );
        assert!(incremental.input_tokens >= full.input_tokens);
        assert_eq!(cache.full_rebuilds(), 1);
    }
    let kept = session.append(user_message("kept".into())).unwrap();
    session.compact("summary", kept).unwrap();
    let incremental = cache.estimate(&session, &model, "system", &[], 1).unwrap();
    let full =
        reconcile_context_estimate(&session, &model, "system", &session.context().unwrap(), &[]);
    assert_eq!(incremental, full);
    assert_eq!(cache.full_rebuilds(), 2);
}

#[test]
fn canonical_capacity_advances_new_messages_without_rebuilding_history() {
    use octet_ai::{ModelCatalog, ModelId};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("capacity.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let system = "system";
    session
        .append(user_message(UserInput::from("first message")))
        .unwrap();
    let messages = session.context().unwrap();
    let baseline = context_breakdown(&session, &model, system, &messages, &[]);
    let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);

    session
        .append(user_message(UserInput::from("second message")))
        .unwrap();
    let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
    let full_messages = session.context().unwrap();
    let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

    assert!(
        incremental.input_tokens >= full.input_tokens,
        "incremental capacity undercounted the request: incremental={incremental:?} full={full:?}"
    );
    assert_eq!(cache.full_rebuilds(), 0);
}

#[test]
fn canonical_capacity_overbounds_coalesced_tool_results_without_a_full_scan() {
    use octet_ai::{ModelCatalog, ModelId, ToolResult, ToolResultPart};

    fn tool_result(id: &str, text: &str) -> EntryValue {
        EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId(id.into()),
                content: vec![ToolResultPart::Text(text.into())],
                is_error: false,
                added_tool_names: None,
            })],
        }))
    }

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("coalesced-capacity.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let system = "system";
    session.append(tool_result("one", "first result")).unwrap();
    let messages = session.context().unwrap();
    let baseline = context_breakdown(&session, &model, system, &messages, &[]);
    let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);

    session.append(tool_result("two", "second result")).unwrap();
    let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
    let full_messages = session.context().unwrap();
    let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

    assert_eq!(
        full_messages.len(),
        1,
        "tool results should coalesce in context"
    );
    assert!(
        incremental.input_tokens >= full.input_tokens,
        "coalesced tool result undercounted the request: incremental={incremental:?} full={full:?}"
    );
    assert_eq!(cache.full_rebuilds(), 0);
}

#[test]
fn canonical_capacity_reanchors_to_authoritative_provider_usage() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelCatalog, ModelId};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("provider-capacity.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let system = "system";
    session
        .append(user_message(UserInput::from("prompt")))
        .unwrap();
    let messages = session.context().unwrap();
    let baseline = context_breakdown(&session, &model, system, &messages, &[]);
    let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);
    let usage = Usage {
        input_tokens: 90_000,
        output_tokens: 10_000,
        total_tokens: 100_000,
        ..Usage::default()
    };

    session
        .append_assistant_turn(
            AssistantMessage {
                content: vec![AssistantPart::Text("answer".into())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            },
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            usage,
            None,
            StopReason::EndTurn,
            None,
        )
        .unwrap();
    cache.observe_assistant_response(&session, &model, &usage);
    let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
    let full_messages = session.context().unwrap();
    let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

    assert_eq!(incremental.provider_tokens, Some(100_000));
    assert_eq!(incremental.provider_tokens, full.provider_tokens);
    assert!(incremental.input_tokens >= full.input_tokens);
    assert_eq!(cache.full_rebuilds(), 0);
}

#[test]
fn canonical_capacity_rebuilds_after_a_local_compaction_boundary() {
    use octet_ai::{ModelCatalog, ModelId};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("compaction-capacity.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let system = "system";
    session
        .append(user_message(UserInput::from("old message")))
        .unwrap();
    let first_kept = session
        .append(user_message(UserInput::from("kept message")))
        .unwrap();
    let messages = session.context().unwrap();
    let baseline = context_breakdown(&session, &model, system, &messages, &[]);
    let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);

    session.compact("summary", first_kept).unwrap();
    let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
    let full_messages = session.context().unwrap();
    let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

    assert_eq!(incremental, full);
    assert_eq!(cache.full_rebuilds(), 1);
}

fn tool_media_model(protocol: Protocol, modalities: octet_ai::ModalitySet) -> Model {
    let base = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let mut spec = (*base.spec).clone();
    spec.protocol = protocol;
    spec.capabilities.input_modalities = modalities;
    Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint,
    }
}

#[test]
fn owner_images_come_only_from_accepted_durable_protocol_parts() {
    let raw = Ok(ToolOutput::new("image").with_media(Media::image_bytes(
        bytes::Bytes::from_static(b"payload-sentinel"),
        "image/png".parse().unwrap(),
    )));
    for protocol in [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    ] {
        for supported in [false, true] {
            let modalities = if supported {
                octet_ai::ModalitySet::none().with(octet_ai::Modality::Image)
            } else {
                octet_ai::ModalitySet::none()
            };
            let (message, _, _, _, _) = lower_tool_result(
                octet_ai::ToolCallId("call".into()),
                &raw,
                &tool_media_model(protocol, modalities),
                4096,
                Vec::new(),
            );
            let owner = ToolOutput::new("")
                .with_owner_presentation_images(lowered_tool_result_media(&message));
            assert_eq!(owner.media().len(), usize::from(supported));
            assert!(!owner.presentation_images_omitted());
            assert!(!format!("{owner:?}").contains("payload-sentinel"));
        }
    }
}

#[test]
fn ambiguous_stream_endings_have_no_retry_budget_and_actionable_hints() {
    for error in [
        AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish),
        AiError::StreamProtocol(octet_ai::StreamProtocolError::PrematureEof),
        AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: false,
            message: "connection reset".into(),
        }),
    ] {
        assert!(!retryable_before_generation(&error));
        assert!(!retryable_stream_start(&error));
        assert!(!is_replayable_network_failure(&error));
        assert_eq!(provider_retry_limit(&error), 0);
        let diagnostic = public_ai_error_diagnostic(&error, "test", "test");
        assert!(
            diagnostic.contains("Inspect provider state before retrying explicitly"),
            "{diagnostic}"
        );
    }
}

#[test]
fn anthropic_tool_image_stays_inside_the_paired_result() {
    let model = tool_media_model(
        Protocol::AnthropicMessages,
        octet_ai::ModalitySet::none().with(octet_ai::Modality::Image),
    );
    let result = Ok(ToolOutput::new("read=image").with_media(Media::image_bytes(
        bytes::Bytes::from_static(b"png"),
        "image/png".parse().unwrap(),
    )));
    let (message, accepted, _, is_error, _) = lower_tool_result(
        octet_ai::ToolCallId("call".into()),
        &result,
        &model,
        4096,
        Vec::new(),
    );
    assert_eq!(accepted, vec![ToolOutputMediaKind::Image]);
    assert!(!is_error);
    assert_eq!(message.content.len(), 1);
    let UserPart::ToolResult(result) = &message.content[0] else {
        panic!("tool result must remain first");
    };
    assert!(matches!(
        result.content.get(1),
        Some(ToolResultPart::Media(Media::Image(_)))
    ));
}

#[test]
fn ordered_tool_parts_keep_text_image_text_order_under_one_text_budget() {
    let text_limit = TOOL_TRUNCATION_MARKER.len() + 6;
    for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        let model = tool_media_model(
            protocol,
            octet_ai::ModalitySet::none().with(octet_ai::Modality::Image),
        );
        let result = Ok(ToolOutput::from_content_parts([
            ToolOutputContentPart::Text("ABCDEFGHIJKLMNOPQRSTUVWXYZ".into()),
            ToolOutputContentPart::Media(Media::image_bytes(
                bytes::Bytes::from_static(b"png"),
                "image/png".parse().unwrap(),
            )),
            ToolOutputContentPart::Text("abcdefghijklmnopqrstuvwxyz".into()),
        ]));

        let (message, accepted, persisted_text, is_error, _) = lower_tool_result(
            octet_ai::ToolCallId("call".into()),
            &result,
            &model,
            text_limit,
            Vec::new(),
        );

        assert_eq!(accepted, vec![ToolOutputMediaKind::Image]);
        assert!(!is_error);
        assert!(persisted_text.len() <= text_limit);
        let UserPart::ToolResult(result) = &message.content[0] else {
            panic!("expected canonical tool result");
        };
        assert_eq!(result.content.len(), 3);
        assert!(matches!(
            &result.content[0],
            ToolResultPart::Text(text)
                if text == &format!("ABC{TOOL_TRUNCATION_MARKER}")
        ));
        assert!(matches!(
            result.content[1],
            ToolResultPart::Media(Media::Image(_))
        ));
        assert!(matches!(
            &result.content[2],
            ToolResultPart::Text(text) if text == "xyz"
        ));
        let provider_text_bytes = result
            .content
            .iter()
            .filter_map(|part| match part {
                ToolResultPart::Text(text) => Some(text.len()),
                ToolResultPart::Media(_) => None,
            })
            .sum::<usize>();
        assert_eq!(provider_text_bytes, text_limit);
    }
}

#[test]
fn lowering_keeps_structured_details_outside_provider_visible_content() {
    let model = tool_media_model(Protocol::OpenAiResponses, octet_ai::ModalitySet::none());
    let result = Ok(ToolOutput::new("Found one source.")
        .try_with_details(
            Some(serde_json::json!({"sources": [{"title": "Primary"}]})),
            Some(serde_json::json!({"cache": "miss"})),
        )
        .unwrap());
    let (message, _, _, is_error, details) = lower_tool_result(
        octet_ai::ToolCallId("call".into()),
        &result,
        &model,
        4096,
        Vec::new(),
    );

    assert!(!is_error);
    let details = details.expect("durable details");
    assert_eq!(
        details.structured_content(),
        Some(&serde_json::json!({"sources": [{"title": "Primary"}]}))
    );
    assert_eq!(
        details.metadata(),
        Some(&serde_json::json!({"cache": "miss"}))
    );
    let UserPart::ToolResult(provider_result) = &message.content[0] else {
        panic!("expected canonical tool result");
    };
    assert_eq!(provider_result.content.len(), 1);
    assert!(matches!(
        provider_result.content[0],
        ToolResultPart::Text(ref text) if text == "Found one source."
    ));
}

#[test]
fn openai_chat_wav_and_mp3_follow_the_paired_tool_result() {
    let model = tool_media_model(
        Protocol::OpenAiChat,
        octet_ai::ModalitySet::none().with(octet_ai::Modality::Audio),
    );
    for format in [octet_ai::AudioFormat::Wav, octet_ai::AudioFormat::Mp3] {
        let result = Ok(ToolOutput::new("read=audio").with_media(Media::audio_bytes(
            bytes::Bytes::from_static(b"audio"),
            format,
        )));
        let (message, accepted, _, is_error, _) = lower_tool_result(
            octet_ai::ToolCallId("call".into()),
            &result,
            &model,
            4096,
            Vec::new(),
        );
        assert_eq!(accepted, vec![ToolOutputMediaKind::Audio]);
        assert!(!is_error);
        assert!(matches!(message.content[0], UserPart::ToolResult(_)));
        assert!(matches!(
            message.content[1],
            UserPart::Media(Media::Audio(_))
        ));
    }
}

#[test]
fn unsupported_tool_audio_is_an_error_without_media_or_indicator() {
    let responses = tool_media_model(
        Protocol::OpenAiResponses,
        octet_ai::ModalitySet::none().with(octet_ai::Modality::Audio),
    );
    let audio = Ok(ToolOutput::new("read=audio").with_media(Media::audio_bytes(
        bytes::Bytes::from_static(b"audio"),
        octet_ai::AudioFormat::Wav,
    )));
    let (message, accepted, text, is_error, _) = lower_tool_result(
        octet_ai::ToolCallId("call".into()),
        &audio,
        &responses,
        4096,
        Vec::new(),
    );
    assert!(accepted.is_empty());
    assert!(is_error);
    assert!(text.contains("protocol cannot replay audio"));
    assert_eq!(message.content.len(), 1);

    let chat = tool_media_model(
        Protocol::OpenAiChat,
        octet_ai::ModalitySet::none().with(octet_ai::Modality::Audio),
    );
    let aac = Ok(ToolOutput::new("read=audio").with_media(Media::audio_bytes(
        bytes::Bytes::from_static(b"audio"),
        octet_ai::AudioFormat::Aac,
    )));
    let (message, accepted, text, is_error, _) = lower_tool_result(
        octet_ai::ToolCallId("call".into()),
        &aac,
        &chat,
        4096,
        Vec::new(),
    );
    assert!(accepted.is_empty());
    assert!(is_error);
    assert!(text.contains("accepts WAV or MP3"));
    assert_eq!(message.content.len(), 1);
}

#[test]
fn a_requested_service_tier_is_gated_by_the_route_and_never_silently_dropped() {
    use octet_ai::{ModelCatalog, ModelId, ResponsesRuntimeProfile, ServiceTier};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("service-tier.jsonl")).unwrap();
    let mut model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();

    // A route that does not declare the field refuses the selection with the
    // codec's typed unsupported error instead of dropping it silently.
    let rejection = resolve_service_tier(&model, Some(ServiceTier::Priority)).unwrap_err();
    assert_eq!(
        rejection.to_string(),
        "ai error: Unsupported error: Responses service tier is unsupported on this route"
    );
    // The same route may always clear the selection.
    assert_eq!(resolve_service_tier(&model, None).unwrap(), None);

    // The declared Codex runtime accepts it.
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile = ResponsesRuntimeProfile::Codex;
    assert_eq!(
        resolve_service_tier(&model, Some(ServiceTier::Priority)).unwrap(),
        Some(ServiceTier::Priority)
    );

    // A non-Responses protocol could not emit the field at all, so a declared
    // profile bit must not be enough.
    let mut chat = model.clone();
    Arc::make_mut(&mut chat.spec).protocol = Protocol::OpenAiChat;
    assert!(resolve_service_tier(&chat, Some(ServiceTier::Priority)).is_err());

    // The historical no-tier path is untouched: a session whose assistant
    // turn has no route-affine sidecar still builds no Responses options.
    session
        .append(user_message(UserInput::from("legacy prompt")))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("legacy answer".into())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    assert!(
        durable_responses_options(&session, &model, "system", None)
            .unwrap()
            .is_none(),
        "no tier and no replay window keeps the canonical no-options request"
    );

    // A requested tier still rides on the request when there is no replay
    // window: the codec then replays canonically exactly as it would with no
    // options, so `/fast` cannot be silently inert.
    let options = durable_responses_options(&session, &model, "system", Some(ServiceTier::Flex))
        .unwrap()
        .expect("a requested tier always produces options");
    // A baseline pin is request metadata, not an ordered input update.
    // Legacy sessions without opaque sidecars must keep canonical replay.
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .responses_features
        .reasoning_effort_updates = true;
    Arc::make_mut(&mut model.spec)
        .capabilities
        .responses_features
        .reasoning_effort_updates = true;
    let baseline = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium);
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: model.endpoint.id.clone(),
            model: model.spec.id.clone(),
            baseline: baseline.clone(),
            update: None,
        })
        .unwrap();
    assert!(
        durable_responses_options(&session, &model, "system", None)
            .unwrap()
            .is_none(),
        "baseline-only reasoning history remains canonically replayable"
    );
    session
        .append(EntryValue::ResponsesReasoning {
            endpoint: model.endpoint.id.clone(),
            model: model.spec.id.clone(),
            baseline: baseline.clone(),
            update: Some(octet_ai::ResponsesConfigurationUpdate {
                reasoning: ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
            }),
        })
        .unwrap();
    assert!(durable_responses_options(&session, &model, "system", None)
        .unwrap()
        .is_none());
    let effective = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
    assert_eq!(
        request_reasoning_for_replay(&session, &model, None, &baseline).unwrap(),
        effective
    );

    assert_eq!(options.service_tier, Some(ServiceTier::Flex));
    assert_eq!(
        request_reasoning_for_replay(&session, &model, Some(&options), &baseline).unwrap(),
        effective
    );
    assert!(options.input.is_none());
    assert_eq!(options.previous_response_id, None);
    assert!(!options.store);
    assert_eq!(options.context_management, None);
}

#[cfg(any(unix, windows))]
#[derive(Default)]
struct RecordingCheckpointSink {
    snapshots: Mutex<Vec<String>>,
    refuse: bool,
}

#[cfg(any(unix, windows))]
impl RecordingCheckpointSink {
    fn snapshots(&self) -> Vec<String> {
        self.snapshots.lock().unwrap().clone()
    }
}

#[cfg(any(unix, windows))]
impl PartialOutputCheckpointSink for RecordingCheckpointSink {
    fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
        if self.refuse {
            return Err(ToolError::new("storage fault"));
        }
        self.snapshots.lock().unwrap().push(snapshot.to_owned());
        Ok(())
    }
}

#[cfg(any(unix, windows))]
fn checkpoint_config(
    sink: Arc<RecordingCheckpointSink>,
) -> (
    PartialOutputCheckpointConfig,
    Arc<PartialOutputCheckpointTotals>,
) {
    let totals = Arc::new(PartialOutputCheckpointTotals::default());
    (
        PartialOutputCheckpointConfig {
            tool: "bash".to_owned(),
            sink: Some(sink),
            interval: BASH_CHECKPOINT_INTERVAL,
            totals: Arc::clone(&totals),
        },
        totals,
    )
}

#[cfg(any(unix, windows))]
#[test]
fn partial_output_checkpoints_pace_bound_and_never_claim_completion() {
    let sink = Arc::new(RecordingCheckpointSink::default());
    let (config, totals) = checkpoint_config(Arc::clone(&sink));
    let start = std::time::Instant::now();
    let mut live = LivePartialOutput::for_call(&config, "bash").expect("the opt-in names bash");

    // The opt-in is per tool: another tool's calls are not published.
    assert!(LivePartialOutput::for_call(&config, "search").is_none());

    // First observation publishes immediately.
    let first = live
        .observe_output(OutputStream::Stdout, b"alpha\n", start)
        .expect("first observation publishes");
    assert!(first.contains("stdout: 6 bytes seen"), "{first}");
    assert!(first.contains("alpha"), "{first}");

    // Before the interval: paced away, counted, never published.
    assert!(live
        .observe_output(
            OutputStream::Stdout,
            b"beta\n",
            start + Duration::from_millis(1)
        )
        .is_none());
    assert_eq!(sink.snapshots().len(), 1, "one publication so far");
    assert_eq!(totals.stats().paced, 1);

    // Past the interval a changed snapshot publishes again, and an unchanged
    // one is duplicate-suppressed instead of re-published.
    let second = live
        .observe_output(
            OutputStream::Stdout,
            b"gamma\n",
            start + BASH_CHECKPOINT_INTERVAL,
        )
        .expect("interval elapsed with new output");
    assert!(second.contains("alpha\nbeta\ngamma"), "{second}");
    assert!(live
        .observe_output(
            OutputStream::Stderr,
            b"",
            start + BASH_CHECKPOINT_INTERVAL * 2
        )
        .is_none());
    assert_eq!(sink.snapshots().len(), 2);

    // Non-output progress is not checkpointed at all.
    live.observe_progress(
        &ToolProgress::Status("still running".into()),
        start + BASH_CHECKPOINT_INTERVAL * 3,
    );
    assert_eq!(sink.snapshots().len(), 2);

    // A large burst keeps the newest bytes under the row's 50 KiB bound, keeps
    // its header, and stays a checkpoint: no publication may claim the
    // command finished.
    let burst_bytes = 4 * BASH_CHECKPOINT_MAX_BYTES;
    let mut burst = vec![b'x'; burst_bytes];
    burst.extend_from_slice(b"NEWEST-MARKER");
    let bounded = live
        .observe_output(
            OutputStream::Stdout,
            &burst,
            start + BASH_CHECKPOINT_INTERVAL * 4,
        )
        .expect("a changed snapshot publishes");
    assert!(
        bounded.len() <= BASH_CHECKPOINT_MAX_BYTES,
        "snapshot is bounded: {} bytes",
        bounded.len()
    );
    assert!(
        bounded.contains(&format!(
            "stdout: {} bytes seen (earlier bytes elided)",
            6 + 5 + 6 + burst_bytes as u64 + 13
        )),
        "the header survives bounding: {}",
        &bounded[..bounded.len().min(200)]
    );
    // The newest bytes are what a recovery consumer needs, and the oldest are
    // what got elided: the stdout section keeps the burst's tail. The render
    // carries stdout first and the stderr section last, so the marker sits at
    // the end of its own section rather than at the end of the snapshot.
    let stdout_section = bounded
        .split_once("\nstderr: ")
        .map(|(stdout, _)| stdout)
        .expect("the render always carries both stream sections");
    assert!(
        stdout_section.ends_with("NEWEST-MARKER"),
        "the newest bytes are the ones kept: {}",
        &stdout_section[stdout_section.len().saturating_sub(120)..]
    );
    assert!(
        stdout_section.len() <= PARTIAL_STREAM_CAP + PARTIAL_HEADER_RESERVE,
        "a retained stream section stays inside its half of the cap: {} bytes",
        stdout_section.len()
    );
    let stats = totals.stats();
    assert_eq!(stats.published, 3);
    assert!(stats.failures == 0);
    for snapshot in sink.snapshots() {
        assert!(
            !snapshot.contains("complete_stdout=true")
                && !snapshot.contains("complete_stderr=true"),
            "a checkpoint never claims completion: {snapshot}"
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn a_refused_checkpoint_is_counted_and_never_becomes_a_tool_result() {
    let sink = Arc::new(RecordingCheckpointSink {
        snapshots: Mutex::new(Vec::new()),
        refuse: true,
    });
    let (config, totals) = checkpoint_config(Arc::clone(&sink));
    let start = std::time::Instant::now();
    let mut live = LivePartialOutput::for_call(&config, "bash").unwrap();

    assert!(
        live.observe_output(OutputStream::Stdout, b"alpha\n", start)
            .is_none(),
        "a storage fault is not a publication"
    );
    assert_eq!(totals.stats().failures, 1);
    assert_eq!(totals.stats().published, 0);
    assert!(sink.snapshots().is_empty());
}

/// Row 4.8's run-path consumer: the live panel's *replaceable* state is
/// paced by [`AdaptivePreviewCoalescer`] while append-only flavors are
/// forwarded verbatim.
#[test]
fn live_preview_pacer_publishes_immediately_collapses_and_settles_the_latest() {
    let start = std::time::Instant::now();
    let decoration = |step: usize| {
        ToolProgressDecoration::new(format!("step {step}"), Some(format!("detail {step}")))
            .expect("bounded decoration")
    };
    let mut pacer = LivePreviewPacer::new();

    // The first replaceable state after idle is published immediately.
    let first = pacer
        .observe(decoration(0), start)
        .expect("the first state is immediate");
    assert_eq!(first.label(), "step 0");

    // Every intermediate state before the deadline collapses into one held
    // slot: no queue grows and no intermediate state is published.
    for step in 1..12 {
        assert!(
            pacer.observe(decoration(step), start).is_none(),
            "step {step} must be paced away"
        );
    }
    assert_eq!(
        pacer.stats(),
        (1, 10),
        "ten intermediates collapsed into the single held state"
    );

    // Append-only flavors bypass the coalescer completely.
    let forwarded = forward_tool_progress(
        ToolProgress::Status("still running".into()),
        &mut pacer,
        start,
    );
    assert!(
        matches!(forwarded, Some(ToolProgress::Status(message)) if message == "still running"),
        "an append-only status is forwarded verbatim"
    );
    let chunk = forward_tool_progress(
        ToolProgress::Output {
            stream: OutputStream::Stdout,
            bytes: bytes::Bytes::from_static(b"verbatim\n"),
        },
        &mut pacer,
        start,
    );
    assert!(
        matches!(chunk, Some(ToolProgress::Output { bytes, .. }) if bytes.as_ref() == b"verbatim\n"),
        "a stdout chunk is never collapsed"
    );
    assert_eq!(
        pacer.stats().0,
        1,
        "verbatim forwarding is not a publication"
    );

    // Nothing is published before the deadline, and the deadline publishes
    // the *latest* state rather than one that was paced away.
    assert!(
        pacer.take_due(start).is_none(),
        "the held state is not due yet"
    );
    let deadline = pacer
        .flush_deadline(start)
        .expect("the held state has one trailing timer");
    assert_eq!(deadline, start + DEFAULT_PREVIEW_MIN_EMIT_INTERVAL);
    let due = pacer
        .take_due(deadline)
        .expect("the deadline publishes the held state");
    assert_eq!(due.label(), "step 11", "the latest state survives collapse");
    assert_eq!(
        pacer.flush_deadline(deadline),
        None,
        "the trailing timer is cancelled by its own publication"
    );
    assert_eq!(pacer.stats(), (2, 10));

    // A terminal boundary forces whatever is still held, exactly once: a
    // finished call can never leave the panel on stale state.
    assert!(pacer.observe(decoration(20), start).is_none());
    let settled = pacer
        .settle(start)
        .expect("the terminal boundary publishes the held state");
    assert_eq!(settled.label(), "step 20");
    assert!(
        pacer.settle(start).is_none(),
        "a settled call publishes nothing twice"
    );
    assert_eq!(pacer.stats(), (3, 10));
}

#[test]
fn exact_responses_replay_estimate_counts_opaque_provider_payloads() {
    use octet_ai::{ModelCatalog, ModelId, ResponsesItem, ResponsesOutput};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    session
        .append(user_message(UserInput::from("small prompt")))
        .unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("small answer".into())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    session
        .append_responses_turn(
            assistant,
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                "type": "reasoning",
                "id": "rs_large",
                "encrypted_content": "x".repeat(40_000),
                "unknown": {"phase": "analysis"}
            }))
            .unwrap()]),
        )
        .unwrap();

    let messages = session.context().unwrap();
    let canonical = estimate_request_tokens("system", &messages, &[]);
    let estimate = reconcile_context_estimate(&session, &model, "system", &messages, &[]);
    assert!(
        estimate.structural_tokens > canonical.saturating_add(8_000),
        "opaque replay must drive the structural estimate: canonical={canonical}, replay={}",
        estimate.structural_tokens
    );
    assert_eq!(estimate.provider_tokens, None);

    let options = durable_responses_options(&session, &model, "system", None)
        .unwrap()
        .unwrap();
    assert!(options.input.is_some());
    assert_eq!(options.previous_response_id, None);
    assert!(!options.store);
}

#[test]
fn native_checkpoint_estimate_excludes_compacted_canonical_media() {
    use octet_ai::{ModelCatalog, ModelId, ResponsesItem, ResponsesOutput};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Media(Media::image_bytes(
                bytes::Bytes::from(vec![7u8; 1024 * 1024]),
                "image/png".parse().unwrap(),
            ))],
        })))
        .unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("seen".into())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    session
        .append_responses_turn(
            assistant,
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                "type": "message",
                "id": "old-output"
            }))
            .unwrap()]),
        )
        .unwrap();
    session
        .append_responses_compaction(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                "type": "compaction",
                "id": "small-checkpoint",
                "encrypted_content": "opaque"
            }))
            .unwrap()]),
        )
        .unwrap();

    let messages = session.context().unwrap();
    let estimate = reconcile_context_estimate(
        &session,
        &model,
        "system must already be compacted",
        &messages,
        &[],
    );
    assert!(
        estimate.structural_tokens < 1_000,
        "compacted-away media leaked into the replay estimate: {estimate:?}"
    );
    let exact =
        exact_responses_replay(&session, &model, "system must already be compacted").unwrap();
    let wire = serde_json::to_string(&exact.input).unwrap();
    assert!(wire.contains("small-checkpoint"));
    assert!(!wire.contains("system must already be compacted"));
}

#[test]
fn exact_responses_estimate_counts_current_media_semantically() {
    use octet_ai::{ModelCatalog, ModelId};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Media(Media::image_bytes(
                bytes::Bytes::from(vec![7u8; 1024 * 1024]),
                "image/png".parse().unwrap(),
            ))],
        })))
        .unwrap();

    let messages = session.context().unwrap();
    let estimate = reconcile_context_estimate(&session, &model, "system", &messages, &[]);
    assert!(
        (ESTIMATED_IMAGE_TOKENS..10_000).contains(&estimate.structural_tokens),
        "inline base64 must be replaced by a semantic image estimate: {estimate:?}"
    );
}

#[test]
fn post_checkpoint_instructions_are_included_in_the_exact_estimate() {
    use octet_ai::{ModelCatalog, ModelId, ResponsesItem, ResponsesOutput};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("old".into())],
        })))
        .unwrap();
    session
        .append_responses_compaction(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                "type": "compaction",
                "encrypted_content": "small"
            }))
            .unwrap()]),
        )
        .unwrap();

    let messages = session.context().unwrap();
    let short = reconcile_context_estimate(&session, &model, "short", &messages, &[]);
    let long_system = "x".repeat(128 * 1024);
    let long = reconcile_context_estimate(&session, &model, &long_system, &messages, &[]);
    assert!(
            long.structural_tokens > short.structural_tokens.saturating_add(30_000),
            "top-level instructions must participate in capacity checks: short={short:?}, long={long:?}"
        );
}

#[test]
fn switched_responses_route_uses_canonical_options_but_rejects_native_replay() {
    use octet_ai::{
        AssistantMessage, AssistantPart, Message, ModelCatalog, ModelId, Protocol, ResponsesItem,
        ResponsesOutput,
    };

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("switch.jsonl")).unwrap();
    let astra = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut luna = astra.clone();
    Arc::make_mut(&mut luna.spec).id = ModelId("gpt-6-luna".into());
    session
        .append(user_message(UserInput::from("first prompt")))
        .unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("astra answer".into())],
            model: astra.spec.id.clone(),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();
    session
        .append_responses_turn(
            assistant,
            astra.endpoint.id.clone(),
            astra.spec.id.clone(),
            ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "astra answer"}]
            }))
            .unwrap()]),
        )
        .unwrap();
    session
        .append(user_message(UserInput::from("second prompt")))
        .unwrap();

    assert!(durable_responses_options(&session, &luna, "system", None)
        .unwrap()
        .is_none());
    assert!(matches!(
        native_responses_options(&session, &luna, "system", None),
        Err(AgentError::InvalidCompactionPolicy(_))
    ));
    assert_eq!(session.context().unwrap().len(), 3);
}

#[test]
fn marked_failed_turn_boundary_keeps_exact_replay_available_after_restart() {
    use octet_ai::{ModelCatalog, ModelId};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("fails".into())],
        })))
        .unwrap();
    close_failed_turn(&mut session, &model).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("try again".into())],
        })))
        .unwrap();
    drop(session);

    let session = Session::open(path).unwrap();
    let replay = session
        .responses_replay_snapshot(&model.endpoint.id, &model.spec.id)
        .unwrap()
        .expect("explicit local provenance must not look like a missing sidecar");
    assert!(matches!(
        replay.get(1),
        Some(ResponsesReplayItem::LocalAssistant(message))
            if matches!(
                message.content.as_slice(),
                [AssistantPart::Text(text)] if text == FAILED_TURN_CONTEXT_MARKER
            )
    ));
}

#[test]
fn configured_cost_limit_fails_closed_without_trusted_model_pricing() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("unpriced.jsonl")).unwrap();
    let mut model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    std::sync::Arc::make_mut(&mut model.spec).pricing = None;

    assert!(matches!(
        reserve_request_cost(&session, &model, 1, 1, Some(10), CacheRetention::Short),
        Err(AgentError::CostUnavailable { limit: 10 })
    ));
}

#[tokio::test]
async fn native_compaction_honors_the_session_cost_limit_before_network() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    session
        .append(user_message(UserInput::from("compact this")))
        .unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "system".into(),
        sandbox: SandboxConfig::new(directory.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent.set_max_session_cost_microdollars(Some(0));

    let error = agent.compact_responses_native().await.unwrap_err();
    // The native Responses compaction endpoint has no output-cap field, so a
    // hard cost ceiling cannot be enforced and admission refuses before any
    // provider request. It must not be reported as a reserve-and-compare
    // cost limit that was never actually enforceable.
    assert!(
        matches!(error, AgentError::OutputLimitUnavailable),
        "{error:?}"
    );
    assert!(
        !matches!(
            agent
                .session()
                .head_ref()
                .and_then(|head| agent.session().entry(head)),
            Some(crate::session::Entry {
                value: EntryValue::ResponsesCompaction { .. },
                ..
            })
        ),
        "a rejected native request must not persist a checkpoint"
    );
}

fn provider_context_estimate_reference(session: &Session, model: &Model) -> Option<u64> {
    let branch = active_branch_entries(session);
    let boundary = branch
        .iter()
        .rposition(|entry| {
            matches!(
                entry.value,
                EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. }
            )
        })
        .map_or(0, |index| index.saturating_add(1));

    for (index, entry) in branch.iter().enumerate().skip(boundary).rev() {
        if !matches!(entry.value, EntryValue::Message(Message::Assistant(_))) {
            continue;
        }
        let Some(record) = session.usage_records().iter().rev().find(|record| {
            matches!(
                &record.kind,
                crate::session::UsageRecordKind::AssistantTurn { assistant }
                    if assistant == &entry.id
            ) && record.endpoint.as_ref() == Some(&model.endpoint.id)
                && record.model.as_ref() == Some(&model.spec.id)
                && usage_context_tokens(&record.usage) > 0
        }) else {
            continue;
        };
        let trailing = branch[index.saturating_add(1)..]
            .iter()
            .filter_map(|entry| match &entry.value {
                EntryValue::Message(message) => Some(message),
                _ => None,
            })
            .fold(0u64, |total, message| {
                total.saturating_add(estimate_messages_tokens(std::slice::from_ref(message)))
            });
        return Some(usage_context_tokens(&record.usage).saturating_add(trailing));
    }
    None
}

fn append_usage_fixture(session: &mut Session, model: &Model, tokens: u64) -> EntryId {
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("measured response".into())],
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
        })))
        .unwrap();
    session
        .record_assistant_usage(
            assistant.clone(),
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                total_tokens: tokens,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    assistant
}

#[test]
fn provider_usage_suffix_matches_reference_across_branches_and_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("suffix.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let check = |session: &Session| {
        assert_eq!(
            provider_context_estimate(session, &model),
            provider_context_estimate_reference(session, &model)
        );
    };
    check(&session);
    let measured = append_usage_fixture(&mut session, &model, 1234);
    check(&session);
    session
        .append(user_message(UserInput::from("trailing λ message")))
        .unwrap();
    session
        .append(EntryValue::Config {
            model: None,
            reasoning: Some("low".into()),
            reasoning_mode: None,
        })
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId("fixture-call".into()),
                content: vec![ToolResultPart::Text("tool result λ".into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    check(&session);
    append_usage_fixture(&mut session, &model, 0);
    check(&session);
    let mut other_model = model.clone();
    Arc::make_mut(&mut other_model.spec).id = octet_ai::ModelId("other-model".into());
    append_usage_fixture(&mut session, &other_model, 9000);
    check(&session);
    let mut other_endpoint = model.clone();
    Arc::make_mut(&mut other_endpoint.endpoint).id = octet_ai::EndpointId("other-endpoint".into());
    append_usage_fixture(&mut session, &other_endpoint, 9000);
    check(&session);
    let abandoned = session.head().unwrap();
    session.checkout(measured.clone()).unwrap();
    session
        .append(user_message(UserInput::from("new branch")))
        .unwrap();
    check(&session);
    append_usage_fixture(&mut session, &model, u64::MAX);
    session
        .append(user_message(UserInput::from("saturating suffix")))
        .unwrap();
    check(&session);
    assert_eq!(provider_context_estimate(&session, &model), Some(u64::MAX));
    session.checkout(abandoned).unwrap();
    session.compact("summary", measured).unwrap();
    check(&session);
    assert_eq!(provider_context_estimate(&session, &model), None);
    append_usage_fixture(&mut session, &model, 99);
    check(&session);
    session
        .append_responses_compaction(
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({
                "type": "compaction", "id": "native-checkpoint", "encrypted_content": "opaque"
            }))
            .unwrap()]),
        )
        .unwrap();
    check(&session);
    assert_eq!(provider_context_estimate(&session, &model), None);
    append_usage_fixture(&mut session, &model, 101);
    check(&session);
    drop(session);
    let reopened = Session::open_read_only(directory.path().join("suffix.jsonl")).unwrap();
    check(&reopened);
}

#[test]
fn provider_usage_entry_work_is_bounded_by_the_unmeasured_suffix() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("work.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    for history in [10, 100, 1000] {
        while session.entries().len() < history {
            session
                .append(user_message(UserInput::from("settled history")))
                .unwrap();
        }
        if session.usage_records().is_empty() {
            PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(0));
            assert_eq!(provider_context_estimate(&session, &model), None);
            assert_eq!(PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.get()), 0);
        }
        append_usage_fixture(&mut session, &model, 1000);
        session
            .append(user_message(UserInput::from("fixed tail")))
            .unwrap();
        PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(0));
        let estimate = provider_context_estimate(&session, &model);
        assert_eq!(PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.get()), 3);
        assert_eq!(
            estimate,
            provider_context_estimate_reference(&session, &model)
        );
    }
}

#[test]
fn provider_usage_abandoned_records_remain_an_explicit_scan_cost() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("abandoned.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let anchor = append_usage_fixture(&mut session, &model, 1234);
    let mut other = model.clone();
    Arc::make_mut(&mut other.spec).id = octet_ai::ModelId("other-model".into());
    for records in [5, 20] {
        for _ in 0..records {
            append_usage_fixture(&mut session, &other, 99);
        }
        session.checkout(anchor.clone()).unwrap();
        PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(0));
        PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
        assert_eq!(provider_context_estimate(&session, &model), Some(1234));
        assert_eq!(PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.get()), 1);
        assert_eq!(
            PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()),
            session.usage_records().len()
        );
        assert_eq!(
            provider_context_estimate(&session, &model),
            provider_context_estimate_reference(&session, &model)
        );
    }
}

#[test]
fn provider_usage_unmatched_assistants_scan_the_ledger_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("unmatched.jsonl")).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let anchor = append_usage_fixture(&mut session, &model, 1234);
    // Preserve newest usable record semantics even with repeated accounting
    // for one assistant and a newer zero-token record.
    for tokens in [2345, 0] {
        session
            .record_assistant_usage(
                anchor.clone(),
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    total_tokens: tokens,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
    }
    let mut other = model.clone();
    Arc::make_mut(&mut other.spec).id = octet_ai::ModelId("other-model".into());
    for history in [8, 32, 128] {
        while session.entries().len() < history {
            append_usage_fixture(&mut session, &other, 99);
        }
        PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
        assert_eq!(
            provider_context_estimate(&session, &model),
            provider_context_estimate_reference(&session, &model)
        );
        assert_eq!(
            PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()),
            session.usage_records().len()
        );
        // No matching model at all must also be a single ledger pass.
        let mut absent = model.clone();
        Arc::make_mut(&mut absent.spec).id = octet_ai::ModelId("absent".into());
        PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
        assert_eq!(provider_context_estimate(&session, &absent), None);
        assert_eq!(
            PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()),
            session.usage_records().len()
        );
    }
    session.checkout(anchor).unwrap();
    PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
    assert_eq!(provider_context_estimate(&session, &model), Some(2345));
    append_usage_fixture(&mut session, &model, 3456);
    PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
    assert_eq!(provider_context_estimate(&session, &model), Some(3456));
    assert_eq!(PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()), 1);
}

/// Offline matched microbenchmark, not provider or end-to-end launch latency.
/// Run with: cargo test --release --offline --locked -p octet-agent --lib
/// provider_usage_suffix_benchmark -- --ignored --nocapture --test-threads=1
#[test]
#[ignore = "manual matched timing experiment"]
fn provider_usage_suffix_benchmark() {
    let directory = tempfile::tempdir().unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let repetitions = 200;
    for history in [100, 1000, 10_000] {
        let mut session =
            Session::create(directory.path().join(format!("benchmark-{history}.jsonl"))).unwrap();
        while session.entries().len() < history {
            session
                .append(user_message(UserInput::from("settled history")))
                .unwrap();
        }
        for scenario in ["unmeasured", "suffix"] {
            if scenario == "suffix" {
                append_usage_fixture(&mut session, &model, 1000);
                session
                    .append(user_message(UserInput::from("fixed tail")))
                    .unwrap();
            }
            assert_eq!(
                provider_context_estimate(&session, &model),
                provider_context_estimate_reference(&session, &model)
            );
            for trial in 0..9 {
                // Alternate order to avoid systematically favoring warm caches.
                for candidate in if trial % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let estimate = if candidate {
                        provider_context_estimate
                    } else {
                        provider_context_estimate_reference
                    };
                    let start = std::time::Instant::now();
                    for _ in 0..repetitions {
                        std::hint::black_box(estimate(
                            std::hint::black_box(&session),
                            std::hint::black_box(&model),
                        ));
                    }
                    println!("provider_usage_suffix scenario={scenario} history={history} trial={trial} candidate={candidate} repetitions={repetitions} elapsed_ns={}", start.elapsed().as_nanos());
                }
            }
        }
    }
}

#[test]
fn provider_usage_baseline_skips_newer_unusable_records_and_counts_trailing_messages() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelCatalog, ModelId, Protocol};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    session
        .append(user_message(UserInput::from("old prompt")))
        .unwrap();
    let measured = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("old response".into())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .record_assistant_usage(
            measured,
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                input_tokens: 79_000,
                output_tokens: 1_000,
                total_tokens: 80_000,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    session
        .append(user_message(UserInput::from("x".repeat(4_000))))
        .unwrap();
    let unmeasured = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("new response".into())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .record_assistant_usage(
            unmeasured,
            model.endpoint.id.clone(),
            ModelId("different-model".into()),
            Usage::default(),
            None,
        )
        .unwrap();

    let estimate = provider_context_estimate(&session, &model).unwrap();
    assert!(estimate > 81_000, "{estimate}");
}

#[test]
fn provider_usage_before_latest_compaction_is_not_reused() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelCatalog, ModelId, Protocol};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    session
        .append(user_message(UserInput::from("old prompt")))
        .unwrap();
    let assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("old response".into())],
            model: model.spec.id.clone(),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .record_assistant_usage(
            assistant.clone(),
            model.endpoint.id.clone(),
            model.spec.id.clone(),
            Usage {
                total_tokens: 100_000,
                ..Usage::default()
            },
            None,
        )
        .unwrap();
    session.compact("short summary", assistant).unwrap();

    assert_eq!(provider_context_estimate(&session, &model), None);
}

#[test]
fn usage_accumulates_across_turns() {
    let mut total = Usage::default();
    let turn = Usage {
        input_tokens: 10,
        output_tokens: 5,
        reasoning_tokens: 2,
        total_tokens: 15,
        ..Usage::default()
    };
    add_usage(&mut total, &turn);
    add_usage(&mut total, &turn);
    assert_eq!(total.input_tokens, 20);
    assert_eq!(total.output_tokens, 10);
    assert_eq!(total.reasoning_tokens, 4);
    assert_eq!(total.total_tokens, 30);
}

#[test]
fn run_cost_carries_submicrodollar_remainders_across_turns() {
    let mut total = CostAccumulator::default();
    let fractional = Cost {
        total_picodollars_remainder: 600_000,
        ..Cost::default()
    };
    total.add(Some(fractional));
    total.add(Some(fractional));
    assert_eq!(total.microdollars, 1);
    assert_eq!(total.picodollars_remainder, 200_000);
}

#[test]
fn compaction_boundaries_include_each_completed_tool_episode() {
    use octet_ai::{
        AssistantMessage, AssistantPart, ModelId, Protocol, ToolResult, ToolResultPart,
    };

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    session
        .append(user_message(UserInput::from("one task")))
        .unwrap();
    for (index, text) in [("a", "first"), ("b", "second")] {
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: octet_ai::ToolCallId(index.into()),
                    name: "read".into(),
                    arguments_json: "{}".into(),
                    argument_error: None,
                })],
                model: ModelId("test".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: octet_ai::ToolCallId(index.into()),
                    content: vec![ToolResultPart::Text(text.into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            })))
            .unwrap();
    }
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("done".into())],
            model: ModelId("test".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    assert_eq!(turn_starts(&session).len(), 3);
}

#[test]
fn assistant_after_compaction_marker_remains_a_turn_boundary() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    session
        .append(user_message(UserInput::from("one task")))
        .unwrap();
    let first_assistant = session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("first".into())],
            model: ModelId("test".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    session
        .append(user_message(UserInput::from("continue")))
        .unwrap();
    session.compact("summary", first_assistant).unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("after marker".into())],
            model: ModelId("test".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    assert_eq!(turn_starts(&session).len(), 2);
}

#[test]
fn hard_token_reservation_rejects_before_a_request_can_cross_the_ceiling() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("token-limit.jsonl")).unwrap();
    let error = reserve_request_tokens(&session, 700, 400, Some(1_000)).unwrap_err();
    assert!(matches!(
        error,
        AgentError::TokenLimit {
            current: 0,
            reserved: 1_100,
            limit: 1_000
        }
    ));
}

#[test]
fn delegated_usage_is_accounting_not_parent_context_token_consumption() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("delegated-ledger.jsonl")).unwrap();
    session
        .record_delegated_agent_usage(DelegatedUsage {
            agent_id: "agent-1".into(),
            turn_count: 2,
            tool_call_count: 1,
            endpoint: octet_ai::EndpointId("test-endpoint".into()),
            model: octet_ai::ModelId("test-model".into()),
            usage: Usage {
                input_tokens: 40_000,
                output_tokens: 10_000,
                total_tokens: 50_000,
                ..Usage::default()
            },
            cost: None,
        })
        .unwrap();

    assert_eq!(session_total_tokens_for_own_context(&session), 0);
    assert!(reserve_request_tokens(&session, 700, 200, Some(1_000)).is_ok());
    assert_eq!(session.usage_records()[0].usage.total_tokens, 50_000);
}

#[test]
fn delegated_snapshots_use_only_committed_root_usage_and_borrow_cost_remainders() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mirror-deltas.jsonl");
    let mut session = Session::create(&path).unwrap();
    let mut snapshot = DelegatedUsage {
        agent_id: "child-1".into(),
        turn_count: 1,
        tool_call_count: 0,
        endpoint: octet_ai::EndpointId("endpoint".into()),
        model: octet_ai::ModelId("model".into()),
        usage: Usage {
            input_tokens: 10,
            total_tokens: 10,
            ..Usage::default()
        },
        cost: Some(Cost {
            total_picodollars_remainder: 900_000,
            ..Cost::default()
        }),
    };
    record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
    // A failed append must leave the previous committed baseline intact.
    let mut read_only = Session::open_read_only(&path).unwrap();
    snapshot.usage.input_tokens = 20;
    snapshot.usage.total_tokens = 20;
    snapshot.cost = Some(Cost {
        total: 1,
        total_picodollars_remainder: 200_000,
        ..Cost::default()
    });
    assert!(record_delegated_usage_once(&mut read_only, snapshot.clone()).is_err());
    assert_eq!(read_only.usage_records().len(), 1);
    drop(read_only);
    drop(session);

    let mut session = Session::open(&path).unwrap();
    record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
    assert_eq!(session.usage_records().len(), 2);
    assert_eq!(session.usage_records()[1].usage.total_tokens, 10);
    assert_eq!(session.usage_records()[1].cost.unwrap().total, 0);
    assert_eq!(
        session.usage_records()[1]
            .cost
            .unwrap()
            .total_picodollars_remainder,
        300_000
    );
    record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
    assert_eq!(session.usage_records().len(), 2);
    assert_eq!(session.total_cost_microdollars(), 1);
    assert_eq!(session.total_cost_picodollars_remainder(), 200_000);

    // Auxiliary/cost-only updates can arrive with the same turn/tool counts.
    snapshot.cost.as_mut().unwrap().total_picodollars_remainder = 400_000;
    record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
    assert_eq!(session.usage_records().len(), 3);
    assert_eq!(session.total_cost_picodollars_remainder(), 400_000);
    drop(session);
    let mut session = Session::open(&path).unwrap();
    record_delegated_usage_once(&mut session, snapshot).unwrap();
    assert_eq!(session.usage_records().len(), 3);
    assert_eq!(
        session
            .usage_records()
            .iter()
            .map(|record| record.usage.total_tokens)
            .sum::<u64>(),
        20
    );
}

#[test]
fn repeated_delegated_uncertainty_mirroring_is_idempotent_and_keeps_known_subtotal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mirror.jsonl");
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut session = Session::create(&path).unwrap();
    for pass in 0..3 {
        assert_eq!(
            mirror_delegated_uncertainty(&mut session, &model, "child-1", true, None).unwrap(),
            pass == 0
        );
        record_delegated_usage_once(
            &mut session,
            DelegatedUsage {
                agent_id: "child-1".into(),
                turn_count: 1,
                tool_call_count: 0,
                endpoint: model.endpoint.id.clone(),
                model: model.spec.id.clone(),
                usage: Usage {
                    total_tokens: 10,
                    input_tokens: 10,
                    ..Usage::default()
                },
                cost: Some(octet_ai::Cost {
                    input: 7,
                    total: 7,
                    ..Default::default()
                }),
            },
        )
        .unwrap();
    }
    assert_eq!(session.usage_uncertainty_records().len(), 1);
    assert_eq!(session.usage_records().len(), 1);
    assert_eq!(session.total_cost_microdollars(), 7);
    drop(session);
    let mut session = Session::open(path).unwrap();
    assert!(!mirror_delegated_uncertainty(&mut session, &model, "child-1", true, None).unwrap());
    assert!(session.has_uncertain_usage());
    assert_eq!(session.usage_uncertainty_records().len(), 1);
    assert_eq!(session.total_cost_microdollars(), 7);
}

#[test]
fn delegated_uncertainty_mirrors_per_child_deltas_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mirror-bounded.jsonl");
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut root = Session::create(&path).unwrap();
    let exposure = |tokens, cost| {
        Some(UsageUncertaintyBound {
            tokens,
            cost_microdollars: Some(cost),
        })
    };
    assert!(
        mirror_delegated_uncertainty(&mut root, &model, "agent-1", true, exposure(10, 2)).unwrap()
    );
    assert!(
        !mirror_delegated_uncertainty(&mut root, &model, "agent-1", true, exposure(10, 2)).unwrap()
    );
    assert!(
        mirror_delegated_uncertainty(&mut root, &model, "agent-1", true, exposure(25, 7)).unwrap()
    );
    assert_eq!(
        root.usage_uncertainty_bounds(),
        &[exposure(10, 2), exposure(15, 5)]
    );
    drop(root);
    let mut root = Session::open(&path).unwrap();
    assert!(
        mirror_delegated_uncertainty(&mut root, &model, "agent-1", true, exposure(40, 9)).unwrap()
    );
    assert!(
        mirror_delegated_uncertainty(&mut root, &model, "agent-2", true, exposure(3, 1)).unwrap()
    );
    assert_eq!(root.usage_uncertainty_exposure(), exposure(43, 10));
    assert!(root
        .usage_uncertainty_records()
        .iter()
        .take(3)
        .all(|record| record.operation == "delegated_agent:agent-1"));
    assert!(mirror_delegated_uncertainty(&mut root, &model, "agent-1", true, None).unwrap());
    assert!(!mirror_delegated_uncertainty(&mut root, &model, "agent-1", true, None).unwrap());
    assert!(root.usage_uncertainty_exposure().is_none());
}

#[tokio::test]
async fn abort_flag_wakes_waiters_and_stays_set() {
    let flag = Arc::new(AbortFlag::default());
    let waiter = {
        let flag = flag.clone();
        tokio::spawn(async move { flag.wait().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    flag.set();
    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("waiter must wake")
        .unwrap();
    // Late waiters return immediately.
    tokio::time::timeout(std::time::Duration::from_secs(1), flag.wait())
        .await
        .expect("level-triggered wait");
    assert!(flag.is_set());
}

fn active_tool_test_agent(
    directory: &std::path::Path,
    session: Session,
    extensions: ExtensionHost,
) -> Agent {
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "system".into(),
        sandbox: SandboxConfig::new(directory),
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

#[test]
fn inactive_delegation_reports_no_active_workers() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("no-delegation.jsonl")).unwrap();
    let agent = active_tool_test_agent(directory.path(), session, ExtensionHost::new());
    assert_eq!(agent.active_delegated_worker_count(), 0);
}

fn active_tool_test_extensions(names: &[&'static str]) -> ExtensionHost {
    let mut extensions = ExtensionHost::new();
    for &name in names {
        extensions.tool(PromptTool {
            name,
            snippet: None,
            guidelines: &[],
        });
    }
    extensions
}

fn advertised_tool_names(agent: &Agent) -> Vec<String> {
    agent
        .registered_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect()
}

fn persisted_tool_result_texts(session: &Session) -> Vec<String> {
    session
        .context()
        .unwrap()
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(user.content.iter()),
            Message::Assistant(_) => None,
        })
        .flatten()
        .filter_map(|part| match part {
            UserPart::ToolResult(result) => Some(result.content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            ToolResultPart::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn set_active_tool_names_narrows_the_advertised_surface_and_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("active-tools.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![
                AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: octet_ai::ToolCallId("call-alpha".into()),
                    name: "alpha".into(),
                    arguments_json: "{}".into(),
                    argument_error: None,
                }),
                AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: octet_ai::ToolCallId("call-beta".into()),
                    name: "beta".into(),
                    arguments_json: "{}".into(),
                    argument_error: None,
                }),
            ],
            model: octet_ai::ModelId("test".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        session,
        active_tool_test_extensions(&["alpha", "beta"]),
    );
    assert_eq!(advertised_tool_names(&agent), ["alpha", "beta"]);
    let revision = agent.extensions.tool_snapshot().0;

    agent
        .set_active_tool_names(Some(BTreeSet::from(["alpha".to_owned()])))
        .unwrap();

    assert_eq!(advertised_tool_names(&agent), ["alpha"]);
    assert!(agent.extensions.tool_snapshot().0 > revision);

    // A persisted call issued before the change resolves against the
    // narrowed dispatch map: the deactivated tool gets the existing
    // unknown-tool result while the active tool still dispatches.
    agent.recover_pending_tools(false).await.unwrap();
    let texts = persisted_tool_result_texts(agent.session());
    assert!(
        texts.iter().any(|text| text.contains("unknown tool: beta")),
        "a deactivated tool must be refused by the dispatch map: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("`alpha` was not replayed")),
        "the still-active tool must remain dispatched: {texts:?}"
    );
}

#[test]
fn set_active_tool_names_refuses_unknown_names_without_state_change() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("active-unknown.jsonl")).unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        session,
        active_tool_test_extensions(&["alpha", "beta"]),
    );
    let revision = agent.extensions.tool_snapshot().0;

    let error = agent
        .set_active_tool_names(Some(BTreeSet::from([
            "alpha".to_owned(),
            "ghost".to_owned(),
        ])))
        .unwrap_err();
    match error {
        AgentError::UnknownActiveTools(refused) => assert_eq!(refused, ["ghost"]),
        other => panic!("expected UnknownActiveTools, got {other:?}"),
    }
    assert_eq!(advertised_tool_names(&agent), ["alpha", "beta"]);
    assert_eq!(agent.extensions.tool_snapshot().0, revision);
}

#[test]
fn set_active_tool_names_cannot_readmit_policy_excluded_tools() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("active-policy.jsonl")).unwrap();
    let mut extensions = active_tool_test_extensions(&["read", "write"]);
    extensions.set_tool_policy(|name| name != "write");
    let mut agent = active_tool_test_agent(directory.path(), session, extensions);

    assert_eq!(advertised_tool_names(&agent), ["read"]);
    assert_eq!(agent.registered_tool_names(), ["read"]);
    let revision = agent.extensions.tool_snapshot().0;

    for requested in [
        BTreeSet::from(["write".to_owned()]),
        BTreeSet::from(["read".to_owned(), "write".to_owned()]),
    ] {
        let error = agent.set_active_tool_names(Some(requested)).unwrap_err();
        match error {
            AgentError::UnknownActiveTools(refused) => assert_eq!(refused, ["write"]),
            other => panic!("expected UnknownActiveTools, got {other:?}"),
        }
    }
    assert_eq!(advertised_tool_names(&agent), ["read"]);
    assert_eq!(agent.extensions.tool_snapshot().0, revision);

    agent
        .set_active_tool_names(Some(BTreeSet::from(["read".to_owned()])))
        .unwrap();
    assert_eq!(advertised_tool_names(&agent), ["read"]);
}

#[test]
fn set_active_tool_names_none_restores_the_host_policed_surface() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("active-restore.jsonl")).unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        session,
        active_tool_test_extensions(&["alpha", "beta"]),
    );

    agent
        .set_active_tool_names(Some(BTreeSet::from(["alpha".to_owned()])))
        .unwrap();
    assert_eq!(advertised_tool_names(&agent), ["alpha"]);
    // Deactivated names stay registered, so they can be requested again.
    assert_eq!(agent.registered_tool_names(), ["alpha", "beta"]);

    agent.set_active_tool_names(None).unwrap();
    assert_eq!(advertised_tool_names(&agent), ["alpha", "beta"]);
}

#[test]
fn set_active_tool_names_bump_the_revision_the_run_host_observes() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("active-run-host.jsonl")).unwrap();
    let mut agent = active_tool_test_agent(
        directory.path(),
        session,
        active_tool_test_extensions(&["alpha", "beta"]),
    );
    // `Agent::prompt` hands a clone of the host to the streaming loop.
    let run_host = agent.extensions.clone();
    let before = run_host.tool_snapshot().0;

    agent
        .set_active_tool_names(Some(BTreeSet::from(["beta".to_owned()])))
        .unwrap();

    let (revision, tools) = run_host.tool_snapshot();
    assert!(revision > before);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.definition().name.clone())
            .collect::<Vec<_>>(),
        ["beta"]
    );
}
