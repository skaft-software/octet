//! Chrome repaint minimisation, theme-swap replays, the application viewport, and drag/select
//! mapping. Separate because they assert which rows a repaint may touch and which rows the
//! semantic selection may claim.

use super::support::*;

use super::*;

#[test]
fn tool_progress_repaints_the_bounded_rendered_tail() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    let id = ToolCallId("long-bash".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "long-running-audit"}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Output {
                stream: octet_agent::OutputStream::Stdout,
                bytes: bytes::Bytes::from_static(b"private live output"),
            },
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Status("private status detail".into()),
        },
    );
    let rendered = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 96).join("\n"));
    assert!(rendered.contains("Bash  long-running-audit"), "{rendered}");
    assert!(rendered.contains("private live output"), "{rendered}");
    assert!(rendered.contains("private status detail"), "{rendered}");
    assert!(shell.debug_tool_output(&id).is_some());
}

#[test]
fn short_transcript_chrome_follows_content_without_viewport_padding() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 40);
    shell.set_identity("codex", "gpt-5.6", "high");
    let run_id = shell.begin_run("codex");
    let now = Instant::now();

    let composer_row = |lines: &[String]| {
        lines
            .iter()
            .position(|line| line.contains(CURSOR_MARKER))
            .expect("composer cursor row")
    };
    let initial = render_shell_at(&shell.state.borrow(), 80, now);
    let initial_composer = composer_row(&initial);
    assert!(
        initial.len() < 40,
        "native mode must not pad a short frame to the terminal height"
    );

    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "I’ll inspect the tree.".into(),
        },
    );
    let streamed = render_shell_at(&shell.state.borrow(), 80, now);
    // The response retains one trailing Working row while the run is active.
    // The response row plus its transition therefore grow the short frame by
    // two rows without padding it to the terminal height.
    assert_eq!(composer_row(&streamed), initial_composer + 2);
    assert!(streamed.len() < 40);

    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: ToolCallId("read-1".into()),
            name: "read".into(),
            args: serde_json::json!({"path": "src/main.rs"}),
        },
    );
    let tool = render_shell_at(&shell.state.borrow(), 80, now);
    assert!(
        composer_row(&tool) > composer_row(&streamed),
        "the active tool should precede the persistent Working row"
    );
    assert!(tool
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .any(|line| line.contains("Working")));
    assert!(tool.len() < 40);

    shell.queue_steering(&ComposedInput::from_text("also inspect tests".into()));
    let steering = render_shell_at(&shell.state.borrow(), 80, now);
    assert!(composer_row(&steering) > composer_row(&tool));
    assert!(steering.len() < 40);
    let steering_plain = steering
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(steering_plain.contains("Steering · queued"));
    assert!(steering_plain.contains("  └ also inspect tests"));

    // The native-scrollback renderer retains committed transcript rows and
    // returns only the mutable suffix after the first frame.
    let mut frame = ShellFrameState::default();
    let first = render_shell_update(&shell.state.borrow(), 80, now, &mut frame);
    assert_eq!(first.stable_prefix, 0);
    assert_eq!(first.replacement, steering);
    assert!(!first.rebuild_scrollback);
    let next = render_shell_update(&shell.state.borrow(), 80, now, &mut frame);
    assert!(next.stable_prefix > 0);
    assert!(!next.rebuild_scrollback);
    assert!(next.stable_prefix + next.replacement.len() < 40);
    assert!(next
        .replacement
        .iter()
        .any(|line| line.contains(CURSOR_MARKER)));
}

#[test]
fn emulated_native_short_frame_does_not_pin_composer_to_terminal_bottom() {
    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 40;
    let (mut shell, bytes) = emulated_shell(crate::tui::theme::test_theme(), WIDTH, HEIGHT);
    shell.set_identity("codex", "gpt-5.6", "high");
    shell.notice("recent transcript row");
    shell.render();

    let output = bytes.lock().unwrap().clone();
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 0);
    terminal.process(&output);
    assert!(
        terminal
            .screen()
            .contents()
            .contains("recent transcript row"),
        "transcript was not painted: {:?}",
        terminal.screen().contents()
    );
    let (cursor_row, _) = terminal.screen().cursor_position();
    assert!(
        cursor_row < HEIGHT / 2,
        "short native frame pinned the composer at terminal row {cursor_row}"
    );
}

