use super::*;

#[test]
fn clipping_preserves_utf8_boundaries() {
    let clipped = clip_text("abc🙂def", 5);
    assert!(clipped.starts_with("abc"));
    assert!(!clipped.contains('�'));
    assert!(clipped.contains("bytes omitted"));
}

#[tokio::test]
async fn terminal_events_preserve_outcomes_head_and_run_summary_without_an_app() {
    for (reason, expected) in [
        (
            octet_agent::FinishReason::Completed,
            HostRunOutcome::Completed,
        ),
        (octet_agent::FinishReason::Aborted, HostRunOutcome::Aborted),
        (
            octet_agent::FinishReason::MaxTurns,
            HostRunOutcome::MaxTurns,
        ),
    ] {
        let mut state = EventState {
            final_output: "committed answer".into(),
            tool_calls: 2,
            steps: 3,
            files_changed: BTreeSet::from(["changed.rs".into()]),
            terminal_head: Some("previous-head".into()),
            ..EventState::default()
        };
        // RunFinished returns the outcome without writing protocol events;
        // orchestration still owns settlement and final_result emission.
        let mut output = tokio::io::stdout();
        let mut emitter = Emitter::new(&mut output, "terminal-test".into());
        let outcome = translate(
            AgentEvent::RunFinished {
                head: octet_agent::EntryId("terminal-head".into()),
                reason,
            },
            &mut state,
            &mut emitter,
            "endpoint-for-diagnostic",
            "model-for-diagnostic",
        )
        .await
        .unwrap();

        assert_eq!(outcome, Some(expected));
        assert_eq!(state.terminal_head.as_deref(), Some("terminal-head"));
        assert_eq!(state.final_output, "committed answer");
        assert_eq!(state.tool_calls, 2);
        assert_eq!(state.steps, 3);
        assert_eq!(state.files_changed, BTreeSet::from(["changed.rs".into()]));
        assert!(!emitter.is_terminal());
    }
}

#[tokio::test]
async fn terminal_failure_uses_supplied_diagnostic_identifiers_without_an_app() {
    let error = octet_agent::AgentError::IncompleteResponse {
        stop_reason: "length".into(),
    };
    let expected = octet_agent::public_error_diagnostic(
        &error,
        "endpoint-for-diagnostic",
        "model-for-diagnostic",
    );
    assert!(expected.contains("provider=endpoint-for-diagnostic"));
    assert!(expected.contains("model=model-for-diagnostic"));
    let mut state = EventState::default();
    let mut output = tokio::io::stdout();
    let mut emitter = Emitter::new(&mut output, "failure-test".into());
    let outcome = translate(
        AgentEvent::RunFinished {
            head: octet_agent::EntryId("failed-head".into()),
            reason: octet_agent::FinishReason::Failed(error),
        },
        &mut state,
        &mut emitter,
        "endpoint-for-diagnostic",
        "model-for-diagnostic",
    )
    .await
    .unwrap();

    assert_eq!(outcome, Some(HostRunOutcome::Failed(expected)));
    assert_eq!(state.terminal_head.as_deref(), Some("failed-head"));
    assert!(!emitter.is_terminal());
}
