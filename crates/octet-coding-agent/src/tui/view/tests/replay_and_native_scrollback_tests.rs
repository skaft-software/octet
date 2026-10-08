//! Commit seams, resize and overlay replays, and native scrollback admission for streamed
//! tables, rich paragraphs, and closing code fences. Separate because they assert the full-frame
//! replay contract against the real shell renderer.

use super::support::*;

use super::*;

#[test]
fn removed_streaming_tail_keeps_a_tombstoned_commit_seam() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(64, 10);
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "first finalized paragraph\n\nreplacement remains mutable".into(),
        },
    );
    let _ = render_shell(&shell.state.borrow(), 64);

    let (active_index, retained_cursor) = {
        let state = shell.state.borrow();
        let active_index = state.active_text.expect("streaming assistant block");
        let TranscriptBlock::Assistant(assistant) = &state.transcript[active_index] else {
            panic!("active text must be an assistant block");
        };
        assert!(!assistant.layout.borrow().committed_block_ends().is_empty());
        (
            active_index,
            transcript_commit_cursor(&state, active_index, 0),
        )
    };

    shell.state.borrow_mut().discard_streaming_blocks();
    let _ = render_shell(&shell.state.borrow(), 64);
    let tombstone = transcript_commit_position(&shell.state.borrow(), retained_cursor)
        .expect("removed commit cursor should map to its insertion seam");
    assert_eq!(tombstone.cursor, retained_cursor);

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "retry output\n\nnext block".into(),
        },
    );
    let state = shell.state.borrow();
    assert_eq!(state.active_text, Some(active_index));
    let retry_cursor = transcript_commit_cursor(&state, active_index, 0);
    assert!(retry_cursor > retained_cursor);
}

#[test]
fn resize_matches_pi_clear_and_complete_replay_semantics() {
    const WIDTH: u16 = 48;
    const RESIZED_WIDTH: u16 = 64;
    const HEIGHT: u16 = 10;
    const SHELL_SENTINEL: &str = "PRE-OCTET-SHELL-HISTORY";

    let (mut shell, bytes) =
        emulated_shell_with_sync(crate::tui::theme::test_theme(), WIDTH, HEIGHT, true);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
    terminal.process(format!("{SHELL_SENTINEL}\r\n").as_bytes());
    terminal.process(&drain(&bytes));

    for index in 0..18 {
        shell.notice(format!("OCTET-OWNED-RESIZE-{index:02}"));
    }
    shell.render();
    terminal.process(&drain(&bytes));

    terminal.set_size(256, WIDTH);
    terminal.set_scrollback(usize::MAX);
    assert!(terminal.screen().contents().contains(SHELL_SENTINEL));
    terminal.set_size(HEIGHT, WIDTH);
    terminal.set_scrollback(0);

    terminal.set_size(HEIGHT, RESIZED_WIDTH);
    shell.set_size(RESIZED_WIDTH, HEIGHT);
    shell.render();
    let resize = drain(&bytes);
    let resize_text = String::from_utf8_lossy(&resize);
    assert!(resize_text.contains("\x1b[?2026h"), "{resize_text:?}");
    assert!(
        resize_text.contains("\x1b[2J\x1b[H\x1b[3J"),
        "Pi resize must clear saved lines before replay: {resize_text:?}"
    );
    assert!(
        resize_text.contains("OCTET-OWNED-RESIZE-00"),
        "Pi resize omitted off-screen logical history: {resize_text:?}"
    );
    assert!(
        resize_text.contains("OCTET-OWNED-RESIZE-17"),
        "{resize_text:?}"
    );
    assert!(resize_text.contains("\x1b[?2026l"), "{resize_text:?}");
    process_vt100_with_saved_line_clear(&mut terminal, &resize, HEIGHT, RESIZED_WIDTH, 512);

    terminal.set_size(256, RESIZED_WIDTH);
    terminal.set_scrollback(usize::MAX);
    let physical = terminal.screen().contents();
    assert!(
        !physical.contains(SHELL_SENTINEL),
        "Pi saved-line reset retained pre-application history: {physical}"
    );
    for index in 0..18 {
        let sentinel = format!("OCTET-OWNED-RESIZE-{index:02}");
        assert_eq!(
            physical.matches(&sentinel).count(),
            1,
            "{sentinel} was lost or duplicated after resize:\n{physical}"
        );
    }
}

