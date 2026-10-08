//! Transcript layout caching, scrolled-viewport stability, deferred history, and session resume
//! materialisation. Separate because they assert that replayed history is complete and stays
//! lazy until the reader asks for it.

use super::support::*;

use super::*;

#[test]
fn scrolling_reuses_the_cached_transcript_layout() {
    let mut shell = InteractiveShell::test_shell();
    for number in 0..200 {
        shell.notice(format!("notice {number}"));
    }
    let _ = render_shell(&shell.state.borrow(), 120);
    let first_generation = shell.state.borrow().transcript_cache.borrow().generation;

    shell.scroll_lines(-3);
    let _ = render_shell(&shell.state.borrow(), 120);
    assert_eq!(
        shell.state.borrow().transcript_cache.borrow().generation,
        first_generation,
        "scrolling must only slice the existing layout"
    );

    shell.notice("new transcript block");
    let _ = render_shell(&shell.state.borrow(), 120);
    assert_eq!(
        shell.state.borrow().transcript_cache.borrow().generation,
        first_generation + 1
    );
}

#[test]
fn transcript_cache_reflows_when_width_changes_without_content_changes() {
    let shell = InteractiveShell::test_shell();
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized(
                "Width-sensitive transcript caching must rebuild this line when the terminal shrinks while preserving TAIL-MARKER.".into(),
            ),
        )));

    let wide = shell.state.borrow().rendered_transcript(120).clone();
    let first_generation = shell.state.borrow().transcript_cache.borrow().generation;
    let narrow = shell.state.borrow().rendered_transcript(36).clone();
    let state = shell.state.borrow();
    let cache = state.transcript_cache.borrow();

    assert_eq!(cache.width, Some(36));
    assert_eq!(cache.generation, first_generation + 1);
    assert!(narrow.len() > wide.len(), "wide={wide:?} narrow={narrow:?}");
    assert!(narrow.iter().all(|line| visible_width(line) <= 36));
    assert!(strip_terminal_sequences(&narrow.join("\n")).contains("TAIL-MARKER"));
}

#[test]
fn new_output_does_not_move_a_scrolled_reader_viewport() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 18);
    for number in 0..100 {
        shell.notice(format!("anchor notice {number}"));
    }
    let _ = render_shell(&shell.state.borrow(), 80);
    shell.scroll_lines(-6);
    let before = render_shell(&shell.state.borrow(), 80)
        .into_iter()
        .filter(|line| line.contains("anchor notice"))
        .collect::<Vec<_>>();

    shell.notice("new output while reading");
    let after = render_shell(&shell.state.borrow(), 80)
        .into_iter()
        .filter(|line| line.contains("anchor notice"))
        .collect::<Vec<_>>();
    assert_eq!(after, before);
}

#[test]
fn terminal_native_resume_materializes_complete_history_for_pi_scrollback() {
    let directory = tempfile::tempdir().unwrap();
    let session = session_with_user_prompts(
        &directory.path().join("native-complete-session.jsonl"),
        "native prompt",
        100,
    );
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 12);
    shell.hydrate(&session).unwrap();
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("native prompt 0\n"));
    assert!(snapshot.contains("native prompt 99"));
    assert!(shell.state.borrow().deferred_session_history.is_none());
}

#[test]
fn application_viewport_resume_is_tail_first_and_materializes_when_scrolling_past_it() {
    use octet_agent::EntryValue;
    use octet_ai::{Message, UserMessage, UserPart};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    for index in 0..100 {
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(format!("prompt {index}"))],
            })))
            .unwrap();
    }

    let mut shell = lazy_history_test_shell();
    shell.set_size(80, 12);
    shell.hydrate(&session).unwrap();
    assert!(shell.debug_snapshot().contains("prompt 99"));
    assert!(!shell.debug_snapshot().contains("prompt 0\n"));
    assert!(shell.state.borrow().deferred_session_history.is_some());
    shell.on_local_command_submitted("!local-only command");
    let retained_tail_cursor = {
        let state = shell.state.borrow();
        transcript_commit_cursor(
            &state,
            state.transcript.len().saturating_sub(1),
            FINAL_COMMIT_SEGMENT,
        )
    };

    let page = usize::from(shell.state.borrow().size.1.max(4) / 2);
    let mut crossing_scroll = None;
    for _ in 0..100 {
        let before = shell.state.borrow().scroll_from_bottom.get();
        shell.scroll(-1);
        if shell.state.borrow().deferred_session_history.is_none() {
            crossing_scroll = Some((before, shell.state.borrow().scroll_from_bottom.get()));
            break;
        }
    }
    assert!(shell.state.borrow().deferred_session_history.is_none());
    let (before, after) = crossing_scroll.expect("deferred history crossing");
    assert!(
        after <= before.saturating_add(page),
        "prepending history must advance one page, not jump to oldest: {before} -> {after}"
    );
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("prompt 0\n"));
    assert_eq!(snapshot.matches("!local-only command").count(), 1);
    let remapped_tail_cursor = {
        let state = shell.state.borrow();
        transcript_commit_cursor(
            &state,
            state.transcript.len().saturating_sub(1),
            FINAL_COMMIT_SEGMENT,
        )
    };
    assert_eq!(
        remapped_tail_cursor, retained_tail_cursor,
        "prepending deferred history must preserve retained block identity"
    );
}

