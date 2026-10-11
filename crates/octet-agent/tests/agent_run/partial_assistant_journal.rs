//! Terminal journal settlement must not turn successful output into recovery.

use super::*;

fn thinking_answer(reasoning: &str, answer: &str) -> String {
    // Repeated requests must carry valid opaque continuation state. The generic
    // one-turn thinking fixture has no signature and cannot be replayed.
    let thinking = frame(
        "content_block_start",
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": ""}
        }),
    ) + &frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": reasoning}
        }),
    ) + &frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "signature_delta", "signature": "mock-opaque-signature"}
        }),
    ) + &frame(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": 0}),
    );
    msg_start() + &thinking + &text_block(1, &[answer]) + &msg_end("end_turn")
}

#[tokio::test]
async fn successful_reasoning_turns_never_recover_previous_output_in_process_or_after_reopen() {
    let server = MockServer::start().await;
    let bodies = (0..6)
        .map(|turn| thinking_answer(&format!("REASONING-{turn}"), &format!("ANSWER-{turn}")))
        .collect();
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let session_path = directory.path().join("successful.jsonl");
    let journal_path = directory
        .path()
        .join("successful.jsonl.partial-assistant-frames");
    for pass in 0..2 {
        let session = if pass == 0 {
            Session::create(&session_path).unwrap()
        } else {
            Session::open(&session_path).unwrap()
        };
        let mut agent = build_agent_from_session_with_model(
            scripted_model_with_reasoning(&server.uri()),
            workspace.path(),
            session,
            ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
            Some(4),
        );
        for turn in (pass * 3)..(pass * 3 + 3) {
            let mut run = agent.prompt(format!("prompt {turn}")).await.unwrap();
            let events = collect(&mut run).await;
            drop(run);
            assert!(
                matches!(assert_single_run_finished(&events), FinishReason::Completed),
                "successful turn {turn}: {events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::RecoveredOutput { .. })),
                "successful turn {turn} must not recover any previous output: {events:?}"
            );
            let reasoning: String = events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::OutputDelta {
                        channel: OutputChannel::Reasoning,
                        text,
                    } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let answer: String = events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::OutputDelta {
                        channel: OutputChannel::Text,
                        text,
                    } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(reasoning, format!("REASONING-{turn}"));
            assert_eq!(answer, format!("ANSWER-{turn}"));
            assert!(
                !journal_path.exists(),
                "settlement must remove the journal before the agent is dropped"
            );
        }
    }
    assert_eq!(wire_requests(&server).await.len(), 6);
}

#[tokio::test]
async fn interrupted_reasoning_is_recovered_once_without_relabeling_the_next_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                thinking_answer("INTERRUPTED-REASONING", "UNSEEN-ANSWER"),
                thinking_answer("CURRENT-REASONING", "CURRENT-ANSWER"),
                thinking_answer("NEXT-REASONING", "NEXT-ANSWER"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let session_path = directory.path().join("interrupted.jsonl");
    let journal_path = directory
        .path()
        .join("interrupted.jsonl.partial-assistant-frames");
    let model = scripted_model_with_reasoning(&server.uri());
    let mut first = build_agent_from_session_with_model(
        model.clone(),
        workspace.path(),
        Session::create(&session_path).unwrap(),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        Some(4),
    );
    let mut run = first.prompt("interrupt while thinking").await.unwrap();
    let mut interrupted = String::new();
    while let Some(event) = run.next().await {
        if let AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text,
        } = event
        {
            interrupted.push_str(&text);
            break;
        }
    }
    drop(run);
    drop(first);
    assert_eq!(interrupted, "INTERRUPTED-REASONING");
    assert!(journal_path.exists(), "interruption preserves recovery");

    let mut agent = build_agent_from_session_with_model(
        model,
        workspace.path(),
        Session::open(&session_path).unwrap(),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        Some(4),
    );
    for (turn, expected_answer) in ["CURRENT-ANSWER", "NEXT-ANSWER"].iter().enumerate() {
        let mut run = agent.prompt(format!("resume {turn}")).await.unwrap();
        let events = collect(&mut run).await;
        drop(run);
        assert!(
            matches!(assert_single_run_finished(&events), FinishReason::Completed),
            "resumed turn {turn}: {events:?}"
        );
        let recovered: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::RecoveredOutput { channel, text } => Some((*channel, text.as_str())),
                _ => None,
            })
            .collect();
        if turn == 0 {
            assert_eq!(
                recovered,
                vec![(OutputChannel::Reasoning, interrupted.as_str())]
            );
        } else {
            assert!(recovered.is_empty(), "a success must not become recovery");
        }
        let answer: String = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(answer, *expected_answer);
        assert!(!journal_path.exists());
    }
    assert_eq!(wire_requests(&server).await.len(), 3);
}
