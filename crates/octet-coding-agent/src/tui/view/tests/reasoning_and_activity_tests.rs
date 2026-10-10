//! Reasoning blocks, the thinking header, the run-phase lifecycle, and provider lifecycle
//! status. Separate because they own the reasoning disclosure and the run status machine.

use super::*;

#[test]
fn reasoning_heading_moves_below_the_fixed_thinking_header() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "## Verifying reproducibility of evidence package\n\n".into(),
        },
    );

    let rendered = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "",
            "• Thinking (0s • esc to interrupt)",
            "  └ Verifying reproducibility of evidence package (ctrl+o to expand)",
        ]
    );
}

#[test]
fn still_working_to_thinking_keeps_the_transcript_height_stable() {
    let theme = crate::tui::theme::test_theme_from_source(include_str!(
        "../../../../../../examples/themes/Still.toml"
    ));
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.set_size(160, 24);
    let run_id = shell.begin_run("openai");
    let rows = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(160)
            .iter()
            .map(|row| strip_terminal_sequences(row).to_owned())
            .collect::<Vec<_>>()
    };
    let working = rows(&shell);
    assert!(
        working[working.len() - 2].contains("Working"),
        "{working:?}"
    );
    assert!(working.last().is_some_and(String::is_empty), "{working:?}");

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "private detail".into(),
        },
    );
    let thinking = rows(&shell);
    assert_eq!(
        thinking.len(),
        working.len(),
        "status promotion shifted the composer"
    );
    assert!(
        thinking[thinking.len() - 2].contains("Thinking"),
        "{thinking:?}"
    );
    assert!(
        thinking
            .last()
            .is_some_and(|row| row.contains("ctrl+o to expand")),
        "{thinking:?}"
    );

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "Answer".into(),
        },
    );
    let responding = rows(&shell);
    assert!(
        responding.len() >= thinking.len(),
        "response shrank the transcript: {responding:?}"
    );
    assert!(
        responding[responding.len() - 2].contains("Working"),
        "{responding:?}"
    );
    assert!(
        responding.last().is_some_and(String::is_empty),
        "{responding:?}"
    );

    let tool_id = ToolCallId("still-tool".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: tool_id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        },
    );
    let tool = rows(&shell);
    assert!(
        tool.len() >= responding.len(),
        "tool replaced status with fewer rows: {tool:?}"
    );
    assert!(tool.last().is_some_and(String::is_empty), "{tool:?}");

    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: tool_id,
            result: Ok(octet_agent::ToolOutput::new("done")),
            duration: Duration::from_millis(10),
        },
    );
    let finished = rows(&shell);
    assert!(
        finished.len() >= tool.len(),
        "tool completion shrank the transcript: {finished:?}"
    );

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "another private detail".into(),
        },
    );
    let thinking_again = rows(&shell);
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: ToolCallId("next-tool".into()),
            name: "read".into(),
            args: serde_json::json!({"path": "Cargo.toml"}),
        },
    );
    let next_tool = rows(&shell);
    assert!(
        next_tool.len() >= thinking_again.len(),
        "tool replaced Thinking with fewer rows: before={thinking_again:?}, after={next_tool:?}"
    );
}