#[test]
fn resize_while_overlayed_replays_the_pi_composited_frame() {
    const WIDTH: u16 = 48;
    const RESIZED_WIDTH: u16 = 64;
    const HEIGHT: u16 = 10;

    let (mut shell, bytes) =
        emulated_shell_with_sync(crate::tui::theme::test_theme(), WIDTH, HEIGHT, true);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
    terminal.process(&drain(&bytes));

    for index in 0..18 {
        shell.notice(format!("OCTET-OVERLAY-RESIZE-{index:02}"));
    }
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "OVERLAY-ACTIVE-STREAM-BEFORE".into(),
        },
    );
    shell.render();
    terminal.process(&drain(&bytes));

    shell.show_overlay_text("ACTIVE-OVERLAY-SENTINEL".into());
    shell.render();
    terminal.process(&drain(&bytes));
    assert!(
        terminal
            .screen()
            .contents()
            .contains("ACTIVE-OVERLAY-SENTINEL"),
        "{}",
        terminal.screen().contents()
    );

    terminal.set_size(HEIGHT, RESIZED_WIDTH);
    shell.set_size(RESIZED_WIDTH, HEIGHT);
    shell.render();
    let resize = drain(&bytes);
    let resize_text = String::from_utf8_lossy(&resize);
    assert!(
        resize_text.contains("\x1b[2J\x1b[H\x1b[3J"),
        "{resize_text:?}"
    );
    assert!(
        resize_text.contains("ACTIVE-OVERLAY-SENTINEL"),
        "{resize_text:?}"
    );
    assert!(
        !resize_text.contains("OVERLAY-ACTIVE-STREAM-BEFORE"),
        "Pi replays the current composited frame, not rows hidden by its overlay: {resize_text:?}"
    );
    // The public response keeps a trailing Working row. The resize still
    // replays only the composited overlay frame, never its hidden live tail.
    for index in 0..17 {
        let sentinel = format!("OCTET-OVERLAY-RESIZE-{index:02}");
        assert!(
            resize_text.contains(&sentinel),
            "{sentinel} was not replayed with the composited overlay:\n{resize_text:?}"
        );
    }
    assert!(
        !resize_text.contains("OCTET-OVERLAY-RESIZE-17"),
        "unexpected mutable notice in resize replay: {resize_text:?}"
    );

    process_vt100_with_saved_line_clear(&mut terminal, &resize, HEIGHT, RESIZED_WIDTH, 512);
    assert!(
        terminal
            .screen()
            .contents()
            .contains("ACTIVE-OVERLAY-SENTINEL"),
        "{}",
        terminal.screen().contents()
    );

    terminal.set_size(256, RESIZED_WIDTH);
    terminal.set_scrollback(usize::MAX);
    let physical = terminal.screen().contents();
    assert!(physical.contains("OCTET-OVERLAY-RESIZE-00"), "{physical}");
    assert!(physical.contains("ACTIVE-OVERLAY-SENTINEL"), "{physical}");
    terminal.set_size(HEIGHT, RESIZED_WIDTH);
    terminal.set_scrollback(0);

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: " OVERLAY-ACTIVE-STREAM-AFTER".into(),
        },
    );
    shell.render();
    terminal.process(&drain(&bytes));
    assert!(
        terminal
            .screen()
            .contents()
            .contains("ACTIVE-OVERLAY-SENTINEL"),
        "{}",
        terminal.screen().contents()
    );

    shell.close_overlay();
    shell.render();
    let close = drain(&bytes);
    let close_text = String::from_utf8_lossy(&close);
    assert!(
        !close_text.contains("\x1b[3J"),
        "Pi can restore an overlay whose first changed row remains visible: {close_text:?}"
    );
    terminal.process(&close);
    terminal.set_size(256, RESIZED_WIDTH);
    terminal.set_scrollback(usize::MAX);
    let physical = terminal.screen().contents();
    assert!(
        physical.contains("OVERLAY-ACTIVE-STREAM-BEFORE"),
        "{physical}"
    );
    assert!(
        physical.contains("OVERLAY-ACTIVE-STREAM-AFTER"),
        "{physical}"
    );
    for index in 0..18 {
        let sentinel = format!("OCTET-OVERLAY-RESIZE-{index:02}");
        assert_eq!(
            physical.matches(&sentinel).count(),
            1,
            "{sentinel} was not retained exactly once after closing the overlay:\n{physical}"
        );
    }
}

