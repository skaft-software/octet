//! Real terminal-event journeys through the idle/active input dispatch seam.
use super::*;
use crate::tui::keymap::{InputAction, PointerGesture};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn mouse(shell: &mut InteractiveShell, kind: MouseEventKind, row: u16, col: u16) {
    let action = shell.translate_input(
        Some(Event::Mouse(MouseEvent {
            kind,
            row,
            column: col,
            modifiers: KeyModifiers::NONE,
        })),
        true,
    );
    match action {
        InputAction::ScrollLines(lines) => shell.scroll_lines(lines),
        InputAction::TranscriptPointer(PointerGesture::Begin { row, col, extend }) => {
            shell.begin_transcript_selection(row, col, extend)
        }
        InputAction::TranscriptPointer(PointerGesture::Extend { row, col }) => {
            shell.extend_transcript_selection(row, col)
        }
        InputAction::TranscriptPointer(PointerGesture::End { row, col }) => {
            shell.end_transcript_selection(row, col)
        }
        InputAction::Ignore => {}
        other => panic!("unexpected mouse action: {other:?}"),
    }
    clipboard_transport::TEST_COPY.with(|copy| {
        assert_eq!(
            *copy.borrow(),
            shell.copy_buffer(),
            "clipboard handoff payload"
        );
    });
}

fn stream(shell: &mut InteractiveShell) {
    shell.on_agent_event(&AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: "new live output below the selected range\n".into(),
    });
}

fn fixture() -> (InteractiveShell, String) {
    clipboard_transport::TEST_COPY.with(|copy| *copy.borrow_mut() = None);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    shell.capture_mouse = true;
    shell.state.borrow_mut().application_viewport_requested = true;
    let text = (0..200)
        .map(|index| format!("row {index:03} stable transcript text\n"))
        .collect::<String>();
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized(format!("```text\n{text}```")),
        )));
    let text = block_copy_text(&shell.state.borrow().transcript[0]);
    stream(&mut shell);
    shell.isolate_native_test_renderer();
    shell.render_written_test_frame();
    shell.scroll_lines(-25);
    shell.render_written_test_frame();
    (shell, text)
}

fn cell(shell: &InteractiveShell, row: u16, col: u16) -> TranscriptPosition {
    InteractiveShell::transcript_position_at_gesture_cell(&shell.state.borrow(), row, col)
        .expect("painted transcript cell")
}

fn expected(text: &str, a: TranscriptPosition, b: TranscriptPosition) -> &str {
    assert_eq!(a.block, 0);
    assert_eq!(b.block, 0);
    &text[a.offset.min(b.offset)..a.offset.max(b.offset)]
}

#[test]
fn streaming_copyselect_release_copies_exact_range_in_both_directions() {
    for reverse in [false, true] {
        for coalesced in [false, true] {
            let (mut shell, text) = fixture();
            let (a, b) = if reverse {
                ((5, 20), (2, 4))
            } else {
                ((2, 4), (5, 20))
            };
            let payload =
                expected(&text, cell(&shell, a.0, a.1), cell(&shell, b.0, b.1)).to_owned();
            mouse(
                &mut shell,
                MouseEventKind::Down(MouseButton::Left),
                a.0,
                a.1,
            );
            stream(&mut shell);
            shell.render_written_test_frame();
            if !coalesced {
                mouse(
                    &mut shell,
                    MouseEventKind::Drag(MouseButton::Left),
                    b.0,
                    b.1,
                );
            }
            stream(&mut shell);
            // Complete a frame after newer stream state has already arrived.
            shell.render();
            stream(&mut shell);
            shell.state.frame_written_at(None);
            mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), b.0, b.1);
            assert_eq!(shell.copy_buffer().as_deref(), Some(payload.as_str()));
            let selection = shell.state.borrow().transcript_selection.clone();
            stream(&mut shell);
            shell.render_written_test_frame();
            assert_eq!(shell.state.borrow().transcript_selection, selection);
            assert_eq!(
                shell.selected_plain_text().as_deref(),
                Some(payload.as_str())
            );
        }
    }
}

#[test]
fn streaming_copyselect_edge_drag_copies_offscreen_rows() {
    let (mut shell, text) = fixture();
    let anchor = cell(&shell, 5, 20);
    let first_visible = cell(&shell, 0, 4);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 5, 20);
    for _ in 0..16 {
        mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 0, 4);
        stream(&mut shell);
        shell.render_written_test_frame();
    }
    mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 0, 4);
    let focus = cell(&shell, 0, 4);
    assert!(focus.offset < first_visible.offset);
    let payload = expected(&text, anchor, focus);
    assert!(payload.lines().count() > 20);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 0, 4);
    assert_eq!(shell.copy_buffer().as_deref(), Some(payload));
}