#[test]
fn working_status_persists_below_running_tools_in_default_and_still() {
    use octet_agent::{EntryId, FinishReason};

    for theme in [
        crate::tui::theme::test_theme(),
        crate::tui::theme::test_theme_from_source(include_str!(
            "../../../../../../examples/themes/Still.toml"
        )),
    ] {
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        let run_id = shell.begin_run("openai");
        let first = ToolCallId("long-bash".into());
        let second = ToolCallId("overlapping-read".into());
        let rows = |shell: &InteractiveShell| {
            shell
                .state
                .borrow()
                .rendered_transcript(100)
                .iter()
                .map(|row| strip_terminal_sequences(row).to_owned())
                .collect::<Vec<_>>()
        };
        let assert_working_tail = |shell: &InteractiveShell| {
            let rendered = rows(shell);
            assert_eq!(
                rendered
                    .iter()
                    .filter(|row| row.contains("Working ("))
                    .count(),
                1,
                "{rendered:?}"
            );
            assert!(
                rendered
                    .iter()
                    .rev()
                    .take(2)
                    .any(|row| row.contains("Working (")),
                "status must remain at the bottom: {rendered:?}"
            );
            let state = shell.state.borrow();
            assert!(state.has_active_status_shimmer());
            assert!(state.has_active_status_timer());
        };

        shell.on_run_event(
            run_id,
            &AgentEvent::ToolStarted {
                id: first.clone(),
                name: "bash".into(),
                args: serde_json::json!({"command": "sleep 30"}),
            },
        );
        assert_working_tail(&shell);
        {
            let mut state = shell.state.borrow_mut();
            let frame = state.status_shimmer_frame;
            state.advance_status_shimmer();
            assert_eq!(state.status_shimmer_frame, frame + 1);
        }
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolProgress {
                id: first.clone(),
                progress: ToolProgress::Status("still running".into()),
            },
        );
        assert_working_tail(&shell);
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolStarted {
                id: second.clone(),
                name: "read".into(),
                args: serde_json::json!({"path": "README.md"}),
            },
        );
        assert_working_tail(&shell);
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolFinished {
                id: first,
                result: Ok(octet_agent::ToolOutput::new("done")),
                duration: Duration::from_secs(1),
            },
        );
        assert_working_tail(&shell);
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolFinished {
                id: second,
                result: Ok(octet_agent::ToolOutput::new("done")),
                duration: Duration::from_secs(1),
            },
        );
        assert_working_tail(&shell);
        shell.on_run_event(
            run_id,
            &AgentEvent::RunFinished {
                head: EntryId("head".into()),
                reason: FinishReason::Completed,
            },
        );
        assert!(!rows(&shell).iter().any(|row| row.contains("Working (")));
        assert!(!shell.state.borrow().has_active_status_shimmer());
    }
}

#[test]
fn reasoning_off_run_uses_a_truthful_non_expandable_working_status() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "off");
    let run_id = shell.begin_run("codex");
    let rendered = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert_eq!(rendered.len(), 3, "{rendered:?}");
    assert!(rendered[0].is_empty(), "{rendered:?}");
    assert!(
        rendered[1].starts_with("• Working (0s • esc to interrupt)"),
        "{rendered:?}"
    );
    assert!(rendered[2].is_empty(), "{rendered:?}");
    assert!(!rendered[1].contains("ctrl+o"), "{rendered:?}");

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "provider-private detail".into(),
        },
    );
    let promoted = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert_eq!(
        promoted.len(),
        rendered.len(),
        "promotion moved the composer"
    );
    assert!(promoted[0].is_empty(), "{promoted:?}");
    assert!(promoted[1].contains("Thinking"), "{promoted:?}");
    assert!(promoted[1].contains("Ctrl+O expand"), "{promoted:?}");
    assert!(promoted[2].is_empty(), "{promoted:?}");
    assert!(!promoted.join("\n").contains("provider-private detail"));
}

#[test]
fn empty_working_status_leaves_no_ghost_block_when_interrupted() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "off");
    let run_id = shell.begin_run("codex");

    shell.interrupt_run(run_id);

    let state = shell.state.borrow();
    assert!(
        state
            .transcript
            .iter()
            .all(|block| !matches!(block, TranscriptBlock::Reasoning(_))),
        "a display-only status must not become durable transcript history"
    );
    assert!(state.active_event_blocks.is_empty());
}

#[test]
fn public_text_stream_keeps_exactly_one_working_row_while_the_run_is_active() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    let run_id = shell.begin_run("codex");

    for text in ["Ready.", " Still running."] {
        shell.on_run_event(
            run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: text.into(),
            },
        );
    }

    let state = shell.state.borrow();
    assert!(matches!(
        state.transcript.first(),
        Some(TranscriptBlock::Assistant(_))
    ));
    assert_eq!(state.transcript.len(), 2);
    assert!(state.active_text.is_some());
    assert!(state.active_reasoning.is_some());
    assert!(state.has_active_status_shimmer());
    let rendered = state
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert!(rendered
        .iter()
        .any(|line| line.contains("Ready. Still running.")));
    assert_eq!(
        rendered
            .iter()
            .filter(|line| line.starts_with("• Working ("))
            .count(),
        1,
        "{rendered:?}"
    );
    let working = rendered
        .iter()
        .position(|line| line.starts_with("• Working ("))
        .unwrap();
    assert_eq!(working + 2, rendered.len(), "{rendered:?}");
    assert!(rendered[working - 1].is_empty(), "{rendered:?}");
    assert!(rendered[working + 1].is_empty(), "{rendered:?}");
}