#[test]
fn slash_popup_then_context_overlay_uses_pi_full_frame_replay() {
    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 16;

    for synchronized_output in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_sync(
            crate::tui::theme::test_theme(),
            WIDTH,
            HEIGHT,
            synchronized_output,
        );
        let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
            std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
        };
        let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
        terminal.process(&drain(&bytes));

        for index in 0..40 {
            shell.notice(format!("CONTEXT-OVERLAY-HISTORY-{index:02}"));
        }
        shell.render();
        terminal.process(&drain(&bytes));

        // Paint the tallest slash-command surface before completing the
        // command. This is the transition that used to advance the native
        // history seam by nine rows in a 16-row terminal.
        shell.apply_edit(EditAction::Char('/'));
        shell.render();
        terminal.process(&drain(&bytes));
        assert!(terminal.screen().contents().contains("commands 1–9/"));

        let (_directory, app) = crate::compaction::tests::app_for_estimate();
        shell.clear_editor();
        shell.show_context_report(crate::tui::context::ContextReport::capture(&app, &[]));
        shell.render();
        let context_frame = drain(&bytes);
        assert!(
            context_frame
                .windows(b"\x1b[3J".len())
                .any(|bytes| bytes == b"\x1b[3J"),
            "Pi must clear and replay when the overlay changes above its viewport"
        );
        process_vt100_with_saved_line_clear(&mut terminal, &context_frame, HEIGHT, WIDTH, 512);
        terminal.set_scrollback(0);

        let visible = terminal.screen().contents();
        assert!(
            visible
                .lines()
                .next()
                .is_some_and(|line| line.contains("Context")),
            "context heading was clipped with synchronized_output={synchronized_output}:\n{visible}"
        );
        assert!(visible.contains("Estimated usage by category"), "{visible}");
        assert!(visible.contains("Runtime framing and tools"), "{visible}");
        assert!(visible.contains("System instructions"), "{visible}");
        assert!(visible.contains("tokens"), "{visible}");
        assert!(
            visible.contains('⛀'),
            "context grid was clipped:\n{visible}"
        );
        assert!(
            visible
                .lines()
                .any(|line| line == default_composer_rule(WIDTH)),
            "composer disappeared:\n{visible}"
        );

        shell.close_overlay();
        shell.render();
        let close = drain(&bytes);
        assert!(
            !close
                .windows(b"\x1b[3J".len())
                .any(|bytes| bytes == b"\x1b[3J"),
            "Pi can restore the context overlay from its visible first change"
        );
        terminal.process(&close);
        terminal.set_size(512, WIDTH);
        terminal.set_scrollback(usize::MAX);
        let physical = terminal.screen().contents();
        assert!(
            !physical.contains("Estimated usage by category"),
            "overlay entered history:\n{physical}"
        );
        for index in 0..40 {
            let sentinel = format!("CONTEXT-OVERLAY-HISTORY-{index:02}");
            assert_eq!(
                physical.matches(&sentinel).count(),
                1,
                "{sentinel} was lost or duplicated with synchronized_output={synchronized_output}:\n{physical}"
            );
        }
    }
}