#[test]
fn deferred_history_keeps_local_outcome_before_a_later_persisted_prompt() {
    use octet_agent::EntryValue;
    use octet_ai::{Message, UserMessage, UserPart};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interleaved-session.jsonl");
    let mut session = Session::create(&path).unwrap();
    for index in 0..100 {
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(format!("persisted prompt {index}"))],
            })))
            .unwrap();
    }

    let mut shell = lazy_history_test_shell();
    shell.set_size(80, 12);
    shell.hydrate(&session).unwrap();
    assert!(shell.state.borrow().deferred_session_history.is_some());

    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Outcome(OutcomeBlock::new(
            RunOutcome::Completed {
                elapsed: Duration::from_secs(1),
                summary: crate::presentation::RunSummary {
                    files_changed: 0,
                    tool_calls: 0,
                    warnings: 0,
                },
            },
            None,
        )));
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("persisted after local outcome".into())],
        })))
        .unwrap();
    shell.on_prompt_submitted("persisted after local outcome");
    shell.mark_prompt_persisted();
    drop(session);

    assert!(shell.materialize_deferred_history().unwrap());
    let state = shell.state.borrow();
    assert!(
        state
            .transcript_commit_ids
            .windows(2)
            .all(|ids| ids[0] < ids[1]),
        "materialized commit identities must remain strictly ordered: {:?}",
        state.transcript_commit_ids
    );
    let outcome = state
        .transcript
        .iter()
        .position(|block| matches!(block, TranscriptBlock::Outcome(_)))
        .expect("local outcome retained");
    let later_prompt = state
        .transcript
        .iter()
        .position(|block| {
            matches!(
                block,
                TranscriptBlock::User { text, .. } if text == "persisted after local outcome"
            )
        })
        .expect("later persisted prompt hydrated");
    assert!(outcome < later_prompt);
}

#[test]
fn fullscreen_theme_swap_keeps_deferred_history_and_stream_identity() {
    let directory = tempfile::tempdir().unwrap();
    let session = session_with_user_prompts(
        &directory.path().join("active-theme.jsonl"),
        "theme prompt",
        100,
    );
    let (mut shell, bytes) =
        emulated_shell_with_mode(crate::tui::theme::test_theme(), 80, 12, false, true);
    shell.hydrate(&session).unwrap();
    let run_id = shell.begin_run("test");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "before theme ".into(),
        },
    );
    let (index, commit) = {
        let state = shell.state.borrow();
        assert!(state.deferred_session_history.is_some());
        let index = state.active_text.unwrap();
        (index, state.transcript_commit_ids[index])
    };
    shell.set_theme(crate::tui::theme::test_theme_from_source(
        "[colors]\nforeground = '#2040c0'\n",
    ));
    {
        let state = shell.state.borrow();
        assert!(
            state.deferred_session_history.is_some(),
            "theme selection must not synchronously load history"
        );
        assert_eq!(state.active_text, Some(index));
        assert_eq!(state.transcript_commit_ids[index], commit);
        assert!(state.run.is_active());
    }
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "after theme".into(),
        },
    );
    shell.render();
    let mut terminal = vt100::Parser::new(12, 80, 128);
    terminal.process(&bytes.lock().unwrap());
    let screen = terminal.screen().contents();
    assert!(screen.contains("before theme after theme"), "{screen}");
}