#[test]
fn activity_lifecycle_is_working_thinking_streaming_working_then_settled() {
    use octet_agent::{EntryId, FinishReason};
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol, StopReason};

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    let rendered = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
    };
    assert!(rendered(&shell)
        .iter()
        .any(|line| line.starts_with("• Working (")));

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "real private trace".into(),
        },
    );
    let thinking = rendered(&shell);
    assert!(thinking.iter().any(|line| line.contains("Thinking")));
    assert!(!thinking.iter().any(|line| line.starts_with("• Working (")));

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "Answer".into(),
        },
    );
    let responding = rendered(&shell);
    assert!(responding.iter().any(|line| line.contains("Answer")));
    assert_eq!(
        responding
            .iter()
            .filter(|line| line.starts_with("• Working ("))
            .count(),
        1,
        "{responding:?}"
    );
    let working = responding
        .iter()
        .position(|line| line.starts_with("• Working ("))
        .unwrap();
    assert_eq!(working + 2, responding.len(), "{responding:?}");
    assert!(responding[working - 1].is_empty(), "{responding:?}");
    assert!(responding[working + 1].is_empty(), "{responding:?}");
    assert!(shell.state.borrow().has_active_status_shimmer());

    shell.on_run_event(
        run_id,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: AssistantMessage {
                content: vec![AssistantPart::Text("Answer".into())],
                model: ModelId("m".into()),
                protocol: Protocol::OpenAiResponses,
            },
            stop_reason: StopReason::EndTurn,
            turn_usage: Usage::default(),
            usage: Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        },
    );
    let finalizing = rendered(&shell);
    assert_eq!(
        finalizing
            .iter()
            .filter(|line| line.starts_with("• Working ("))
            .count(),
        1,
        "a completed turn is not an authoritative run terminal: {finalizing:?}"
    );

    shell.on_run_event(
        run_id,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );
    let settled = rendered(&shell);
    assert!(!settled.iter().any(|line| line.contains("Working")));
    assert!(shell.state.borrow().active_reasoning.is_none());
}

#[test]
fn provider_lifecycle_status_is_transient_and_cannot_regress_output() {
    use octet_agent::{EntryId, FinishReason};
    use octet_ai::{ProviderLifecycle, ProviderLifecycleState};

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("custom-openai");
    shell.set_awaiting_provider(run_id);
    let lifecycle = |state| AgentEvent::ProviderLifecycle {
        lifecycle: ProviderLifecycle {
            state,
            detail: Some("warming".into()),
        },
    };
    let rendered = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
    };

    shell.on_run_event(run_id, &lifecycle(ProviderLifecycleState::Loading));
    assert!(rendered(&shell)
        .iter()
        .any(|line| line.contains("Loading local endpoint · warming")));

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "Answer".into(),
        },
    );
    // A delayed readiness comment is advisory only; it cannot replace a
    // streaming response's trailing generic liveness row.
    shell.on_run_event(run_id, &lifecycle(ProviderLifecycleState::Ready));
    let after_output = rendered(&shell);
    assert!(after_output.iter().any(|line| line.contains("Answer")));
    assert!(after_output
        .iter()
        .any(|line| line.starts_with("• Working (")));
    assert!(!after_output
        .iter()
        .any(|line| line.contains("ready · warming")));

    shell.on_run_event(
        run_id,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );
    assert!(!rendered(&shell)
        .iter()
        .any(|line| line.contains("Loading local endpoint")));
}