#[test]
fn slash_popup_height_changes_use_differential_repaint() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    for character in "/res".chars() {
        shell.apply_edit(EditAction::Char(character));
    }

    let mut frame = ShellFrameState::default();
    let initial = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(!initial.reanchor_viewport);
    let composer_row = |update: &sexy_tui_rs::FrameUpdate| {
        update
            .replacement
            .iter()
            .position(|line| line.contains(CURSOR_MARKER))
            .expect("composer cursor row")
    };
    let initial_composer_row = composer_row(&initial);

    for _ in 0..3 {
        shell.apply_edit(EditAction::Backspace);
    }
    assert_eq!(shell.pending(), "/");
    let expanded = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(
        !expanded.reanchor_viewport,
        "growing mutable chrome must not replay the viewport"
    );
    assert_eq!(
        composer_row(&expanded),
        initial_composer_row,
        "suggestion growth must expand below the composer"
    );

    for character in "res".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    let collapsed = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(
        !collapsed.reanchor_viewport,
        "shrinking mutable chrome must clear only its changed tail"
    );
    assert_eq!(
        composer_row(&collapsed),
        initial_composer_row,
        "suggestion shrinkage must leave the composer in place"
    );
}

#[test]
fn native_scrollback_frame_exposes_committed_rows_and_reuses_stable_history() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    for number in 0..80 {
        shell.notice(format!("older {number}"));
    }
    shell.notice("live streamed result");

    let full = render_shell(&shell.state.borrow(), 80);
    let full_text = full.join("\n");
    assert!(full.len() > 14);
    assert!(full_text.contains("older 0"));
    assert!(full_text.contains("live streamed result"));

    let mut frame = ShellFrameState::default();
    let initial = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert_eq!(initial.stable_prefix, 0);
    assert!(!initial.rebuild_scrollback);
    let committed = frame.transcript_len;

    shell.notice("new native row");
    let appended = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert_eq!(appended.stable_prefix, committed);
    assert!(!appended.rebuild_scrollback);
    let appended_text = appended.replacement.join("\n");
    assert!(appended_text.contains("new native row"));
    assert!(!appended_text.contains("older 0"));
}

#[test]
fn native_ctrl_o_rebuilds_offscreen_compaction_and_contracts_the_complete_frame() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 12);
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Compaction(Box::new(CompactionBlock {
            label: "Context compacted".into(),
            summary: "COMPLETE COMPACTION SUMMARY".into(),
            expanded: false,
        })));
        for number in 0..40 {
            state.push_block(TranscriptBlock::Notice(format!("later event {number}")));
        }
    }

    let mut frame = ShellFrameState::default();
    let initial = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(!initial.rebuild_scrollback);
    assert!(initial.pinned.is_some());
    assert!(initial.stable_prefix + initial.replacement.len() > 12);
    assert!(!initial
        .replacement
        .iter()
        .any(|line| line.contains("COMPLETE COMPACTION SUMMARY")));

    shell.toggle_disclosure();
    let expanded = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert_eq!(expanded.stable_prefix, 0);
    assert!(expanded.rebuild_scrollback);
    assert!(expanded.pinned.is_some());
    assert!(expanded
        .replacement
        .iter()
        .any(|line| line.contains("COMPLETE COMPACTION SUMMARY")));

    shell.toggle_disclosure();
    let collapsed = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert_eq!(collapsed.stable_prefix, 0);
    assert!(collapsed.rebuild_scrollback);
    assert!(collapsed.pinned.is_some());
    assert!(!collapsed
        .replacement
        .iter()
        .any(|line| line.contains("COMPLETE COMPACTION SUMMARY")));
    assert!(collapsed
        .replacement
        .iter()
        .any(|line| line.contains(CURSOR_MARKER)));
}

