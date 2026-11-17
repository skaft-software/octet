//! Exercise the foreground result-commit poll with a real read and a slow decoder.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct VideoReadTransport;

#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for VideoReadTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        _: Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, AiError> {
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::ToolCall(ToolCall {
                        id: octet_ai::ToolCallId("video-read".into()),
                        name: "read".into(),
                        arguments_json: r#"{"path":"clip.mp4"}"#.into(),
                        argument_error: None,
                        async_execution: false,
                    })],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::ToolUse,
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
        agent.client.register_host_stream_transport(agent.model.endpoint.id.clone(), Arc::new(VideoReadTransport));
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
        assert!(result_text.contains(if aborting { "cancelled" } else { "decoder rejected" }));
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
        assert!(video.prepare_frames().await.is_err());
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
    assert_eq!(
        agent.prepared_video_entries,
        agent.session().entries().len() - 1
    );
}