#[test]
fn removing_a_tail_status_preserves_an_older_semantic_selection() {
    let mut shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Notice("older transcript".into()));
        state.transcript_selection = Some(TranscriptSelection {
            anchor: TranscriptPosition {
                block: 0,
                offset: 0,
                trailing_affinity: false,
            },
            focus: TranscriptPosition {
                block: 0,
                offset: 5,
                trailing_affinity: false,
            },
        });
    }
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    let run_id = shell.begin_run("codex");

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "Ready.".into(),
        },
    );

    let state = shell.state.borrow();
    let selection = state
        .transcript_selection
        .as_ref()
        .expect("older selection should survive removal of the empty tail status");
    assert_eq!(selection.anchor.block, 0);
    assert_eq!(selection.focus.block, 0);
}

#[test]
fn reasoning_status_reopens_after_tools_for_the_next_model_turn() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("codex", "gpt-5.3-codex-spark", "high");
    shell.set_context_estimate(13_000, 128_000);
    let run_id = shell.begin_run("codex");
    let id = ToolCallId("tool-1".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        },
    );
    assert!(matches!(
        shell.state.borrow().transcript.last(),
        Some(TranscriptBlock::Reasoning(reasoning)) if reasoning.is_working_activity()
    ));
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id,
            result: Ok(octet_agent::ToolOutput::new("x".repeat(4_000))),
            duration: Duration::from_millis(10),
        },
    );
    let state = shell.state.borrow();
    let index = state.active_reasoning.expect("next-turn reasoning status");
    let TranscriptBlock::Reasoning(reasoning) = &state.transcript[index] else {
        panic!("reasoning status expected")
    };
    assert!(reasoning.text.is_empty());
    assert!(!reasoning.finished);
    assert_eq!(state.run_context_estimate, Some((14_008, 128_000)));
    assert_eq!(state.context_estimate, Some((14_008, 128_000)));
}

#[test]
fn streamed_reasoning_shows_one_live_indicator_until_ctrl_o() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "first private sentinel".into(),
        },
    );
    let transcript = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
    };
    let initial = transcript(&shell);
    assert_eq!(initial.len(), 3, "{initial:?}");
    assert!(initial[0].is_empty(), "{initial:?}");
    assert!(initial[1].contains("Thinking"), "{initial:?}");
    assert!(initial[1].contains("Ctrl+O expand"), "{initial:?}");
    assert!(initial[2].is_empty(), "{initial:?}");
    assert!(!initial.join("\n").contains("first private sentinel"));

    let continuation = (0..128)
        .map(|index| format!("\nprivate reasoning row {index}"))
        .collect::<String>();
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: continuation.clone(),
        },
    );
    let after_stream = transcript(&shell);
    assert_eq!(
        after_stream, initial,
        "hidden deltas changed transcript geometry"
    );
    {
        let state = shell.state.borrow();
        let TranscriptBlock::Reasoning(reasoning) = &state.transcript[0] else {
            panic!("reasoning block expected");
        };
        assert_eq!(
            reasoning.text,
            format!("first private sentinel{continuation}")
        );
    }

    shell.toggle_disclosure();
    let expanded = transcript(&shell).join("\n");
    assert!(expanded.contains("first private sentinel"), "{expanded}");
    assert!(expanded.contains("private reasoning row 127"), "{expanded}");
    assert!(!expanded.contains("Ctrl+O expand"), "{expanded}");

    shell.toggle_disclosure();
    assert_eq!(transcript(&shell), initial);
}

#[test]
fn a_new_reasoning_event_retires_the_previous_ctrl_o_hint() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "first thought".into(),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "answer".into(),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "second thought".into(),
        },
    );
    let rendered = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(rendered.matches("Ctrl+O expand").count(), 1, "{rendered}");
    shell.toggle_disclosure();
    let expanded = shell
        .state
        .borrow()
        .rendered_transcript(80)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded.contains("first thought"), "{expanded}");
    assert!(expanded.contains("second thought"), "{expanded}");
    assert!(!expanded.contains("Ctrl+O expand"), "{expanded}");
}

