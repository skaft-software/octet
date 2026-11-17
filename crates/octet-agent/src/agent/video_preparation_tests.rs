//! Exercise the foreground result-commit poll with a real read and a slow decoder.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct VideoReadTransport(bool);

#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for VideoReadTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        let continuing = self.0
            && request
                .messages
                .iter()
                .any(|message| matches!(message, Message::Assistant(_)));
        let output = if continuing {
            serde_json::json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]})
        } else {
            serde_json::json!({"type":"function_call","call_id":"video-read","name":"read","arguments":r#"{"path":"clip.mp4"}"#})
        };
        let responses_output = (model.protocol == Protocol::OpenAiResponses)
            .then(|| octet_ai::ResponsesOutput::new(vec![serde_json::from_value(output).unwrap()]));
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: if continuing {
                        vec![AssistantPart::Text("done".into())]
                    } else {
                        vec![AssistantPart::ToolCall(ToolCall {
                            id: octet_ai::ToolCallId("video-read".into()),
                            name: "read".into(),
                            arguments_json: r#"{"path":"clip.mp4"}"#.into(),
                            argument_error: None,
                            async_execution: self.0,
                        })]
                    },
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: if continuing {
                    StopReason::EndTurn
                } else {
                    StopReason::ToolUse
                },
                usage: Usage::default(),
                cost: None,
                response_id: None,
                responses_output,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

#[test]
fn cold_video_read_keeps_foreground_input_pollable() {
    // PATH belongs to a separate process so concurrent tests keep real FFmpeg.
    const CHILD: &str = "OCTET_VIDEO_DRIVER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("ffprobe");
        std::fs::write(&probe, concat!(
            "#!/usr/bin/python3\n",
            "import os, time\n",
            "open(os.environ['OCTET_VIDEO_PROBE_STARTED'], 'w').write(str(os.getpid()))\n",
            "time.sleep(1)\n",
            "print('{\"streams\":[{\"width\":16,\"height\":16}],\"format\":{\"duration\":\"2\"}}')\n",
        )).unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let decoder = dir.path().join("ffmpeg");
        std::fs::write(&decoder, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&decoder, std::fs::Permissions::from_mode(0o700)).unwrap();
        for mode in ["complete", "abort"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "agent::tests::video_preparation_tests::cold_video_read_keeps_foreground_input_pollable", "--nocapture"])
                .env(CHILD, mode)
                .env("PATH", dir.path())
                .env("OCTET_VIDEO_PROBE_STARTED", dir.path().join(mode))
                .output().unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(dir.path().join("clip.mp4"), include_bytes!("../../../octet-ai/src/video/tiny.mp4")).unwrap();
        let mut host = ExtensionHost::new();
        host.load(&crate::tools::CoreTools);
        let mut agent = active_tool_test_agent(dir.path(), Session::create(&path).unwrap(), host);
        assert!(!agent.model.spec.supports_native_video());
        assert!(agent.model.spec.supports_video_input());
        agent.client.register_host_stream_transport(agent.model.endpoint.id.clone(), Arc::new(VideoReadTransport(false)));
        let mut run = agent.prompt("read clip.mp4").await.unwrap();
        let start = std::time::Instant::now();
        let started = std::path::PathBuf::from(std::env::var_os("OCTET_VIDEO_PROBE_STARTED").unwrap());
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                    assert!(start.elapsed() < std::time::Duration::from_millis(500), "foreground run.next() blocked input for {:?}", start.elapsed());
                    if started.exists() { break; }
                }
                event = run.next() => {
                    assert!(event.is_some(), "run ended before decoder preparation");
                    assert!(!matches!(event, Some(AgentEvent::ToolFinished { .. })), "tool result commit blocked the input branch for {:?}", start.elapsed());
                }
            }
        }
        let aborting = std::env::var(CHILD).unwrap() == "abort";
        let settlement = std::time::Instant::now();
        if aborting { run.control().abort(); }
        let mut tool_finished = false;
        let mut aborted = false;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while let Some(event) = run.next().await {
                match event {
                    AgentEvent::ToolFinished { result, .. } => {
                        let output = result.expect("completed read survives optional preparation cancellation");
                        assert!(!output.is_error());
                        tool_finished = true;
                    }
                    AgentEvent::RunFinished { reason, .. } => {
                        assert!(!matches!(reason, FinishReason::Failed(_)), "{reason:?}");
                        aborted = matches!(reason, FinishReason::Aborted);
                    }
                    _ => {}
                }
            }
        }).await.expect("bounded video result settlement");
        assert!(tool_finished);
        assert_eq!(aborted, aborting);
        if aborting { assert!(settlement.elapsed() < std::time::Duration::from_millis(500)); }
        drop(run);
        let result_text = persisted_tool_result_texts(agent.session()).join("\n");
        assert!(result_text.contains("video source retained; frames prepared before provider input"));
        assert!(!result_text.contains("frames not prepared"));
        let context = agent.session().context().unwrap();
        let video = context.iter().find_map(|message| match message {
            Message::User(user) => user.content.iter().find_map(|part| match part {
                UserPart::Media(Media::Video(video)) => Some(video),
                _ => None,
            }),
            _ => None,
        }).expect("original video is durable even on preparation failure/abort");
        assert_eq!(video.data.as_ref(), include_bytes!("../../../octet-ai/src/video/tiny.mp4"));
        let again = std::time::Instant::now();
        let error = video.prepare_frames().await.unwrap_err().to_string();
        assert!(error.contains(if aborting { "cancelled" } else { "decoder rejected" }));
        assert!(again.elapsed() < std::time::Duration::from_millis(100), "failed sampling must stay cached");
        if aborting {
            let pid: i32 = std::fs::read_to_string(&started).unwrap().parse().unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while unsafe { libc::kill(pid, 0) } == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.expect("cancelled decoder is killed and reaped");
        }
        drop(context);
        drop(agent);
        let saved = Session::open(&path).unwrap().context().unwrap();
        assert!(saved.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Media(Media::Video(_)))))));
    });
}