#[test]
fn theme_swap_matches_pi_clear_and_complete_replay() {
    const WIDTH: u16 = 32;
    const HEIGHT: u16 = 10;
    let theme_source = |name: &str, foreground: &str| {
        crate::tui::theme::test_theme_from_source(&format!(
            r##"
                    [metadata]
                    name = "{name}"

                    [colors]
                    foreground = "{foreground}"
                "##
        ))
    };
    let first_theme = theme_source("Viewport red", "#b01020");
    let old_foreground = role_rgb_color(&first_theme, "foreground");
    let (mut shell, bytes) = emulated_shell(first_theme, WIDTH, HEIGHT);
    {
        let mut state = shell.state.borrow_mut();
        for number in 0..12 {
            state.push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(format!("historic-{number}")),
            )));
        }
    }
    shell.render();

    let before = bytes
        .lock()
        .expect("emulated terminal output mutex poisoned")
        .clone();
    let mut before_terminal = vt100::Parser::new(HEIGHT, WIDTH, 128);
    before_terminal.process(&before);
    let blank_row = before_terminal
        .screen()
        .rows(0, WIDTH)
        .enumerate()
        .find_map(|(row, contents)| contents.trim().is_empty().then_some(row as u16))
        .expect("fixture should leave a visible semantic separator row");

    // Put a cell into a row that is byte-identical across the two logical
    // frames. A changed-row diff would leave this corruption behind; the
    // required full visible repaint must erase it.
    bytes
        .lock()
        .expect("emulated terminal output mutex poisoned")
        .extend_from_slice(
            format!("\x1b[{};{}H\x1b[48;2;1;2;3mX\x1b[0m", blank_row + 1, WIDTH).as_bytes(),
        );

    let second_theme = theme_source("Viewport blue", "#2040c0");
    let new_foreground = role_rgb_color(&second_theme, "foreground");
    shell.set_theme(second_theme);
    shell.render();

    let complete = bytes
        .lock()
        .expect("emulated terminal output mutex poisoned")
        .clone();
    assert!(
        complete
            .windows(b"\x1b[3J".len())
            .any(|window| window == b"\x1b[3J"),
        "Pi theme changes above the viewport must clear and replay"
    );
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 128);
    process_vt100_with_saved_line_clear(&mut terminal, &complete, HEIGHT, WIDTH, 128);
    assert!(
        find_ascii_cell(terminal.screen(), "historic-").is_some(),
        "visible tail lost after theme replay: {:?}",
        terminal.screen().contents()
    );
    assert_ascii_foreground(&terminal, "historic-11", new_foreground);
    assert!(
        find_ascii_cell(terminal.screen(), "X").is_none(),
        "full replay left a stale cell: {:?}",
        terminal.screen().contents()
    );

    terminal.set_size(128, WIDTH);
    terminal.set_scrollback(usize::MAX);
    for number in 0..12 {
        assert_ascii_foreground(&terminal, &format!("historic-{number}"), new_foreground);
    }
    assert_ne!(old_foreground, new_foreground);
}

#[test]
fn application_viewport_theme_swap_repaints_without_clearing_shell_scrollback() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    let mut frame = ShellFrameState::default();
    let now = Instant::now();

    let initial = render_shell_viewport_update(&shell.state.borrow(), 80, now, &mut frame);
    assert!(!initial.reanchor_viewport);
    assert!(!initial.rebuild_scrollback);

    shell.set_theme(crate::tui::theme::test_theme_from_source(
        r##"
                [metadata]
                name = "Application viewport theme"

                [colors]
                foreground = "#2040c0"
            "##,
    ));
    let repainted = render_shell_viewport_update(&shell.state.borrow(), 80, now, &mut frame);
    assert!(repainted.reanchor_viewport);
    assert!(!repainted.rebuild_scrollback);
}

#[test]
fn switching_back_to_default_clears_custom_theme_attributes() {
    const WIDTH: u16 = 48;
    const HEIGHT: u16 = 10;
    let custom = crate::tui::theme::test_theme_from_source(SURFACE_TEST_THEME);
    let (mut shell, bytes) = emulated_shell(custom, WIDTH, HEIGHT);
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("plain-default-prose".into()),
        )));
    shell.render();

    // Custom surface rendering terminates every row with a full rendition
    // reset. Switching themes must clear and replay the complete frame.
    shell.set_theme(crate::tui::theme::test_theme());
    shell.render();

    let complete = bytes
        .lock()
        .expect("emulated terminal output mutex poisoned")
        .clone();
    let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 128);
    terminal.process(&complete);
    assert_ascii_default_rendition(&terminal, "plain-default-prose");
}