#[test]
fn native_scrollback_keeps_finalized_tool_stable_while_streaming_scrolled_away() {
    use octet_agent::ToolOutput;

    const WIDTH: u16 = 72;
    const HEIGHT: u16 = 12;

    for synchronized_output in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_sync(
            crate::tui::theme::test_theme(),
            WIDTH,
            HEIGHT,
            synchronized_output,
        );
        let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
            std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
        };
        let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
        terminal.process(&drain(&bytes));

        for index in 0..18 {
            shell.notice(format!("TOOL-SCROLLBACK-HISTORY-{index:02}"));
        }
        let run_id = shell.begin_run("openai");
        let tool_id = ToolCallId("tool-scrollback-regression".into());
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolStarted {
                id: tool_id.clone(),
                name: "bash".into(),
                args: serde_json::json!({"command": "TOOL-CARD-SENTINEL"}),
            },
        );
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolFinished {
                id: tool_id,
                result: Ok(ToolOutput::new("tool completed")),
                duration: Duration::from_millis(10),
            },
        );
        shell.render();
        terminal.process(&drain(&bytes));

        for index in 0..4 {
            shell.on_run_event(
                run_id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: format!(
                        "STREAM-BEFORE-{index:02} has enough words to occupy a physical row.\n\n"
                    ),
                },
            );
            shell.render();
            terminal.process(&drain(&bytes));
        }

        let offset = (1..=usize::from(HEIGHT))
            .find(|offset| {
                terminal.set_scrollback(*offset);
                terminal
                    .screen()
                    .contents()
                    .lines()
                    .take(terminal.screen().scrollback())
                    .any(|line| line.contains("TOOL-CARD-SENTINEL"))
            })
            .expect("finalized tool should be retained in native scrollback");
        terminal.set_scrollback(offset);
        let historical_row_count = terminal.screen().scrollback();
        let historical_view = terminal
            .screen()
            .contents()
            .lines()
            .take(historical_row_count)
            .map(str::to_owned)
            .collect::<Vec<_>>();

        for index in 0..12 {
            if index == 2 {
                // vt100 0.15 cannot materialize a viewport whose preserved
                // scrollback offset exceeds the screen height. Two streamed
                // frames are enough to exercise read-while-streaming; process
                // the remaining chronology from the live tail.
                terminal.set_scrollback(0);
            }
            shell.on_run_event(
                run_id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: format!(
                        "STREAM-AFTER-{index:02} has enough words to occupy a physical row.\n\n"
                    ),
                },
            );
            shell.render();
            terminal.process(&drain(&bytes));
            if index < 2 {
                let viewed_history = terminal
                    .screen()
                    .contents()
                    .lines()
                    .take(historical_row_count)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                assert_eq!(
                    viewed_history, historical_view,
                    "historical tool surface changed during token {index} with synchronized_output={synchronized_output}"
                );
            }
        }

        terminal.set_size(256, WIDTH);
        terminal.set_scrollback(usize::MAX);
        let physical = terminal.screen().contents();
        assert_eq!(
            physical.matches("TOOL-CARD-SENTINEL").count(),
            1,
            "finalized tool was lost or duplicated:\n{physical}"
        );
        for index in 0..12 {
            let sentinel = format!("STREAM-AFTER-{index:02}");
            assert_eq!(
                physical.matches(&sentinel).count(),
                1,
                "{sentinel} was lost or duplicated:\n{physical}"
            );
        }
    }
}