#[tokio::test]
async fn resumed_video_prepares_before_request_projection_without_reloading_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resume.jsonl");
    let mut session = Session::create(&path).unwrap();
    let InputPart::Media(media) = video_input_fixture() else {
        unreachable!()
    };
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Media(media)],
        })))
        .unwrap();
    drop(session);
    let session = Session::open(&path).unwrap();
    let context = session.context().unwrap();
    let Message::User(user) = &context[0] else {
        unreachable!()
    };
    let UserPart::Media(Media::Video(video)) = &user.content[0] else {
        unreachable!()
    };
    assert!(video.cached_frames().is_none());
    let mut agent = active_tool_test_agent(dir.path(), session, ExtensionHost::new());
    let transport = Arc::new(ToolBudgetTransport {
        calls: std::sync::atomic::AtomicUsize::new(0),
        expected_tools: vec![],
    });
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), transport.clone());
    let mut run = agent.prompt("describe saved clip").await.unwrap();
    assert!(
        video.cached_frames().is_none(),
        "synchronous prompt projections stay local-state only"
    );
    while let Some(event) = run.next().await {
        assert!(
            !matches!(
                event,
                AgentEvent::RunFinished {
                    reason: FinishReason::Failed(_),
                    ..
                }
            ),
            "{event:?}"
        );
    }
    drop(run);
    assert_eq!(video.cached_frames().unwrap().len(), 4);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(agent.prepared_video_context.unwrap().messages, 2);
}