#[test]
fn new_session_shrink_reanchors_but_picker_growth_does_not() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    for number in 0..80 {
        shell.notice(format!("resumed history {number}"));
    }

    let mut frame = ShellFrameState::default();
    let resumed = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(!resumed.reanchor_viewport);
    assert!(!resumed.rebuild_scrollback);
    assert!(frame.transcript_len > 14);

    {
        let mut state = shell.state.borrow_mut();
        state.transcript_epoch = state.transcript_epoch.wrapping_add(1);
        state.transcript.clear();
        state.transcript_commit_ids.clear();
        state.block_revisions.clear();
        state.invalidate_transcript_layout();
        state.push_block(TranscriptBlock::Notice("new session created".into()));
    }
    let fresh = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(fresh.reanchor_viewport);
    assert!(!fresh.rebuild_scrollback);
    assert!(fresh.stable_prefix + fresh.replacement.len() < 14);

    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Models"),
        items: vec!["model-a".into(), "model-b".into()],
        descriptions: vec![None, None],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectModel(vec![
            ModelId("model-a".into()),
            ModelId("model-b".into()),
        ]),
    });
    let picker = render_shell_update(&shell.state.borrow(), 80, Instant::now(), &mut frame);
    assert!(
        !picker.reanchor_viewport,
        "inserting picker rows must use a differential tail repaint"
    );
    assert!(!picker.rebuild_scrollback);
    assert!(picker.stable_prefix + picker.replacement.len() <= 14);
    assert!(picker
        .replacement
        .iter()
        .any(|line| line.contains("Models")));
    assert!(picker
        .replacement
        .iter()
        .any(|line| line.contains(CURSOR_MARKER)));
}

#[test]
fn keyboard_page_navigation_claims_the_semantic_viewport_without_mouse_capture() {
    const WIDTH: u16 = 80;
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, 14);
    for number in 0..80 {
        shell.notice(format!("keyboard viewport {number:02}"));
    }
    assert!(!shell.state.borrow().application_viewport_requested);

    let component = ShellComponent::new(shell.state.clone(), false);
    let native = sexy_tui_rs::Component::render_update(&component, WIDTH).expect("native frame");
    // The text-only renderer needs native rows, not the pinned commit handshake.
    assert!(native.pinned.is_none());
    assert!(native.replacement.len() > 14);

    shell.scroll(-1);
    assert!(shell.state.borrow().application_viewport_requested);
    let scrolled =
        sexy_tui_rs::Component::render_update(&component, WIDTH).expect("semantic viewport frame");
    assert!(scrolled.pinned.is_none());
    assert!(scrolled.reanchor_viewport);
    assert!(
        scrolled
            .replacement
            .iter()
            .any(|line| line.contains("PageDown returns to live")),
        "{:?}",
        scrolled.replacement
    );
    assert!(scrolled.replacement.len() <= 14);

    // Returning to live keeps semantic viewport ownership; mouse reporting was
    // never enabled and therefore remains an independent policy decision.
    shell.jump_to_tail();
    let live =
        sexy_tui_rs::Component::render_update(&component, WIDTH).expect("semantic live frame");
    assert!(live.pinned.is_none());
    assert!(live.replacement.len() <= 14);
    assert!(!live
        .replacement
        .iter()
        .any(|line| line.contains("PageDown returns to live")));
}

#[test]
fn explicit_application_viewport_bounds_history_and_keeps_old_rows_reachable() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    for number in 0..80 {
        shell.notice(format!("older {number}"));
    }
    shell.notice("live streamed result");

    let live = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
    let live_text = live.join("\n");
    assert_eq!(live.len(), 14);
    assert!(!live_text.contains("older 0"));
    assert!(live_text.contains("older 79"));
    assert!(live_text.contains("live streamed result"));

    shell.scroll_lines(-10_000);
    let oldest = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
    let oldest_text = oldest.join("\n");
    assert_eq!(oldest.len(), 14);
    assert!(oldest_text.contains("older 0"), "{oldest_text}");
    assert!(oldest_text.contains("PageDown returns to live"));
    assert!(!oldest_text.contains("live streamed result"));

    shell.scroll_lines(10_000);
    let returned = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now()).join("\n");
    assert!(returned.contains("live streamed result"));
    assert!(!returned.contains("PageDown returns to live"));

    shell.select_all_transcript();
    let copied = shell.copy_selected_plain_text().expect("semantic copy");
    assert!(copied.contains("older 0"));
    assert!(copied.contains("live streamed result"));
}