#[test]
fn reasoning_heading_tracks_only_explicit_markdown_headings() {
    let mut reasoning = AssistantBlock::streaming_reasoning(
        "Body sentence must stay private.\n\n## Plan `carefully`\n\nMore private detail.",
    );
    assert_eq!(
        reasoning.reasoning_heading.as_deref(),
        Some("Plan carefully")
    );

    reasoning.append_reasoning("\n\nThis is still ordinary body text.");
    assert_eq!(
        reasoning.reasoning_heading.as_deref(),
        Some("Plan carefully")
    );

    reasoning.append_reasoning("\n\n**Verify results**");
    assert_eq!(
        reasoning.reasoning_heading.as_deref(),
        Some("Verify results")
    );

    reasoning.append_reasoning("\n\nPrefix **bold body text** suffix.");
    assert_eq!(
        reasoning.reasoning_heading.as_deref(),
        Some("Verify results")
    );
}

#[test]
fn reasoning_heading_handles_adjacent_bold_sections_split_across_deltas() {
    let mut reasoning = AssistantBlock::streaming_reasoning("**Plan**");
    assert_eq!(reasoning.reasoning_heading.as_deref(), Some("Plan"));

    reasoning.append_reasoning("**");
    assert_eq!(reasoning.reasoning_heading.as_deref(), Some("Plan"));
    reasoning.append_reasoning("Verify**");
    assert_eq!(reasoning.reasoning_heading.as_deref(), Some("Verify"));
    assert_eq!(reasoning.text, "**Plan****Verify**");
    assert!(!reasoning.markdown.raw_text().contains("****"));
}

#[test]
fn reasoning_heading_is_terminal_sanitized() {
    let heading = reasoning_heading_from_block(&Block::Heading {
        level: 2,
        content: vec![Inline::Text("Safe\x1b[31m heading\x07".into())],
    })
    .expect("heading");
    assert_eq!(heading, "Safe heading␇");
    assert!(!heading.contains('\x1b'));
}

#[test]
fn hydrated_reasoning_is_retained_but_collapsed_until_ctrl_o() {
    use octet_ai::{AssistantMessage, AssistantPart, Message, ModelId, Protocol, ReasoningPart};

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let source = "durable private thought\nwith a second line";
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Reasoning(ReasoningPart {
                text: Some(source.into()),
                state: None,
            })],
            model: ModelId("test".into()),
            protocol: Protocol::OpenAiResponses,
        })))
        .unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.hydrate(&session).unwrap();
    let render = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(80)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
    };
    let collapsed = render(&shell);
    assert_eq!(collapsed.len(), 0, "{collapsed:?}");
    assert!(!collapsed.join("\n").contains(source));
    let state = shell.state.borrow();
    let TranscriptBlock::Reasoning(reasoning) = &state.transcript[0] else {
        panic!("hydrated reasoning block expected");
    };
    assert_eq!(reasoning.text, source);
    assert!(reasoning.finished);
    drop(state);

    shell.toggle_disclosure();
    let expanded = render(&shell).join("\n");
    assert!(expanded.contains("durable private thought"), "{expanded}");
    assert!(expanded.contains("with a second line"), "{expanded}");

    shell.toggle_disclosure();
    assert_eq!(render(&shell), collapsed);
}

#[test]
fn completed_reasoning_uses_rich_markdown_without_raw_delimiters() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "**Planning validation**".into(),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "**Inspecting `render.rs`**".into(),
        },
    );
    let collapsed = render_shell(&shell.state.borrow(), 80).join("\n");
    let collapsed_plain = strip_terminal_sequences(&collapsed);
    assert!(
        collapsed_plain.contains("Inspecting render.rs"),
        "{collapsed_plain}"
    );
    assert!(
        !collapsed_plain.contains("Planning validation"),
        "{collapsed_plain}"
    );
    {
        let state = shell.state.borrow();
        let TranscriptBlock::Reasoning(reasoning) = &state.transcript[0] else {
            panic!("first block must be reasoning Markdown");
        };
        assert!(reasoning.text.contains("****"));
        assert!(!reasoning.markdown.raw_text().contains("****"));
    }
    shell.toggle_disclosure();
    let live = render_shell(&shell.state.borrow(), 80).join("\n");
    assert!(live.contains("Planning validation"), "{live}");
    assert!(live.contains("Inspecting"), "{live}");
    assert!(!live.contains("**"), "{live}");
    assert!(!live.contains("`render.rs`"), "{live}");

    // A tool boundary finalizes both assistant and reasoning streams.
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: ToolCallId("read-1".into()),
            name: "read".into(),
            args: serde_json::json!({"path":"render.rs"}),
        },
    );

    let state = shell.state.borrow();
    let TranscriptBlock::Reasoning(reasoning) = &state.transcript[0] else {
        panic!("first block must be reasoning Markdown");
    };
    assert!(reasoning.markdown.is_finished());
    let rendered = render_shell(&state, 80).join("\n");
    assert!(rendered.contains("Planning validation"), "{rendered}");
    assert!(rendered.contains("Inspecting"), "{rendered}");
    assert!(!rendered.contains("**"), "{rendered}");
    assert!(!rendered.contains("`render.rs`"), "{rendered}");
}