// Isolate decoder PATH changes in a child so other tests retain real FFmpeg.
fn with_slow_decoder(test_name: &str, test: impl FnOnce()) {
    const CHILD: &str = "OCTET_VIDEO_LIFECYCLE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        test();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let probe = dir.path().join("ffprobe");
    std::fs::write(
        &probe,
        concat!(
        "#!/usr/bin/python3\n",
        "import os, time\n",
        "open(os.environ['OCTET_VIDEO_PROBE_STARTED'], 'w').write(str(os.getpid()))\n",
        "time.sleep(2)\n",
        "print('{\"streams\":[{\"width\":16,\"height\":16}],\"format\":{\"duration\":\"2\"}}')\n",
    ),
    )
    .unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!(
            "agent::tests::video_preparation_tests::{test_name}"
        ))
        .arg("--nocapture")
        .env(CHILD, test_name)
        .env("PATH", dir.path())
        .env("OCTET_VIDEO_PROBE_STARTED", dir.path().join("started"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("video lifecycle test child exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn drop_completed_video_read(background: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drop.jsonl");
    let source = dir.path().join("clip.mp4");
    std::fs::write(
        &source,
        include_bytes!("../../../octet-ai/src/video/tiny.mp4"),
    )
    .unwrap();
    let mut host = ExtensionHost::new();
    host.load(&crate::tools::CoreTools);
    let mut agent = active_tool_test_agent(dir.path(), Session::create(&path).unwrap(), host);
    if background {
        agent.model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-6-astra".into()))
            .unwrap();
        Arc::make_mut(&mut agent.model.endpoint).transport = octet_ai::EndpointTransport::Http;
        assert!(agent.model.responses_features().async_tools);
        agent.reasoning = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low);
        agent.max_turns = Some(2);
    }
    agent.client.register_host_stream_transport(
        agent.model.endpoint.id.clone(),
        Arc::new(VideoReadTransport(background)),
    );
    let mut run = agent.prompt("read video").await.unwrap();
    let started = std::path::PathBuf::from(std::env::var_os("OCTET_VIDEO_PROBE_STARTED").unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(10)) => if started.exists() { break; },
                event = run.next() => {
                    assert!(!matches!(event, Some(AgentEvent::RunFinished { reason: FinishReason::Failed(_), .. })), "{event:?}");
                    assert!(event.is_some(), "run ended before optional preparation");
                    assert!(!matches!(event, Some(AgentEvent::ToolFinished { .. })));
                }
            }
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(source).unwrap();
    // Match interactive shutdown: abandon the stream without draining it.
    drop(run);
    let context = agent.session().context().unwrap();
    assert!(context.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Media(Media::Video(_)))))), "completed video read lost its durable bytes during optional preparation");
    assert!(persisted_tool_result_texts(agent.session())
        .iter()
        .any(|text| text.contains("read=video")));
    drop(agent);
    let session = Session::open(&path).unwrap();
    let context = session.context().unwrap();
    let videos: Vec<_> = context
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            UserPart::Media(Media::Video(video)) => Some(video),
            _ => None,
        })
        .collect();
    assert_eq!(
        videos.len(),
        1,
        "the completed result is committed exactly once"
    );
    assert_eq!(
        videos[0].data.as_ref(),
        include_bytes!("../../../octet-ai/src/video/tiny.mp4")
    );
    assert!(!persisted_tool_result_texts(&session)
        .iter()
        .any(|text| text.contains("tool execution cancelled")));
}

#[test]
fn dropped_run_preserves_completed_video_bytes() {
    with_slow_decoder("dropped_run_preserves_completed_video_bytes", || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(drop_completed_video_read(false));
    });
}

#[test]
fn dropped_background_settlement_preserves_completed_video_bytes() {
    with_slow_decoder(
        "dropped_background_settlement_preserves_completed_video_bytes",
        || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(drop_completed_video_read(true));
        },
    );
}

enum ExcludedVideoWindow {
    Branch,
    LocalCompaction,
    NativeCompaction,
}

async fn excluded_video_does_not_launch_decoder(window: ExcludedVideoWindow) {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("inactive.jsonl")).unwrap();
    let InputPart::Media(media) = video_input_fixture() else {
        unreachable!()
    };
    let Media::Video(video) = media.clone() else {
        unreachable!()
    };
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Media(media)],
        })))
        .unwrap();
    let native = matches!(window, ExcludedVideoWindow::NativeCompaction);
    let model = tool_media_model(
        if native {
            Protocol::OpenAiResponses
        } else {
            Protocol::OpenAiChat
        },
        octet_ai::ModalitySet::none().with(Modality::Image),
    );
    if native {
        let checkpoint = octet_ai::ResponsesItem::new(serde_json::json!({
            "type": "compaction",
            "id": "native-video-checkpoint",
            "encrypted_content": "opaque",
        }))
        .unwrap();
        session
            .append_responses_compaction(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                octet_ai::ResponsesOutput::new(vec![checkpoint]),
            )
            .unwrap();
    } else if matches!(window, ExcludedVideoWindow::LocalCompaction) {
        let first_kept = session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("current context".into())],
            })))
            .unwrap();
        session
            .compact("earlier video summarized", first_kept)
            .unwrap();
    } else {
        session.checkout_root().unwrap();
    }
    if !native {
        assert!(!session.context().unwrap().iter().any(|message| {
            matches!(message, Message::User(user) if
                user.content.iter().any(|part| matches!(part, UserPart::Media(Media::Video(_)))))
        }));
    }
    let mut agent = active_tool_test_agent(dir.path(), session, ExtensionHost::new());
    agent.model = model;
    let transport = Arc::new(ToolBudgetTransport {
        calls: std::sync::atomic::AtomicUsize::new(0),
        expected_tools: vec![],
    });
    agent
        .client
        .register_host_stream_transport(agent.model.endpoint.id.clone(), transport.clone());
    let mut run = agent
        .prompt("question unrelated to inactive video")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_millis(300), async {
        while let Some(event) = run.next().await {
            assert!(
                !matches!(
                    event,
                    AgentEvent::RunFinished {
                        reason: FinishReason::Failed(_),
                        ..
                    }
                ),
                "{event:?}"
            );
        }
    })
    .await;
    let started = std::path::PathBuf::from(std::env::var_os("OCTET_VIDEO_PROBE_STARTED").unwrap());
    assert!(
        !started.exists(),
        "decoder was launched for video excluded from current context"
    );
    result.expect("inactive video must not delay the unrelated turn");
    drop(run);
    assert!(video.cached_frames().is_none());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn inactive_branch_video_does_not_launch_decoder() {
    with_slow_decoder("inactive_branch_video_does_not_launch_decoder", || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(excluded_video_does_not_launch_decoder(
                ExcludedVideoWindow::Branch,
            ));
    });
}