#[test]
fn application_viewport_stays_anchored_while_one_markdown_block_streams() {
    const WIDTH: u16 = 80;
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(WIDTH, 16);
    for number in 0..80 {
        shell.notice(format!("reading anchor {number:02}"));
    }

    let _ = render_shell_viewport_at(&shell.state.borrow(), WIDTH, Instant::now());
    shell.scroll_lines(-8);
    let anchor_rows = |shell: &InteractiveShell| {
        render_shell_viewport_at(&shell.state.borrow(), WIDTH, Instant::now())
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .filter(|line| line.contains("reading anchor"))
            .collect::<Vec<_>>()
    };
    let before = anchor_rows(&shell);
    assert!(!before.is_empty());

    let run_id = shell.begin_run("openai");
    for number in 0..24 {
        shell.on_run_event(
            run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: format!(
                    "\n\n### streamed section {number}\n\nA growing Markdown paragraph whose wrapping changes while the reader remains above the live tail."
                ),
            },
        );
        assert_eq!(
            anchor_rows(&shell),
            before,
            "viewport moved on token batch {number}"
        );
    }

    let scrolled =
        render_shell_viewport_at(&shell.state.borrow(), WIDTH, Instant::now()).join("\n");
    assert!(scrolled.contains("new"), "{scrolled}");
    assert!(scrolled.contains("PageDown returns to live"), "{scrolled}");
}

#[test]
fn semantic_viewport_anchor_survives_wrapped_markdown_resize() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 16);
    let markdown = (0..80)
        .map(|number| {
            format!(
                "paragraph-{number:02} carries enough stable prose to wrap differently after a narrow resize"
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized(markdown),
        )));
        state.push_block(TranscriptBlock::Notice("live tail".into()));
    }

    let _ = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
    shell.scroll_lines(-40);
    let _ = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
    let before = shell
        .state
        .borrow()
        .viewport_anchor
        .get()
        .expect("scrolled semantic anchor");
    assert!(before.semantic);

    shell.set_size(42, 16);
    let _ = render_shell_viewport_at(&shell.state.borrow(), 42, Instant::now());
    let state = shell.state.borrow();
    let after = state
        .viewport_anchor
        .get()
        .expect("anchor retained after resize");
    assert_eq!(after.commit_id, before.commit_id);
    assert_eq!(after.text_offset, before.text_offset);

    let chrome = shell_chrome(&state, 42, Instant::now());
    let transcript = state.rendered_transcript(42);
    let maximum = max_scroll_for_available(transcript.len(), chrome.transcript_rows);
    let scroll = state.scroll_from_bottom.get().min(maximum);
    let capacity = transcript_viewport_capacity(chrome.transcript_rows, scroll > 0);
    let end = transcript.len().saturating_sub(scroll);
    let start = end.saturating_sub(capacity);
    drop(transcript);
    let anchored = selection_position_for_visual_cell(
        &state,
        start + after.desired_screen_row.min(capacity.saturating_sub(1)),
        0,
    )
    .expect("anchored semantic row after resize");
    assert_eq!(state.transcript_commit_ids[anchored.block], after.commit_id);
    assert!(anchored.offset <= after.text_offset);
    let next = selection_position_for_visual_cell(
        &state,
        start + after.desired_screen_row.min(capacity.saturating_sub(1)) + 1,
        0,
    )
    .expect("row following semantic anchor");
    assert!(
        after.text_offset <= next.offset,
        "semantic point {after:?} escaped anchored rows {anchored:?}..{next:?}"
    );
}

#[test]
fn semantic_viewport_anchor_survives_disclosure_contraction_above_it() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 16);
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Compaction(Box::new(CompactionBlock {
            label: "Context compacted".into(),
            summary: (0..60)
                .map(|number| format!("expanded summary row {number}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
            expanded: false,
        })));
        for number in 0..80 {
            state.push_block(TranscriptBlock::Notice(format!(
                "stable event after compaction {number:02}"
            )));
        }
    }
    shell.toggle_disclosure();
    let _ = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
    shell.scroll_lines(-24);
    let visible_events = |shell: &InteractiveShell| {
        render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now())
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .filter(|line| line.contains("stable event after compaction"))
            .collect::<Vec<_>>()
    };
    let before = visible_events(&shell);
    assert!(!before.is_empty());

    shell.toggle_disclosure();
    let after = visible_events(&shell);
    assert_eq!(after, before);
}