#[test]
fn resize_keeps_deferred_history_lazy_during_an_active_stream() {
    const WIDTH: u16 = 80;
    const RESIZED_WIDTH: u16 = 96;
    const HEIGHT: u16 = 12;

    let directory = tempfile::tempdir().unwrap();
    let session = session_with_user_prompts(
        &directory.path().join("active-resize-session.jsonl"),
        "active resize prompt",
        100,
    );
    let (mut shell, bytes) =
        emulated_shell_with_mode(crate::tui::theme::test_theme(), WIDTH, HEIGHT, false, true);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    shell.hydrate(&session).unwrap();
    assert!(shell.state.borrow().deferred_session_history.is_some());

    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "active-stream-before-resize".into(),
        },
    );
    shell.render();
    let _ = drain(&bytes);
    let (active_index_before, active_commit_id) = {
        let state = shell.state.borrow();
        let index = state.active_text.expect("active assistant stream");
        (index, state.transcript_commit_ids[index])
    };

    shell.set_size(RESIZED_WIDTH, HEIGHT);
    let active_index_after = {
        let state = shell.state.borrow();
        assert!(state.deferred_session_history.is_some());
        assert!(state.run.is_active());
        assert!(
            !state.transcript.iter().any(|block| matches!(
                block,
                TranscriptBlock::User { text, .. } if text == "active resize prompt 0"
            )),
            "resize must not materialize deferred history"
        );
        assert!(state
            .transcript_commit_ids
            .windows(2)
            .all(|ids| ids[0] < ids[1]));
        let index = state.active_text.expect("retained assistant stream");
        assert_eq!(index, active_index_before);
        assert_eq!(state.transcript_commit_ids[index], active_commit_id);
        index
    };

    shell.render();
    let resize = String::from_utf8_lossy(&drain(&bytes)).into_owned();
    assert!(resize.contains("\x1b[3J"), "{resize:?}");
    assert!(!resize.contains("active resize prompt 0"), "{resize:?}");
    assert!(resize.contains("active-stream-before-resize"), "{resize:?}");

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "-and-after".into(),
        },
    );
    let state = shell.state.borrow();
    assert_eq!(state.active_text, Some(active_index_after));
    let TranscriptBlock::Assistant(assistant) = &state.transcript[active_index_after] else {
        panic!("active stream must remain an assistant block");
    };
    assert_eq!(
        assistant.text, "active-stream-before-resize-and-after",
        "post-resize deltas must continue the retained live block"
    );
}

#[test]
fn delayed_resize_reconciliation_keeps_deferred_history_lazy() {
    let directory = tempfile::tempdir().unwrap();
    let session = session_with_user_prompts(
        &directory.path().join("reconciled-resize-session.jsonl"),
        "reconciled resize prompt",
        100,
    );
    let mut shell = lazy_history_test_shell();
    shell.set_size(80, 12);
    shell.hydrate(&session).unwrap();
    assert!(shell.state.borrow().deferred_session_history.is_some());
    shell.notice("live block before reconciled resize");
    let live_commit_id = *shell
        .state
        .borrow()
        .transcript_commit_ids
        .last()
        .expect("live block identity");

    assert!(reconcile_terminal_size(&shell.state, &shell.size, (91, 17)));
    let state = shell.state.borrow();
    assert_eq!(state.size, (91, 17));
    assert!(state.deferred_session_history.is_some());
    assert!(!state.transcript.iter().any(|block| matches!(
        block,
        TranscriptBlock::User { text, .. } if text == "reconciled resize prompt 0"
    )));
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptBlock::Notice(text)) if text == "live block before reconciled resize"
    ));
    assert_eq!(state.transcript_commit_ids.last(), Some(&live_commit_id));
}