#[test]
fn streaming_copyselect_wheel_extends_active_drag_at_stationary_pointer() {
    let (mut shell, text) = fixture();
    let anchor = cell(&shell, 5, 20);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 5, 20);
    mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 2, 4);
    let original = cell(&shell, 2, 4);
    for _ in 0..6 {
        mouse(&mut shell, MouseEventKind::ScrollUp, 2, 4);
        stream(&mut shell);
        shell.render_written_test_frame();
    }
    let focus = cell(&shell, 2, 4);
    assert!(focus.offset < original.offset);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 2, 4);
    assert_eq!(
        shell.copy_buffer().as_deref(),
        Some(expected(&text, anchor, focus))
    );
}

#[test]
fn streaming_copyselect_cancelled_drag_release_never_recopies_old_range() {
    for cancel in [Event::FocusLost, Event::Resize(82, 20)] {
        let (mut shell, _) = fixture();
        mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 2, 4);
        mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 5, 20);
        shell.translate_input(Some(cancel.clone()), true);
        stream(&mut shell);
        shell.render_written_test_frame();
        mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 5, 20);
        assert_eq!(shell.copy_buffer(), None, "cancel={cancel:?}");
    }
}

fn highlighted(shell: &InteractiveShell) -> String {
    shell
        .tui
        .as_ref()
        .unwrap()
        .rendered_frame()
        .iter()
        .filter_map(|line| {
            let (_, selected) = line.split_once("\x1b[0;7m")?;
            Some(sexy_tui_rs::strip_terminal_sequences(
                selected.split("\x1b[0m").next().unwrap(),
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn streaming_copyselect_wheel_after_release_refreshes_highlight_and_clipboard() {
    let (mut shell, text) = fixture();
    let anchor = cell(&shell, 5, 20);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 5, 20);
    mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 2, 4);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 2, 4);
    let original = shell.copy_buffer();
    mouse(&mut shell, MouseEventKind::ScrollUp, 2, 4);
    stream(&mut shell);
    shell.render_written_test_frame();
    let focus = cell(&shell, 2, 4);
    let payload = expected(&text, anchor, focus);
    assert_eq!(shell.copy_buffer().as_deref(), Some(payload));
    assert_ne!(shell.copy_buffer(), original);
    assert_eq!(highlighted(&shell), payload);
}

#[test]
fn streaming_copyselect_stationary_edge_scrolls_on_timer_and_stops_on_release() {
    for bottom in [false, true] {
        let (mut shell, text) = fixture();
        let anchor = cell(&shell, 5, 20);
        let row = if bottom { 19 } else { 0 };
        mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 5, 20);
        mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), row, 4);
        let before = shell.state.borrow().scroll_from_bottom.get();
        for _ in 0..4 {
            let at = shell
                .state
                .borrow()
                .selection_autoscroll_at
                .expect("edge timer armed");
            stream(&mut shell);
            shell.render_written_test_frame();
            // Same deadline service as the real renderer loop, without sleeps.
            shell.state.borrow_mut().advance_transcript_pointer(at);
        }
        stream(&mut shell);
        shell.render_written_test_frame();
        let capacity = shell
            .state
            .borrow()
            .pointer_render_geometry()
            .unwrap()
            .viewport_rows;
        let focus = cell(&shell, row.min(capacity as u16 - 1), 4);
        let after = shell.state.borrow().scroll_from_bottom.get();
        assert!(if bottom {
            after < before
        } else {
            after > before
        });
        mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), row, 4);
        assert_eq!(
            shell.copy_buffer().as_deref(),
            Some(expected(&text, anchor, focus))
        );
        assert!(shell.state.borrow().selection_autoscroll_at.is_none());
    }
}

#[test]
fn streaming_copyselect_confirmation_expires_and_reports_bounded_terminal_copy() {
    let (mut shell, _) = fixture();
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 2, 4);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 2, 16);
    shell.render_written_test_frame();
    let expires = shell.state.borrow().copy_notice.as_ref().unwrap().1;
    assert!(shell
        .tui
        .as_ref()
        .unwrap()
        .rendered_frame()
        .iter()
        .any(|row| row.contains("Copied selection")));
    shell.state.borrow_mut().advance_transcript_pointer(expires);
    shell.render_written_test_frame();
    assert!(!shell
        .tui
        .as_ref()
        .unwrap()
        .rendered_frame()
        .iter()
        .any(|row| row.contains("Copied selection")));
    let text = format!("a{}", "🦀".repeat(20_000));
    shell
        .state
        .borrow_mut()
        .retain_transcript_copy(&text, Instant::now());
    shell.render_written_test_frame();
    assert_eq!(shell.copy_buffer().as_deref(), Some(text.as_str()));
    assert!(shell
        .tui
        .as_ref()
        .unwrap()
        .rendered_frame()
        .iter()
        .any(|row| row.contains("terminal copy truncated")));
    let mut osc = Vec::new();
    clipboard_transport::write_osc52(&mut osc, &text).unwrap();
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&osc[7..osc.len() - 1])
        .unwrap();
    assert_eq!(payload, text.as_bytes()[..49_149]);
}