#[test]
fn select_all_copy_is_semantic_and_excludes_pinned_chrome() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("openai", "gpt-test", "high");
    for number in 0..120 {
        shell.on_prompt_submitted(&format!("user {number}"));
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(format!(
                    "**assistant {number}**\n\n```rust\nlet n = {number};\n```"
                )),
            )));
    }
    shell.select_all_transcript();
    let copied = shell.copy_selected_plain_text().expect("selection copy");
    assert!(copied.contains("user 0"));
    assert!(copied.contains("assistant 119"));
    assert!(copied.contains("let n = 119;"));
    assert!(!copied.contains(CURSOR_MARKER));
    assert!(!copied.contains("gpt-test"));
    assert!(!copied.contains("\x1b["));
    assert_eq!(shell.copy_buffer().as_deref(), Some(copied.as_str()));
}

#[test]
fn drag_selection_autoscrolls_through_a_transcript_ten_viewports_tall() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    for number in 0..180 {
        shell.on_prompt_submitted(&format!("record {number}"));
    }
    // Establish the cached viewport before mapping mouse rows.
    let _ = render_shell(&shell.state.borrow(), 80);
    let available = shell_chrome(&shell.state.borrow(), 80, Instant::now()).transcript_rows;
    let bottom_row = transcript_viewport_capacity(available, false).saturating_sub(1) as u16;
    // Begin at the physical end of the newest row so the reverse drag
    // includes that complete semantic block as well as the oldest one.
    shell.begin_transcript_selection(bottom_row, 79, false);
    for _ in 0..240 {
        shell.extend_transcript_selection(0, 0);
    }
    shell.end_transcript_selection(0, 0);
    let copied = shell.copy_selected_plain_text().expect("drag copy");
    assert!(copied.contains("record 0"), "{copied}");
    assert!(copied.contains("record 179"), "{copied}");
    assert!(!copied.contains(CURSOR_MARKER));
}

#[test]
fn dragging_into_pinned_chrome_clamps_to_last_semantic_transcript_row() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 14);
    for number in 0..40 {
        shell.on_prompt_submitted(&format!("record {number}"));
    }
    let _ = render_shell(&shell.state.borrow(), 80);
    let capacity = transcript_viewport_capacity_for_state(&shell.state.borrow(), 80);
    assert!(capacity > 1);

    shell.begin_transcript_selection(0, 0, false);
    shell.extend_transcript_selection(13, 0);

    let state = shell.state.borrow();
    let expected = InteractiveShell::transcript_position_at_screen_cell(
        &state,
        capacity.saturating_sub(1) as u16,
        0,
    )
    .expect("last transcript row");
    assert_eq!(
        state
            .transcript_selection
            .as_ref()
            .expect("drag selection")
            .focus,
        expected
    );
}

#[test]
fn overscrolled_viewport_clamps_to_available_transcript() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("visible prompt");
    shell.state.borrow().scroll_from_bottom.set(9_999);
    let rendered = render_shell(&shell.state.borrow(), 120);
    assert!(rendered.iter().any(|line| line.contains("visible prompt")));
    shell.scroll(1);
    assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 0);
}

#[test]
fn character_accurate_selection_maps_correct_columns() {
    for inset in [0_u16, 2, 4] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 14);
        shell.set_theme(theme_with_layout(&format!("transcript_inset = {inset}")));
        shell.on_prompt_submitted("hello world");

        // Establish the cached viewport. Prompts share the resolved inset;
        // selection geometry removes both that inset and the marker cells.
        let _ = render_shell(&shell.state.borrow(), 80);
        let resolved = crate::tui::layout::PresentationLayout::new(&shell.state.borrow().theme, 80);
        let start = resolved.inset + 3; // marker (2) + cell index of 'e' (1)
        let end = start + 4;
        shell.begin_transcript_selection(0, start, false);
        shell.extend_transcript_selection(0, end);
        shell.end_transcript_selection(0, end);

        let copied = shell
            .copy_selected_plain_text()
            .expect("character drag copy");
        assert_eq!(copied, "ello", "transcript inset {inset}");
    }
}