#[test]
fn compacted_video_does_not_launch_decoder() {
    with_slow_decoder("compacted_video_does_not_launch_decoder", || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(excluded_video_does_not_launch_decoder(
                ExcludedVideoWindow::LocalCompaction,
            ));
    });
}

#[test]
fn native_compacted_video_does_not_launch_decoder() {
    with_slow_decoder("native_compacted_video_does_not_launch_decoder", || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(excluded_video_does_not_launch_decoder(
                ExcludedVideoWindow::NativeCompaction,
            ));
    });
}

#[test]
fn video_candidates_advance_incrementally_and_reset_with_the_active_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("candidates.jsonl")).unwrap();
    let model = tool_media_model(
        Protocol::OpenAiChat,
        octet_ai::ModalitySet::none().with(Modality::Image),
    );
    let InputPart::Media(video) = video_input_fixture() else {
        unreachable!()
    };
    let video_message = || {
        EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Media(video.clone())],
        }))
    };
    session.append(video_message()).unwrap();
    for _ in 0..100 {
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("settled text".repeat(100))],
            })))
            .unwrap();
    }
    let (cursor, initial) = session_video_candidates(&session, &model, None).unwrap();
    assert_eq!(
        lowered_tool_result_media(match &initial[0] {
            Message::User(user) => user,
            _ => unreachable!(),
        })
        .count(),
        1
    );
    let (_, unchanged) = session_video_candidates(&session, &model, Some(cursor)).unwrap();
    assert!(
        matches!(&unchanged[0], Message::User(user) if user.content.is_empty()),
        "settled prefix must not be scanned again"
    );
    let mut limited_model = model.clone();
    Arc::make_mut(&mut limited_model.spec)
        .preset
        .image_input_limits = Some(ImageInputLimits {
        max_width: 8,
        max_height: 8,
        max_bytes: 4096,
    });
    let (_, changed_limits) =
        session_video_candidates(&session, &limited_model, Some(cursor)).unwrap();
    assert!(
        matches!(&changed_limits[0], Message::User(user) if user.content.len() == 1),
        "changed limits must revisit historical video even for the same model id"
    );
    session.append(video_message()).unwrap();
    let (cursor, suffix) = session_video_candidates(&session, &model, Some(cursor)).unwrap();
    assert!(matches!(&suffix[0], Message::User(user) if user.content.len() == 1));
    // A coalesced result can add media without increasing context length.
    let result_message = || {
        EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId("read".into()),
                content: vec![ToolResultPart::Media(video.clone())],
                is_error: false,
                added_tool_names: None,
                tool_name: Some("read".into()),
            })],
        }))
    };
    session.append(result_message()).unwrap();
    let (cursor, _) = session_video_candidates(&session, &model, Some(cursor)).unwrap();
    session.append(result_message()).unwrap();
    let (coalesced, suffix) = session_video_candidates(&session, &model, Some(cursor)).unwrap();
    assert_eq!(cursor.messages, coalesced.messages);
    assert!(matches!(&suffix[0], Message::User(user) if user.content.len() == 2));
    let first_kept = session.append(video_message()).unwrap();
    session
        .compact("earlier videos summarized", first_kept)
        .unwrap();
    let (compacted, active) = session_video_candidates(&session, &model, Some(coalesced)).unwrap();
    assert!(matches!(&active[0], Message::User(user) if user.content.len() == 1));
    session.checkout_root().unwrap();
    session.append(video_message()).unwrap();
    let (_, active) = session_video_candidates(&session, &model, Some(compacted)).unwrap();
    assert!(matches!(&active[0], Message::User(user) if user.content.len() == 1));
}
