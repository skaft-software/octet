use super::*;
use crate::tui::keymap::{InputAction, PointerGesture};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use sexy_tui_rs::Component;

fn fixture(text: &str, width: u16, height: u16) -> (InteractiveShell, ShellComponent) {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(width, height);
    shell.state.borrow_mut().editor.set_text(text);
    shell.state.borrow_mut().application_viewport_requested = true;
    shell.state.borrow_mut().render_threaded = true;
    let component = ShellComponent::isolated(shell.state.clone(), false);
    (shell, component)
}

fn paint(shell: &InteractiveShell, component: &ShellComponent) -> Vec<String> {
    let width = shell.state.borrow().size.0;
    let lines = component.render(width);
    shell.state.frame_written_at(None);
    lines
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect()
}

fn cell(lines: &[String], text: &str) -> (u16, u16) {
    lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(row, line)| {
            line.find(text)
                .map(|at| (row as u16, visible_width(&line[..at]) as u16))
        })
        .unwrap_or_else(|| panic!("missing {text:?} in {lines:?}"))
}

fn pointer(shell: &mut InteractiveShell, kind: MouseEventKind, (row, column): (u16, u16)) {
    let active = shell.state.borrow().run.is_active();
    let action = shell.translate_input(
        Some(Event::Mouse(MouseEvent {
            kind,
            row,
            column,
            modifiers: KeyModifiers::NONE,
        })),
        active,
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
}

fn click(shell: &mut InteractiveShell, at: (u16, u16)) {
    let before = shell.state.borrow().editor.cursor();
    pointer(shell, MouseEventKind::Down(MouseButton::Left), at);
    assert_eq!(
        shell.state.borrow().editor.cursor(),
        before,
        "press is selection-owned"
    );
    pointer(shell, MouseEventKind::Up(MouseButton::Left), at);
}

#[test]
fn composer_click_wrapped_line_uses_painted_segment() {
    let text = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let (mut shell, component) = fixture(text, 24, 24);
    let width = composer_editor_geometry(&shell.state.borrow(), 24).text_width();
    let lines = paint(&shell, &component);
    let at = cell(&lines, &text[width..width + 3]);
    click(&mut shell, (at.0, at.1 + 2));
    assert_eq!(shell.state.borrow().editor.cursor(), width + 2);
}

#[test]
fn composer_click_wide_combining_and_tab_cells_are_grapheme_safe() {
    for (column, expected) in [(1, 1), (2, 1), (3, 4), (4, 7), (6, 7), (8, 8)] {
        let (mut shell, component) = fixture("a界e\u{301}\tZ", 40, 24);
        let lines = paint(&shell, &component);
        let at = cell(&lines, "a界");
        click(&mut shell, (at.0, at.1 + column));
        assert_eq!(
            shell.state.borrow().editor.cursor(),
            expected,
            "column {column}"
        );
    }
}

#[test]
fn composer_click_past_end_distinguishes_soft_wrap_and_logical_end() {
    let text = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\nshort";
    let (mut shell, component) = fixture(text, 24, 24);
    let width = composer_editor_geometry(&shell.state.borrow(), 24).text_width();
    let lines = paint(&shell, &component);
    let at = cell(&lines, "abc");
    click(&mut shell, (at.0, 23));
    assert_eq!(shell.state.borrow().editor.cursor(), width - 1);
    let lines = paint(&shell, &component);
    let at = cell(&lines, "short");
    click(&mut shell, (at.0, 23));
    assert_eq!(shell.state.borrow().editor.cursor(), text.len());
}

#[test]
fn composer_click_scrolled_multiline_uses_visible_row_offset() {
    let text = (0..30)
        .map(|n| format!("row{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (mut shell, component) = fixture(&text, 40, 18);
    let lines = paint(&shell, &component);
    assert!(!lines.iter().any(|line| line.contains("row00")));
    let at = cell(&lines, "row28");
    click(&mut shell, (at.0, at.1 + 2));
    assert_eq!(
        shell.state.borrow().editor.cursor(),
        text.find("row28").unwrap() + 2
    );
}

#[test]
fn composer_click_while_streaming_survives_unpainted_output() {
    let (mut shell, component) = fixture("editable draft", 40, 24);
    shell.begin_run("openai");
    let lines = paint(&shell, &component);
    let at = cell(&lines, "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    shell.on_agent_event(&AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: "new streaming output".into(),
    });
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(shell.state.borrow().editor.cursor(), 0);
    assert!(shell.state.borrow().run.is_active());
}

#[test]
fn composer_click_drag_selects_without_moving_cursor_or_clicking_on_return() {
    let (mut shell, component) = fixture("editable draft", 40, 24);
    let lines = paint(&shell, &component);
    let at = cell(&lines, "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    pointer(
        &mut shell,
        MouseEventKind::Up(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    assert_eq!(shell.state.borrow().editor.cursor(), "editable draft".len());
    assert_eq!(shell.state.borrow().copy_buffer.as_deref(), Some("edit"));
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(shell.state.borrow().editor.cursor(), "editable draft".len());
}

#[test]
fn composer_drag_wheel_release_keeps_selection_and_copies() {
    for active in [false, true] {
        for wheel in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
            clipboard_transport::TEST_COPY.with(|copy| *copy.borrow_mut() = None);
            let (mut shell, component) = fixture("editable draft", 40, 24);
            shell.capture_mouse = true;
            shell.state.borrow_mut().push_block(TranscriptBlock::Notice(
                (0..80).map(|n| format!("history row {n}\n")).collect(),
            ));
            if active {
                shell.begin_run("openai");
            }
            paint(&shell, &component);
            shell.scroll_lines(-5);
            let rows = paint(&shell, &component);
            let at = cell(&rows, "editable");
            pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
            pointer(
                &mut shell,
                MouseEventKind::Drag(MouseButton::Left),
                (at.0, at.1 + 4),
            );
            let before = shell.state.borrow().scroll_from_bottom.get();
            pointer(&mut shell, wheel, (at.0, at.1 + 4));
            assert_ne!(shell.state.borrow().scroll_from_bottom.get(), before);
            paint(&shell, &component);
            pointer(
                &mut shell,
                MouseEventKind::Drag(MouseButton::Left),
                (at.0, at.1 + 8),
            );
            pointer(
                &mut shell,
                MouseEventKind::Up(MouseButton::Left),
                (at.0, at.1 + 8),
            );
            assert_eq!(shell.state.borrow().editor.cursor(), "editable draft".len());
            assert_eq!(
                shell.state.borrow().copy_buffer.as_deref(),
                Some("editable"),
                "active={active}, wheel={wheel:?}"
            );
            clipboard_transport::TEST_COPY
                .with(|copy| assert_eq!(copy.borrow().as_deref(), Some("editable")));
            assert!(shell.state.borrow().pending_screen_press.is_none());
        }
    }
}

#[test]
fn composer_drag_wheel_still_cancels_on_replacement_input() {
    for cancel in ["focus", "resize", "edit", "press"] {
        let (mut shell, component) = fixture("editable draft", 40, 24);
        shell.capture_mouse = true;
        let rows = paint(&shell, &component);
        let at = cell(&rows, "editable");
        pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
        pointer(
            &mut shell,
            MouseEventKind::Drag(MouseButton::Left),
            (at.0, at.1 + 4),
        );
        pointer(&mut shell, MouseEventKind::ScrollUp, at);
        match cancel {
            "focus" => {
                shell.translate_input(Some(Event::FocusLost), false);
            }
            "resize" => shell.set_size(39, 24),
            "edit" => shell.apply_edit(EditAction::Char('!')),
            "press" => pointer(&mut shell, MouseEventKind::Down(MouseButton::Right), at),
            _ => unreachable!(),
        }
        pointer(
            &mut shell,
            MouseEventKind::Up(MouseButton::Left),
            (at.0, at.1 + 4),
        );
        assert!(shell.state.borrow().copy_buffer.is_none(), "{cancel}");
    }
}

#[test]
fn composer_click_wheel_invalidates_cursor_placement() {
    let (mut shell, component) = fixture("editable draft", 40, 24);
    shell.capture_mouse = true;
    let rows = paint(&shell, &component);
    let at = cell(&rows, "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(&mut shell, MouseEventKind::ScrollUp, at);
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(shell.state.borrow().editor.cursor(), "editable draft".len());
    assert!(shell.state.borrow().copy_buffer.is_none());
}

#[test]
fn composer_click_snaps_attachment_and_paste_placeholders_to_nearest_boundary() {
    for paste in [false, true] {
        for right in [false, true] {
            let (mut shell, component) = fixture("", 80, 24);
            let chip = if paste {
                shell
                    .state
                    .borrow_mut()
                    .ledger
                    .attach_pasted_text("payload\n".repeat(20))
            } else {
                let chip = "[Image 界 #1]".to_owned();
                shell
                    .state
                    .borrow_mut()
                    .ledger
                    .restore(vec![composer::Attachment {
                        id: 1,
                        chip: chip.clone(),
                        payload: composer::AttachmentPayload::FileReference(
                            "/fixture/image.png".into(),
                        ),
                    }]);
                chip
            };
            shell
                .state
                .borrow_mut()
                .editor
                .set_text(format!("before {chip} after"));
            let lines = paint(&shell, &component);
            let at = cell(&lines, &chip);
            let offset = if right {
                visible_width(&chip) as u16 - 2
            } else {
                2
            };
            click(&mut shell, (at.0, at.1 + offset));
            assert_eq!(
                shell.state.borrow().editor.cursor(),
                7 + if right { chip.len() } else { 0 }
            );
            assert_eq!(shell.state.borrow().ledger.entries.len(), 1);
        }
    }
}

#[test]
fn composer_click_chrome_and_compact_windows_match_paint() {
    for chrome in ["boxed", "framed", "shaded", "topline"] {
        for width in [8, 30, 160] {
            let text = (0..20)
                .map(|n| format!("row{n:02}"))
                .collect::<Vec<_>>()
                .join("\n");
            let (mut shell, component) = fixture(&text, width, 18);
            shell.state.borrow_mut().theme = crate::tui::theme::test_theme_from_source(&format!(
                "[colors]\ncomposer = \"{chrome}\"\ncontent_max_width = 60\n[layout]\ntranscript_inset = 2\nshow_footer = true"
            ));
            let lines = paint(&shell, &component);
            let at = cell(&lines, "row18");
            click(&mut shell, (at.0, at.1 + 2));
            assert_eq!(
                shell.state.borrow().editor.cursor(),
                text.find("row18").unwrap() + 2,
                "{chrome} at {width}"
            );
        }
    }
}

#[test]
fn composer_click_cancels_after_input_resize_or_motion_and_ignores_other_buttons() {
    for cancel in ["key", "resize", "motion", "right"] {
        let (mut shell, component) = fixture("editable draft", 40, 24);
        let lines = paint(&shell, &component);
        let at = cell(&lines, "editable");
        let button = if cancel == "right" {
            MouseButton::Right
        } else {
            MouseButton::Left
        };
        pointer(&mut shell, MouseEventKind::Down(button), at);
        match cancel {
            "key" => {
                shell.translate_input(
                    Some(Event::Key(crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Null,
                        KeyModifiers::NONE,
                    ))),
                    false,
                );
            }
            "resize" => shell.set_size(39, 24),
            "motion" => {
                pointer(&mut shell, MouseEventKind::Drag(button), (at.0, at.1 + 2));
                pointer(&mut shell, MouseEventKind::Drag(button), at);
            }
            _ => {}
        }
        pointer(&mut shell, MouseEventKind::Up(button), at);
        assert_eq!(
            shell.state.borrow().editor.cursor(),
            "editable draft".len(),
            "{cancel}"
        );
    }
}

#[test]
fn composer_click_ends_history_and_undo_coalescing_and_refreshes_completion() {
    let (mut shell, component) = fixture("", 40, 24);
    shell.on_prompt_submitted("old prompt");
    shell.apply_edit(EditAction::Up);
    assert!(shell.state.borrow().prompt_history_navigation.is_some());
    let lines = paint(&shell, &component);
    let at = cell(&lines, "old prompt");
    click(&mut shell, (at.0, at.1 + 3));
    assert!(shell.state.borrow().prompt_history_navigation.is_none());
    shell.clear_editor();
    for ch in "abc".chars() {
        shell.apply_edit(EditAction::Char(ch));
    }
    let lines = paint(&shell, &component);
    let at = cell(&lines, "abc");
    click(&mut shell, (at.0, at.1 + 3));
    shell.apply_edit(EditAction::Char('d'));
    shell.apply_edit(EditAction::Undo);
    assert_eq!(shell.pending(), "abc");
    let snapshot = shell.extension_editor_snapshot();
    assert!(shell.set_extension_autocomplete(
        &snapshot,
        "abc".into(),
        vec![ShellAutocompleteItem {
            value: "abcd".into(),
            label: "abcd".into(),
            description: None,
            replace_after_bytes: None,
            cursor_offset_bytes: None,
        }]
    ));
    let lines = paint(&shell, &component);
    let at = cell(&lines, "abc ");
    click(&mut shell, at);
    assert!(shell.state.borrow().extension_autocomplete.is_none());
    assert_ne!(shell.extension_editor_revision(), snapshot.revision);
}

#[test]
fn composer_click_in_completion_list_accepts_item_without_positioning_native_cursor() {
    let (mut shell, component) = fixture("/", 60, 24);
    shell.state.borrow_mut().extension_commands =
        vec![("fixture-command".into(), "Fixture".into())].into();
    shell.state.borrow_mut().editor.set_text("/fixture-");
    let lines = paint(&shell, &component);
    let at = cell(&lines, "fixture-command");
    click(&mut shell, at);
    assert_eq!(shell.pending(), "/fixture-command ");
}

#[test]
fn composer_click_preserves_custom_editor_native_mirror_and_list_region_ownership() {
    use crate::extensions::remote_ui::{MountView, Projection};
    let (mut shell, component) = fixture("kept native mirror", 40, 24);
    let lines = paint(&shell, &component);
    let at = cell(&lines, "kept native");
    shell.set_remote_ui(Projection {
        mounts: vec![MountView {
            id: "fixture.editor".into(),
            surface_id: "fixture".into(),
            title: "Custom editor".into(),
            placement: octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Editor,
            columns: 40,
            rows: 24,
            mouse_capture: false,
            blocked: None,
            native_editor: false,
        }],
        ..Projection::default()
    });
    click(&mut shell, at);
    assert_eq!(
        shell.state.borrow().editor.cursor(),
        "kept native mirror".len()
    );
}

#[test]
fn composer_click_streaming_repaints_keep_pressed_geometry_and_selection_highlight() {
    let (mut shell, component) = fixture("editable draft", 40, 24);
    shell.begin_run("openai");
    let lines = paint(&shell, &component);
    let at = cell(&lines, "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    shell.on_agent_event(&AgentEvent::OutputDelta {
        channel: OutputChannel::Text,
        text: "streamed output changes above composer".into(),
    });
    paint(&shell, &component);
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(shell.state.borrow().editor.cursor(), 0);
    let lines = paint(&shell, &component);
    let at = cell(&lines, "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    let rows = component.render(40);
    assert!(
        rows.iter().any(|line| line.contains("\x1b[7medit\x1b[27m")),
        "{rows:?}"
    );
    shell.state.frame_written_at(None);
    pointer(
        &mut shell,
        MouseEventKind::Up(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    assert_eq!(shell.state.borrow().copy_buffer.as_deref(), Some("edit"));
    assert_eq!(shell.state.borrow().editor.cursor(), 0);
}

#[test]
fn composer_click_tern_caret_events_keep_chips_atomic_while_streaming() {
    use octet_tern::frame::Incoming;
    use octet_tern::wire::Event as TernEvent;
    let (mut shell, _) = fixture("", 40, 24);
    shell.begin_run("openai");
    let chip = shell
        .state
        .borrow_mut()
        .ledger
        .attach_pasted_text("private payload".into());
    let text = format!("界 {chip} after");
    shell.state.borrow_mut().editor.set_text(&text);
    shell.state.native().lock().unwrap().accepting_input = true;
    let handler = shell.tern_input_handler();
    let start_utf16 = "界 ".encode_utf16().count();
    for (offset, expected) in [(2, "界 ".len()), (chip.len() - 2, "界 ".len() + chip.len())] {
        handler(Incoming::Event(TernEvent::Edit {
            sf: tern::SURFACE.into(),
            id: "composer.editor".into(),
            from: start_utf16 + offset,
            to: start_utf16 + offset,
            text: "".into(),
            cursor: start_utf16 + offset,
            len: text.encode_utf16().count(),
        }));
        assert_eq!(shell.state.borrow().editor.cursor(), expected);
        assert_eq!(shell.pending(), text);
    }
}

#[test]
fn composer_click_on_chrome_or_without_written_geometry_only_focuses() {
    for painted in [false, true] {
        let (mut shell, component) = fixture("editable draft", 40, 24);
        let rows = paint(&shell, &component);
        let at = cell(&rows, "editable");
        if !painted {
            shell.state.borrow_mut().render_geometry = None;
            click(&mut shell, at);
        } else {
            click(&mut shell, (at.0 - 1, at.1));
            click(&mut shell, (at.0 + 1, at.1));
        }
        assert_eq!(shell.state.borrow().editor.cursor(), "editable draft".len());
    }
}

#[test]
fn composer_click_inside_wrapped_placeholder_remains_atomic() {
    for right in [false, true] {
        let (mut shell, component) = fixture("", 24, 24);
        let chip = shell
            .state
            .borrow_mut()
            .ledger
            .attach_pasted_text("payload\n".repeat(20));
        shell
            .state
            .borrow_mut()
            .editor
            .set_text(format!("before {chip} after"));
        let rows = paint(&shell, &component);
        let at = cell(&rows, if right { "lines]" } else { "Pasted" });
        click(&mut shell, at);
        assert_eq!(
            shell.state.borrow().editor.cursor(),
            7 + if right { chip.len() } else { 0 }
        );
    }
}

#[test]
fn paste_chips_delete_whole_and_arrows_step_over_them() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 30);
    shell.apply_edit(EditAction::Char('a'));
    let large = (0..40).map(|n| format!("line {n}\n")).collect::<String>();
    shell.apply_edit(EditAction::Paste(large));
    shell.apply_edit(EditAction::Char('b'));
    let text = shell.state.borrow().editor.text().to_owned();
    let chip_start = 1;
    let chip_end = text.len() - 1;
    assert!(
        text[chip_start..chip_end].starts_with("[Pasted text #"),
        "{text}"
    );
    // Left from after the chip lands before it in one step, then back over it.
    shell.apply_edit(EditAction::Left);
    assert_eq!(shell.state.borrow().editor.cursor(), chip_end);
    shell.apply_edit(EditAction::Left);
    assert_eq!(shell.state.borrow().editor.cursor(), chip_start);
    shell.apply_edit(EditAction::Right);
    assert_eq!(shell.state.borrow().editor.cursor(), chip_end);
    // One Backspace removes the whole chip, never a single letter of it.
    shell.apply_edit(EditAction::Backspace);
    assert_eq!(shell.state.borrow().editor.text(), "ab");
    assert_eq!(shell.state.borrow().editor.cursor(), 1);
    // Undo restores the chip as one unit.
    shell.apply_edit(EditAction::Undo);
    assert_eq!(shell.state.borrow().editor.text(), text);
}

#[test]
fn native_down_does_not_split_a_real_paste_chip() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 30);
    let chip = shell
        .state
        .borrow_mut()
        .ledger
        .attach_pasted_text("line\n".repeat(40));
    let before = format!("abc\n{chip}");
    shell.state.borrow_mut().editor.set_text(&before);
    shell.state.borrow_mut().editor.set_cursor(2);
    shell.apply_edit(EditAction::Down);
    let cursor = shell.state.borrow().editor.cursor();
    shell.apply_edit(EditAction::Char('X'));
    assert!(
        [4, before.len()].contains(&cursor),
        "native cursor={cursor}"
    );
    assert!(shell.state.borrow().editor.text().contains(&chip));
}