#[test]
fn reasoning_is_subdued_without_losing_inline_code_colour() {
    let theme = crate::tui::theme::test_theme();
    let response = AssistantBlock::finalized("Answer with `Session`".into())
        .render(&theme.rich_renderer(), &theme, 80)
        .join("\n");
    let reasoning = AssistantBlock::finalized_reasoning("Thinking about `Session`".into())
        .render(&theme.reasoning_renderer(), &theme, 80)
        .join("\n");
    let prompt = render_block(
        None,
        &TranscriptBlock::User {
            text: "prompt".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        },
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        false,
    )
    .into_iter()
    .next()
    .expect("prompt line");
    let reasoning_block = render_block(
        None,
        &TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::streaming_reasoning("Thinking about `Session`")
                .with_model_lab(Some(ModelLab::Unknown)),
        )),
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        true,
    )
    .into_iter()
    .next()
    .expect("thinking line");
    let reasoning_code = AssistantBlock::finalized_reasoning(
        "Thinking before code:\n\n```rust\nlet answer = 42;\n```".into(),
    )
    .render(&theme.reasoning_renderer(), &theme, 80)
    .join("\n");
    let linked_reasoning =
        AssistantBlock::finalized_reasoning("See [the docs](https://example.com)".into())
            .render(&theme.reasoning_renderer(), &theme, 80)
            .join("\n");
    let conservative_theme =
        crate::tui::theme::test_theme_with(crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::Ansi16,
        ));
    let conservative_reasoning = AssistantBlock::finalized_reasoning("thinking".into())
        .render(
            &conservative_theme.reasoning_renderer(),
            &conservative_theme,
            80,
        )
        .join("\n");
    let code_line = reasoning_code
        .lines()
        .find(|line| line.contains("answer"))
        .expect("thinking code line");

    assert!(
        response.starts_with("Answer"),
        "responses stay flush: {response:?}"
    );
    assert!(
        strip_terminal_sequences(&prompt).starts_with("› prompt"),
        "prompts should share the presentation inset: {prompt:?}"
    );
    assert!(
        !prompt.contains("\x1b[48;"),
        "prompt identity should use only a restrained foreground marker"
    );
    assert!(
        prompt.contains("\x1b[38;2;"),
        "prompt needs readable text colour"
    );
    assert!(response.contains("Session"));
    assert!(
        response.contains("\x1b[38;2;"),
        "inline code should be coloured"
    );
    assert!(
        strip_terminal_sequences(&reasoning_block).starts_with("  Thinking"),
        "expanded reasoning keeps the transcript inset without a status dot or content bullet: {reasoning_block:?}"
    );
    assert!(reasoning.contains("Session"));
    assert!(
        reasoning.contains("\x1b[38;2;"),
        "reasoning should use a muted foreground"
    );
    assert!(
        !reasoning.contains("\x1b[3m"),
        "reasoning should stay upright"
    );
    assert!(
        !reasoning.contains("\x1b[2m"),
        "reasoning must not use SGR faint"
    );
    assert!(
        !code_line.contains("\x1b[3m"),
        "thinking code blocks must stay upright"
    );
    assert!(
        linked_reasoning.contains("\x1b]8;;https://example.com"),
        "thinking links retain native hyperlink support"
    );
    assert!(
        !conservative_reasoning.contains("\x1b[4m"),
        "unsupported italics must not degrade into underlines"
    );
    assert!(
        !response.contains("\x1b[2m"),
        "ordinary response prose must not inherit reasoning dim"
    );
}