#[test]
fn streamed_table_and_wrapped_lists_survive_shrink_scroll_and_resize() {
    const WIDTH: u16 = 96;
    const HEIGHT: u16 = 22;
    let (mut shell, bytes) =
        emulated_shell_with_sync(crate::tui::theme::test_theme(), WIDTH, HEIGHT, true);
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
    let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
    };
    terminal.process(&drain(&bytes));

    shell.on_prompt_submitted(
        "TABLE-PROMPT-SENTINEL: stream a long table while mutable chrome changes height",
    );
    shell.render();
    terminal.process(&drain(&bytes));

    // Submission is painted while idle. Beginning the run changes the
    // composer/status height before any model text arrives.
    let run_id = shell.begin_run("openai");
    shell.render();
    terminal.process(&drain(&bytes));
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "constructing a long boundary regression table".into(),
        },
    );
    shell.state.borrow_mut().advance_event_dot_animation();
    shell.render();
    terminal.process(&drain(&bytes));

    let mut update_probe = ShellFrameState::default();
    let initial_probe = render_shell_update(
        &shell.state.borrow(),
        WIDTH,
        Instant::now(),
        &mut update_probe,
    );
    assert!(!initial_probe.reanchor_viewport);
    let mut saw_streaming_row_shrink = false;
    let mut current_width = WIDTH;

    let mut response = String::from(
        "# TABLE HEADING SENTINEL\n\n\
             | Marker | Boundary condition details | Regression scenario details | Expected behavior details |\n\
             |---|---|---|---|\n",
    );
    for index in 0..12 {
        response.push_str(&format!(
                "| ROW{index:02}SENTINEL | boundary condition {index} contains enough distinct words to wrap | streaming markdown reparses this row as tokens arrive | retain exactly one final physical copy in terminal history |\n"
            ));
    }
    response.push_str(
            "\n## LIST HEADING SENTINEL\n\n\
             - **WRAPPED-LIST-SENTINEL** remains unique while this deliberately long list item wraps across several terminal cells and rows.\n",
        );
    for (chunk_index, chunk) in response.as_bytes().chunks(5).enumerate() {
        shell.on_run_event(
            run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: String::from_utf8(chunk.to_vec()).unwrap(),
            },
        );

        // Change the emulator dimensions before delivering the resize event,
        // matching terminal event order. `vt100` does not model modern
        // soft-wrap reflow; the ED 3 helper below models only the reset.
        // Keep a nonzero scrollback offset while more tokens arrive, then
        // widen again during the same generation.
        let resized = match chunk_index {
            180 => Some(61),
            360 => Some(WIDTH),
            _ => None,
        };
        if let Some(width) = resized {
            terminal.set_scrollback(6);
            terminal.set_size(HEIGHT, width);
            shell.set_size(width, HEIGHT);
            current_width = width;
        }

        let previous_rows = update_probe.transcript_len;
        let probe = render_shell_update(
            &shell.state.borrow(),
            current_width,
            Instant::now(),
            &mut update_probe,
        );
        saw_streaming_row_shrink |= update_probe.transcript_len < previous_rows;
        assert_eq!(
            probe.reanchor_viewport,
            resized.is_some(),
            "only width reflow should reanchor the streaming viewport"
        );
        shell.render();
        let output = drain(&bytes);
        process_vt100_with_saved_line_clear(&mut terminal, &output, HEIGHT, current_width, 512);
        if chunk_index == 360 {
            terminal.set_scrollback(0);
        }
    }
    assert!(
        saw_streaming_row_shrink,
        "fixture did not exercise a shrinking streamed layout"
    );

    terminal.set_size(256, WIDTH);
    terminal.set_scrollback(usize::MAX);
    let physical = terminal.screen().contents();
    let rule = default_composer_rule(WIDTH);
    assert_eq!(
        physical.lines().filter(|line| line == &rule).count(),
        2,
        "mutable composer was committed to scrollback:\n{physical}"
    );
    for sentinel in [
        "TABLE-PROMPT-SENTINEL",
        "TABLE HEADING SENTINEL",
        "ROW00SENTINEL",
        "ROW05SENTINEL",
        "ROW11SENTINEL",
        "LIST HEADING SENTINEL",
        "WRAPPED-LIST-SENTINEL",
    ] {
        assert_eq!(
            physical.matches(sentinel).count(),
            1,
            "{sentinel:?} was duplicated in native scrollback:\n{physical}"
        );
    }
}

