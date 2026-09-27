use super::*;

struct RecordingTerminal {
    writes: Arc<Mutex<String>>,
    capabilities: sexy_tui_rs::TerminalCapabilities,
}

impl sexy_tui_rs::Terminal for RecordingTerminal {
    fn start_events(&mut self, _: Box<dyn FnMut(sexy_tui_rs::TerminalInput)>, _: Box<dyn FnMut()>) {
    }
    fn stop(&mut self) {}
    fn write(&mut self, data: &str) {
        self.writes.lock().unwrap().push_str(data);
    }
    fn columns(&self) -> u16 {
        80
    }
    fn rows(&self) -> u16 {
        24
    }
    fn move_by(&mut self, _: i16) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn capabilities(&self) -> sexy_tui_rs::TerminalCapabilities {
        self.capabilities
    }
}

fn recording_tui(
    capabilities: sexy_tui_rs::TerminalCapabilities,
) -> (TUI<'static>, Arc<Mutex<String>>) {
    let writes = Arc::new(Mutex::new(String::new()));
    let tui = TUI::new(Box::new(RecordingTerminal {
        writes: writes.clone(),
        capabilities,
    }));
    (tui, writes)
}

fn interactive_capabilities() -> sexy_tui_rs::TerminalCapabilities {
    sexy_tui_rs::TerminalCapabilities::interactive(sexy_tui_rs::ColorDepth::TrueColor, true)
}

#[test]
fn title_waits_for_ready_name_and_changes_only_when_session_changes() {
    let state = SharedState::new(ShellState {
        startup_pending: true,
        session_name: Some("Release \x07\x1breview".into()),
        ..ShellState::default()
    });
    let (mut tui, writes) = recording_tui(interactive_capabilities());
    let mut last_title = None;

    sync_window_title(&mut tui, &state, &mut last_title);
    assert!(writes.lock().unwrap().is_empty(), "startup wrote OSC 2");

    state.borrow_mut().startup_pending = false;
    sync_window_title(&mut tui, &state, &mut last_title);
    assert_eq!(
        &*writes.lock().unwrap(),
        "\x1b]2;octet · Release review\x07"
    );
    sync_window_title(&mut tui, &state, &mut last_title);
    assert_eq!(writes.lock().unwrap().matches("\x1b]2;").count(), 1);

    state.borrow_mut().session_name = Some("Next".into());
    sync_window_title(&mut tui, &state, &mut last_title);
    state.borrow_mut().session_name = None;
    sync_window_title(&mut tui, &state, &mut last_title);
    sync_window_title(&mut tui, &state, &mut last_title);
    assert_eq!(
        &*writes.lock().unwrap(),
        "\x1b]2;octet · Release review\x07\x1b]2;octet · Next\x07\x1b]2;octet\x07"
    );

    // A new renderer (e.g. after terminal suspension) must restore the title.
    let (mut resumed, resumed_writes) = recording_tui(interactive_capabilities());
    sync_window_title(&mut resumed, &state, &mut None);
    assert_eq!(&*resumed_writes.lock().unwrap(), "\x1b]2;octet\x07");
}

#[test]
fn plain_terminal_does_not_receive_a_title_sequence() {
    let state = SharedState::new(ShellState {
        session_name: Some("Private".into()),
        ..ShellState::default()
    });
    let (mut tui, writes) = recording_tui(sexy_tui_rs::TerminalCapabilities::plain());
    sync_window_title(&mut tui, &state, &mut None);
    assert!(writes.lock().unwrap().is_empty());
}

#[test]
fn production_render_loop_writes_title_after_ready_paint() {
    let state = SharedState::new(ShellState {
        theme: crate::tui::theme::test_theme(),
        size: (80, 24),
        startup_pending: true,
        follow_tail: true,
        ..ShellState::default()
    });
    let size = Arc::new(Mutex::new((80, 24)));
    let writes = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = mpsc::channel();
    let polls = std::cell::Cell::new(0);
    render_loop_with_terminal(
        RecordingTerminal {
            writes: writes.clone(),
            capabilities: interactive_capabilities(),
        },
        state.clone(),
        size,
        rx,
        RenderLoopOptions::default(),
        |state, _| {
            if polls.get() == 0 {
                assert!(!writes.lock().unwrap().contains("\x1b]2;"));
                let mut shell = state.borrow_mut();
                shell.session_name = Some("Resolved".into());
                shell.startup_pending = false;
                tx.send(RenderCommand::Render).unwrap();
            } else {
                assert!(writes
                    .lock()
                    .unwrap()
                    .contains("\x1b]2;octet · Resolved\x07"));
                tx.send(RenderCommand::Stop).unwrap();
            }
            polls.set(polls.get() + 1);
            false
        },
    );
    assert_eq!(writes.lock().unwrap().matches("\x1b]2;").count(), 1);
}