#[test]
fn deferred_history_identity_failure_is_transactional() {
    let directory = tempfile::tempdir().unwrap();
    let session = session_with_user_prompts(
        &directory.path().join("transactional-history-session.jsonl"),
        "transactional prompt",
        100,
    );
    let mut shell = lazy_history_test_shell();
    shell.set_size(80, 12);
    shell.hydrate(&session).unwrap();
    assert!(shell.state.borrow().deferred_session_history.is_some());
    let run_id = shell.begin_run("openai");
    let tool_id = ToolCallId("transactional-live-tool".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: tool_id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path": "live.rs"}),
        },
    );
    shell.notice("live block survives failed materialization");
    {
        let mut state = shell.state.borrow_mut();
        state
            .deferred_session_history
            .as_mut()
            .expect("deferred history")
            .retained_id_end = 0;
    }
    let before_snapshot = shell.debug_snapshot();
    let (before_len, before_ids, before_revisions, before_tools, before_deferred, before_next_id) = {
        let state = shell.state.borrow();
        (
            state.transcript.len(),
            state.transcript_commit_ids.clone(),
            state.block_revisions.clone(),
            state.tool_panels.clone(),
            state.deferred_session_history.clone(),
            state.next_transcript_commit_id.0,
        )
    };
    assert_eq!(before_tools.get(&tool_id).copied(), Some(before_len - 3));

    let error = shell.materialize_deferred_history().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deferred history exhausted commit identity space"),
        "{error:#}"
    );
    let state = shell.state.borrow();
    assert_eq!(state.transcript.len(), before_len);
    assert_eq!(state.transcript_commit_ids, before_ids);
    assert_eq!(state.block_revisions, before_revisions);
    assert_eq!(state.tool_panels, before_tools);
    assert_eq!(state.deferred_session_history, before_deferred);
    assert_eq!(state.next_transcript_commit_id.0, before_next_id);
    drop(state);
    assert_eq!(shell.debug_snapshot(), before_snapshot);
}

#[test]
fn resumed_session_restores_every_write_as_a_diff_panel() {
    use octet_agent::EntryValue;
    use octet_ai::{
        AssistantMessage, AssistantPart, Message, Protocol, ToolCall, ToolResult, ToolResultPart,
        UserMessage, UserPart,
    };

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("write both files".into())],
        })))
        .unwrap();

    let writes = [
        (
            "write-current",
            "new.rs",
            "ok\nnew.rs  created hash=x\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,1 @@\n+current format\n",
        ),
        (
            "write-legacy",
            "legacy.rs",
            "ok\nlegacy.rs  created hash=y\n--- /dev/null\n+++ b/legacy.rs\n+legacy format\n",
        ),
    ];
    for (id, path, result) in writes {
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId(id.into()),
                    name: "write".into(),
                    arguments_json: serde_json::json!({
                        "path": path,
                        "content": format!("{path} contents\n"),
                    })
                    .to_string(),
                    argument_error: None,
                })],
                model: ModelId("gpt-5.6-sol".into()),
                protocol: Protocol::OpenAiResponses,
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId(id.into()),
                    content: vec![ToolResultPart::Text(result.into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            })))
            .unwrap();
    }

    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 40);
    shell.hydrate(&session).unwrap();
    let rendered = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"));

    assert!(rendered.contains("current format"), "{rendered}");
    assert!(rendered.contains("legacy format"), "{rendered}");
    assert!(
        rendered.contains("new.rs") && rendered.contains("legacy.rs"),
        "{rendered}"
    );
    shell.set_verbose_tools(true);
    let expanded = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"));
    assert!(expanded.matches("/dev/null").count() >= 2, "{expanded}");
}

#[test]
fn duplicate_hydrated_tool_call_ids_never_leave_a_running_card() {
    use octet_ai::{
        AssistantMessage, AssistantPart, Message, Protocol, ToolCall, ToolResult, ToolResultPart,
        UserMessage, UserPart,
    };

    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![
                AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("duplicate".into()),
                    name: "read".into(),
                    arguments_json: r#"{"path":"first"}"#.into(),
                    argument_error: None,
                }),
                AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("duplicate".into()),
                    name: "read".into(),
                    arguments_json: r#"{"path":"second"}"#.into(),
                    argument_error: None,
                }),
            ],
            model: ModelId("test".into()),
            protocol: Protocol::OpenAiChat,
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId("duplicate".into()),
                content: vec![ToolResultPart::Text("durable result".into())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.hydrate(&session).unwrap();
    let state = shell.state.borrow();
    let panels = state
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Tool(panel) => Some(panel.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(panels.len(), 2);
    assert!(
        panels.iter().all(|panel| panel.finished),
        "duplicate recovered IDs must never revive a running card: {panels:?}"
    );
    assert!(panels.iter().any(|panel| panel.is_error));
    assert!(panels.iter().any(|panel| !panel.is_error));
}