#[test]
fn streaming_copyselect_stale_new_press_never_copies_retained_selection() {
    let (mut shell, _) = fixture();
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 2, 4);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 2, 16);
    shell.state.borrow_mut().copy_buffer = None;
    clipboard_transport::TEST_COPY.with(|copy| *copy.borrow_mut() = None);
    mouse(&mut shell, MouseEventKind::ScrollUp, 2, 4);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 2, 4);
    // A release before the press's own viewport has been painted cannot reuse
    // an older retained selection, even if motion was coalesced into release.
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 5, 20);
    stream(&mut shell);
    shell.render_written_test_frame();
    assert_eq!(shell.copy_buffer(), None);
}

#[test]
fn streaming_copyselect_new_motion_rejects_inflight_old_scroll_range() {
    let (mut shell, text) = fixture();
    let anchor = cell(&shell, 5, 20);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 5, 20);
    mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 2, 4);
    mouse(&mut shell, MouseEventKind::ScrollUp, 2, 4);
    shell.render();
    mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 3, 12);
    let focus = shell
        .state
        .borrow()
        .transcript_selection
        .as_ref()
        .unwrap()
        .focus;
    stream(&mut shell);
    shell.state.frame_written_at(None);
    assert_eq!(
        shell
            .state
            .borrow()
            .transcript_selection
            .as_ref()
            .unwrap()
            .focus,
        focus
    );
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 3, 12);
    assert_eq!(
        shell.copy_buffer().as_deref(),
        Some(expected(&text, anchor, focus))
    );
}

#[test]
fn streaming_copyselect_live_answer_tools_and_roster_keep_semantic_endpoints() {
    clipboard_transport::TEST_COPY.with(|copy| *copy.borrow_mut() = None);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    shell.capture_mouse = true;
    shell.state.borrow_mut().application_viewport_requested = true;
    shell.on_agent_event(&AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: format!(
            "```text\n{}",
            (0..200)
                .map(|i| format!("live row {i:03} retained text\n"))
                .collect::<String>()
        ),
    });
    shell.isolate_native_test_renderer();
    shell.render_written_test_frame();
    shell.scroll_lines(-25);
    shell.render_written_test_frame();
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 2, 4);
    stream(&mut shell);
    shell.render_written_test_frame();
    mouse(&mut shell, MouseEventKind::Drag(MouseButton::Left), 5, 20);
    let selection = shell.state.borrow().transcript_selection.clone();
    let payload = shell.selected_plain_text().unwrap();
    let id = octet_ai::ToolCallId("copy-tool".into());
    shell.on_agent_event(&AgentEvent::ToolStarted {
        id: id.clone(),
        name: "read".into(),
        args: serde_json::json!({"path":"fixture.txt"}),
    });
    shell.render_written_test_frame();
    shell.on_agent_event(&AgentEvent::ToolFinished {
        id,
        result: Ok(octet_agent::ToolOutput::new("finished outside selection")),
        duration: Duration::from_millis(10),
    });
    shell.render_written_test_frame();
    for revision in 1..=3 {
        let snapshot = serde_json::from_value(serde_json::json!({
            "revision": revision,
            "status": {"state": "active", "label": "Subagents"},
            "activities": [{"id": "worker-1", "kind": "subagent", "state": "running",
                "summary": "offline selection fixture", "metrics": {"output_tokens": revision * 100}}],
            "actions": [],
        })).unwrap();
        shell.set_subagent_presentation(Some(&snapshot), false);
        shell.state.borrow_mut().advance_event_dot_animation_by(1);
        shell.render_written_test_frame();
        assert_eq!(shell.state.borrow().transcript_selection, selection);
    }
    assert_eq!(shell.state.borrow().transcript_selection, selection);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 5, 20);
    assert_eq!(shell.copy_buffer().as_deref(), Some(payload.as_str()));
}

#[test]
fn streaming_copyselect_wheel_promotes_new_press_not_retained_old_range() {
    let (mut shell, text) = fixture();
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 2, 4);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 5, 20);
    shell.state.borrow_mut().copy_buffer = None;
    clipboard_transport::TEST_COPY.with(|copy| *copy.borrow_mut() = None);
    let anchor = cell(&shell, 4, 8);
    mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), 4, 8);
    mouse(&mut shell, MouseEventKind::ScrollUp, 4, 8);
    stream(&mut shell);
    shell.render_written_test_frame();
    assert_eq!(
        shell.copy_buffer(),
        None,
        "a held new press must not recopy the old range"
    );
    let focus = cell(&shell, 4, 8);
    mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), 4, 8);
    assert_eq!(
        shell.copy_buffer().as_deref(),
        Some(expected(&text, anchor, focus))
    );
}