#[test]
fn streamed_table_body_does_not_replay_native_history() {
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol, StopReason};

    let mut replay = MarkdownStreamReplay::new();
    // Establish the table before measuring the body, so recognizing the
    // header/delimiter is not confused with a body-layout regression.
    let mut response = String::from("| Marker | Description | State |\n|---|---|---|\n");
    replay.delta(&response);
    let baseline = replay.full_redraws();
    let mut body_ed3 = 0;
    let mut body_history_rewrites = 0;
    for index in 0..24 {
        let row = format!(
            "| ROW{index:02} | distinct boundary words to wrap inside each cell | retained |\n"
        );
        response.push_str(&row);
        for chunk in row.as_bytes().chunks(5) {
            let output = replay.delta(std::str::from_utf8(chunk).unwrap());
            body_ed3 += output.matches("\x1b[3J").count();
            body_history_rewrites += output.matches("MARKDOWN-HISTORY-00").count();
        }
        assert!(
            replay
                .terminal
                .screen()
                .contents()
                .contains(&format!("ROW{index:02}")),
            "completed table row {index} was withheld from the live frame"
        );
    }
    let body_redraws = replay.full_redraws() - baseline;

    // Canonical final parsing may legitimately relayout the table once. Do not
    // count that separately observable boundary as a streaming-body replay.
    let before_finish = replay.full_redraws();
    replay.shell.on_run_event(
        replay.run_id,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: AssistantMessage {
                content: vec![AssistantPart::Text(response)],
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
    let finish_output = replay.render();
    let finish_redraws = replay.full_redraws() - before_finish;
    let finish_ed3 = finish_output.matches("\x1b[3J").count();
    let physical = replay.history();
    for index in 0..24 {
        let sentinel = format!("ROW{index:02}");
        assert_eq!(physical.matches(&sentinel).count(), 1, "{sentinel}");
    }
    assert_eq!(
        (body_redraws, body_ed3, body_history_rewrites),
        (0, 0, 0),
        "table body must append without resetting history; canonical finish separately: \
         full_redraws={finish_redraws}, ED3={finish_ed3}"
    );
}

#[test]
fn streamed_rich_paragraph_keeps_formatting_across_8192_bytes() {
    let mut replay = MarkdownStreamReplay::new();
    let mut response = format!("**RICH-PREFIX** {}", "word ".repeat(1636));
    response.truncate(8192);
    assert_eq!(response.len(), 8192);
    replay.delta(&response[..512]);
    let (row, column) = find_ascii_cell(replay.terminal.screen(), "RICH-PREFIX")
        .expect("rich prefix must be promptly visible");
    assert!(replay.terminal.screen().cell(row, column).unwrap().bold());
    let baseline = replay.full_redraws();

    for (index, chunk) in response.as_bytes()[512..].chunks(512).enumerate() {
        let output = replay.delta(std::str::from_utf8(chunk).unwrap());
        replay.assert_append_frame(&output, baseline, &format!("rich chunk {index}"));
        assert!(!strip_terminal_sequences(&output).contains("**RICH-PREFIX**"));
    }
    for source_bytes in [8193, 8194] {
        let output = replay.delta("x");
        assert!(
            !strip_terminal_sequences(&output).contains("**RICH-PREFIX**"),
            "rich paragraph restored literal delimiters at byte {source_bytes}"
        );
        replay.assert_append_frame(&output, baseline, &format!("rich byte {source_bytes}"));
    }
    let physical = replay.history();
    assert_eq!(physical.matches("RICH-PREFIX").count(), 1);
    assert!(!physical.contains("**RICH-PREFIX**"));
    let (row, column) = find_ascii_cell(replay.terminal.screen(), "RICH-PREFIX").unwrap();
    assert!(replay.terminal.screen().cell(row, column).unwrap().bold());
}

#[test]
fn streamed_code_closing_fence_fragments_do_not_flash_as_code() {
    for marker in ['`', '~'] {
        let mut replay = MarkdownStreamReplay::new();
        replay.delta(&format!("{}rust\n", marker.to_string().repeat(3)));
        for index in 0..32 {
            replay.delta(&format!("let CODE{index:02} = value;\n"));
        }
        let baseline = replay.full_redraws();
        for fragment in [
            marker.to_string(),
            marker.to_string(),
            marker.to_string(),
            "\n".into(),
        ] {
            let output = replay.delta(&fragment);
            let visible = replay.terminal.screen().contents();
            assert!(
                !visible.contains(marker),
                "closing {marker} fence fragment {fragment:?} flashed as code:\n{visible}"
            );
            replay.assert_append_frame(&output, baseline, "closing code fence");
        }
        replay.delta("\nAFTER-CODE-SENTINEL");
        assert!(replay
            .terminal
            .screen()
            .contents()
            .contains("AFTER-CODE-SENTINEL"));
        let physical = replay.history();
        for index in 0..32 {
            let sentinel = format!("CODE{index:02}");
            assert_eq!(physical.matches(&sentinel).count(), 1, "{sentinel}");
        }
        assert!(!physical.contains(marker));
    }
}

#[test]
fn streamed_huge_newline_free_prose_remains_promptly_visible() {
    let mut replay = MarkdownStreamReplay::new();
    let baseline = replay.full_redraws();
    // Exceed even the 64 KiB unstable-parse budget without a newline. Every
    // completed delta must remain live; freezing all long tails is not a fix.
    for index in 0..144 {
        let text = format!("{} PLAIN{index:03} ", "ordinary prose ".repeat(34));
        let output = replay.delta(&text);
        replay.assert_append_frame(&output, baseline, &format!("plain chunk {index}"));
        let visible = replay.terminal.screen().contents();
        assert!(
            visible.contains(&format!("PLAIN{index:03}")),
            "newline-free prose chunk {index} was withheld:\n{visible}"
        );
    }
    let physical = replay.history();
    for index in 0..144 {
        let sentinel = format!("PLAIN{index:03}");
        assert_eq!(physical.matches(&sentinel).count(), 1, "{sentinel}");
    }
}

#[test]
fn closing_overlay_reanchors_without_replaying_native_scrollback() {
    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 16;
    for synchronized_output in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_sync(
            crate::tui::theme::test_theme(),
            WIDTH,
            HEIGHT,
            synchronized_output,
        );
        let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 512);
        let drain = |bytes: &Arc<Mutex<Vec<u8>>>| {
            std::mem::take(&mut *bytes.lock().expect("emulated terminal bytes"))
        };
        terminal.process(&drain(&bytes));

        for index in 0..32 {
            shell.notice(format!("overlay-history-{index:02}"));
        }
        shell.render();
        terminal.process(&drain(&bytes));

        shell.show_overlay_text("status overlay\nclose me".into());
        shell.render();
        terminal.process(&drain(&bytes));

        shell.close_overlay();
        shell.render();
        let close_frame = drain(&bytes);
        // Closing a one-viewport overlay must repaint only the visible
        // tail. Replaying the retained transcript here is what created
        // duplicated lines in terminal-owned scrollback.
        assert!(
            close_frame.len() < 8 * 1024,
            "overlay close replayed an unbounded frame ({} bytes)",
            close_frame.len()
        );
        terminal.process(&close_frame);

        terminal.set_size(512, WIDTH);
        terminal.set_scrollback(usize::MAX);
        let physical = terminal.screen().contents();
        for index in 0..32 {
            let sentinel = format!("overlay-history-{index:02}");
            assert_eq!(
                physical.matches(&sentinel).count(),
                1,
                "{sentinel} replayed with synchronized_output={synchronized_output}:\n{physical}"
            );
        }
    }
}
