use super::*;
use crate::tui::keymap::{InputAction, PointerGesture};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use sexy_tui_rs::Component;

fn fixture() -> (InteractiveShell, ShellComponent) {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(60, 24);
    {
        let mut state = shell.state.borrow_mut();
        state.editor.set_text("editable draft");
        state.application_viewport_requested = true;
        state.render_threaded = true;
        state.startup_card_started_at = Some(Instant::now());
    }
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
        .find_map(|(row, line)| {
            line.find(text)
                .map(|at| (row as u16, visible_width(&line[..at]) as u16))
        })
        .unwrap_or_else(|| panic!("missing {text:?} in {lines:?}"))
}

fn pointer(shell: &mut InteractiveShell, kind: MouseEventKind, (row, column): (u16, u16)) {
    let action = shell.translate_input(
        Some(Event::Mouse(MouseEvent {
            kind,
            row,
            column,
            modifiers: KeyModifiers::NONE,
        })),
        shell.is_agent_run_active(),
    );
    match action {
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

fn select_composer(shell: &mut InteractiveShell, component: &ShellComponent) {
    let at = cell(&paint(shell, component), "editable");
    pointer(shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(
        shell,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    pointer(
        shell,
        MouseEventKind::Up(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    assert_eq!(shell.selected_plain_text().as_deref(), Some("edit"));
}

fn splash_word_press(shell: &mut InteractiveShell, component: &ShellComponent) -> (u16, u16) {
    let at = cell(&paint(shell, component), "octet");
    pointer(shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(shell, MouseEventKind::Up(MouseButton::Left), at);
    pointer(shell, MouseEventKind::Down(MouseButton::Left), at);
    at
}

fn assert_only_transcript(shell: &InteractiveShell) {
    let state = shell.state.borrow();
    assert!(state.transcript_selection.is_some());
    assert!(state.composer_selection.is_none());
    assert!(state.welcome_text_selection.is_none());
}

#[test]
fn composer_selection_does_not_shadow_select_all_transcript() {
    let (mut shell, component) = fixture();
    shell.notice("transcript marker");
    select_composer(&mut shell, &component);
    shell.select_all_transcript();
    assert_eq!(
        shell.copy_selected_plain_text().as_deref(),
        Some("transcript marker")
    );
    assert_only_transcript(&shell);
}

#[test]
fn composer_selection_is_cleared_by_empty_transcript_select_all() {
    let (mut shell, component) = fixture();
    select_composer(&mut shell, &component);
    shell.select_all_transcript();
    assert!(shell.selected_plain_text().is_none());
}

#[test]
fn composer_selection_is_replaced_by_transcript_drag() {
    let (mut shell, component) = fixture();
    shell.notice("transcript marker");
    select_composer(&mut shell, &component);
    let at = cell(&paint(&shell, &component), "transcript marker");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    assert!(shell.state.borrow().composer_selection.is_none());
    pointer(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 + 10),
    );
    pointer(
        &mut shell,
        MouseEventKind::Up(MouseButton::Left),
        (at.0, at.1 + 10),
    );
    assert_eq!(shell.copy_buffer().as_deref(), Some("transcript"));
    assert_only_transcript(&shell);
}

#[test]
fn splash_selection_replaces_composer_selection() {
    let (mut shell, component) = fixture();
    select_composer(&mut shell, &component);
    let at = splash_word_press(&mut shell, &component);
    assert_eq!(shell.copy_selected_plain_text().as_deref(), Some("octet"));
    assert!(shell.state.borrow().composer_selection.is_none());
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
}

#[test]
fn transcript_selection_is_replaced_by_composer_press_and_drag() {
    let (mut shell, component) = fixture();
    shell.notice("transcript marker");
    shell.select_all_transcript();
    let at = cell(&paint(&shell, &component), "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    assert!(shell.state.borrow().transcript_selection.is_none());
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
    assert_eq!(shell.copy_buffer().as_deref(), Some("edit"));
}

#[test]
fn splash_selection_is_replaced_by_composer_press() {
    let (mut shell, component) = fixture();
    let at = splash_word_press(&mut shell, &component);
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(shell.selected_plain_text().as_deref(), Some("octet"));
    let at = cell(&paint(&shell, &component), "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    assert!(shell.state.borrow().welcome_text_selection.is_none());
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
    assert_eq!(shell.selected_plain_text().as_deref(), Some("edit"));
    assert!(shell.state.borrow().welcome_text_selection.is_none());
}

#[test]
fn splash_mark_clears_composer_selection_without_copying() {
    for historical in [false, true] {
        let (mut shell, component) = fixture();
        if historical {
            shell.notice("transcript marker");
        }
        select_composer(&mut shell, &component);
        paint(&shell, &component);
        let (x, y) = shell
            .state
            .borrow()
            .pointer_render_geometry()
            .unwrap()
            .welcome_mark
            .unwrap();
        let at = (y as u16, x);
        pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
        assert!(shell.selected_plain_text().is_none());
        shell.state.borrow_mut().copy_buffer = None;
        pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
        assert!(shell.copy_buffer().is_none());
    }
}

#[test]
fn splash_word_release_copies_selected_word_once() {
    for streaming in [false, true] {
        let (mut shell, component) = fixture();
        if streaming {
            shell.begin_run("fixture");
        }
        let at = splash_word_press(&mut shell, &component);
        assert!(shell.copy_buffer().is_none(), "press must not copy");
        if streaming {
            shell.on_agent_event(&AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "output below splash".into(),
            });
        }
        paint(&shell, &component);
        pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
        assert_eq!(shell.selected_plain_text().as_deref(), Some("octet"));
        assert_eq!(shell.copy_buffer().as_deref(), Some("octet"));
        shell.state.borrow_mut().copy_buffer = None;
        pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
        assert!(
            shell.copy_buffer().is_none(),
            "unowned release must not recopy"
        );
    }
}

#[test]
fn splash_word_release_stays_row_local_after_motion() {
    let (mut shell, component) = fixture();
    let at = splash_word_press(&mut shell, &component);
    let composer = cell(&paint(&shell, &component), "editable");
    pointer(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        composer,
    );
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), composer);
    assert_eq!(shell.copy_buffer().as_deref(), Some("octet"));
    assert!(shell.state.borrow().transcript_selection.is_none());
    assert!(shell.state.borrow().composer_selection.is_none());
    assert_ne!(at, composer);
}

#[test]
fn select_all_cancels_displaced_composer_gesture_before_release() {
    let (mut shell, component) = fixture();
    shell.notice("transcript marker");
    let at = cell(&paint(&shell, &component), "editable");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    pointer(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    shell.select_all_transcript();
    pointer(
        &mut shell,
        MouseEventKind::Up(MouseButton::Left),
        (at.0, at.1 + 4),
    );
    assert!(
        shell.copy_buffer().is_none(),
        "displaced release must not copy"
    );
    assert_only_transcript(&shell);
}

#[test]
fn splash_and_transcript_selections_displace_each_others_pending_release() {
    let (mut shell, component) = fixture();
    shell.notice("transcript marker");
    shell.select_all_transcript();
    let at = cell(&paint(&shell, &component), "octet");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    assert!(shell.selected_plain_text().is_none());
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert!(
        shell.copy_buffer().is_none(),
        "single splash click must not copy"
    );
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    assert_eq!(shell.selected_plain_text().as_deref(), Some("octet"));
    assert!(shell.state.borrow().transcript_selection.is_none());
    shell.select_all_transcript();
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert!(
        shell.copy_buffer().is_none(),
        "displaced splash release must not copy"
    );
    assert_only_transcript(&shell);
}

#[test]
fn completion_press_clears_transcript_selection_before_acceptance() {
    let (mut shell, component) = fixture();
    shell.notice("transcript marker");
    shell.state.borrow_mut().extension_commands =
        vec![("fixture-command".into(), "Fixture".into())].into();
    shell.state.borrow_mut().editor.set_text("/fixture-");
    shell.select_all_transcript();
    let at = cell(&paint(&shell, &component), "fixture-command");
    pointer(&mut shell, MouseEventKind::Down(MouseButton::Left), at);
    assert!(shell.selected_plain_text().is_none());
    assert!(shell.state.borrow().transcript_selection.is_none());
    pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
    assert_eq!(shell.pending(), "/fixture-command ");
    assert!(shell.copy_buffer().is_none());
}

#[test]
fn splash_word_release_is_cancelled_by_intervening_input() {
    for event in [
        Event::FocusLost,
        Event::Resize(61, 24),
        Event::Key(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)),
    ] {
        let (mut shell, component) = fixture();
        let at = splash_word_press(&mut shell, &component);
        shell.translate_input(Some(event), false);
        pointer(&mut shell, MouseEventKind::Up(MouseButton::Left), at);
        assert!(shell.copy_buffer().is_none());
    }
}
