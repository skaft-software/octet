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
fn native_renderer_drains_desktop_notification_intents_exactly_once() {
    let state = SharedState::new(ShellState {
        theme: crate::tui::theme::test_theme(),
        size: (80, 24),
        desktop_notifications: vec![("Native".into(), "Completed".into())],
        ..ShellState::default()
    });
    let hello = octet_tern::frame::decode_body(
        "r",
        &serde_json::json!({"r":"hello", "v":1, "term":"test",
            "kinds":octet_tern::wire::TSP_KINDS, "credits":4, "dark":true})
        .to_string(),
    )
    .unwrap();
    state.native().lock().unwrap().messages.push_back(hello);
    let client =
        octet_tern::client::TernClient::with_writer("test", None, &["edit"], std::io::sink())
            .unwrap();
    let surface = super::super::tern::TernSurface::with_client(client);
    let writes = Arc::new(Mutex::new(String::new()));
    let mut terminal = RecordingTerminal {
        writes: writes.clone(),
        capabilities: interactive_capabilities(),
    };
    let (tx, rx) = mpsc::channel();
    tx.send(RenderCommand::Stop).unwrap();
    render_native_loop_with_surface(
        &mut terminal,
        &state,
        &Arc::new(Mutex::new((80, 24))),
        &rx,
        &|_, _| false,
        surface,
    )
    .unwrap();
    assert_eq!(
        writes
            .lock()
            .unwrap()
            .matches("\x1b]777;notify;Native;Completed\x07")
            .count(),
        1
    );
    assert!(state.borrow().desktop_notifications.is_empty());
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

fn status_records(output: &str) -> Vec<&str> {
    output
        .split("\x1b]7501;")
        .skip(1)
        .map(|part| part.split("\x1b\\").next().unwrap())
        .collect()
}

#[test]
fn terminal_chrome_dedupes_and_preserves_done_across_renderer_replacement() {
    use sexy_tui_rs::program_status::{BlockedKind, ProgramStatus};
    let state = SharedState::new(ShellState::default());
    let (mut tui, writes) = recording_tui(interactive_capabilities());
    sync_terminal_chrome(&mut tui, &state, &mut None);
    sync_terminal_chrome(&mut tui, &state, &mut None);
    state.borrow_mut().terminal_operation_status = Some(ProgramStatus::Blocked(BlockedKind::Auth));
    sync_terminal_chrome(&mut tui, &state, &mut None);
    handoff_program_status(&mut tui, &state);
    let (mut resumed, resumed_writes) = recording_tui(interactive_capabilities());
    sync_terminal_chrome(&mut resumed, &state, &mut None);
    state.borrow_mut().terminal_operation_status = Some(ProgramStatus::Done);
    sync_terminal_chrome(&mut resumed, &state, &mut None);
    handoff_program_status(&mut resumed, &state);
    sync_terminal_chrome(&mut tui, &state, &mut None);
    assert_eq!(
        status_records(&writes.lock().unwrap()),
        [
            "state=idle:app=octet",
            "state=blocked:kind=auth:app=octet",
            "state=idle:app=octet"
        ]
    );
    assert_eq!(
        status_records(&resumed_writes.lock().unwrap()),
        ["state=blocked:kind=auth:app=octet", "state=done:app=octet"]
    );
}

#[test]
fn terminal_status_is_absent_in_plain_and_noninteractive_outputs() {
    for mut capabilities in [
        sexy_tui_rs::TerminalCapabilities::plain(),
        interactive_capabilities(),
    ] {
        if !capabilities.plain {
            capabilities.interactive = false;
        }
        let state = SharedState::new(ShellState::default());
        let (mut tui, writes) = recording_tui(capabilities);
        sync_terminal_chrome(&mut tui, &state, &mut None);
        assert!(writes.lock().unwrap().is_empty());
    }
}

#[test]
fn terminal_status_tern_progress_is_indeterminate_paused_or_failed_never_zero_percent() {
    use sexy_tui_rs::program_status::{BlockedKind, ProgramStatus};
    let writes = Arc::new(Mutex::new(String::new()));
    let mut terminal = RecordingTerminal {
        writes: writes.clone(),
        capabilities: interactive_capabilities(),
    };
    for status in [
        ProgramStatus::Working,
        ProgramStatus::Blocked(BlockedKind::Permission),
        ProgramStatus::Error,
        ProgramStatus::Done,
    ] {
        terminal.set_program_status(status, "octet", true);
    }
    let output = writes.lock().unwrap();
    assert!(output.contains("\x1b]9;4;3\x1b\\"));
    assert!(output.contains("\x1b]9;4;4\x1b\\"));
    assert!(output.contains("\x1b]9;4;2\x1b\\"));
    assert!(output.contains("\x1b]9;4;0\x1b\\"));
    assert!(!output.contains(";1;0"));
    assert!(!output.contains("progress="));
    assert_eq!(
        status_records(&output).last(),
        Some(&"state=done:app=octet")
    );
}

#[test]
fn ansi_and_native_stop_suspend_retire_live_activity_but_preserve_results() {
    use sexy_tui_rs::program_status::{BlockedKind, ProgramStatus};
    for native in [false, true] {
        for suspend in [false, true] {
            for status in [
                ProgramStatus::Working,
                ProgramStatus::Blocked(BlockedKind::Permission),
                ProgramStatus::Done,
                ProgramStatus::Error,
            ] {
                let state = SharedState::new(ShellState {
                    theme: crate::tui::theme::test_theme(),
                    size: (80, 24),
                    terminal_operation_status: Some(status),
                    ..ShellState::default()
                });
                let writes = Arc::new(Mutex::new(String::new()));
                let mut terminal = RecordingTerminal {
                    writes: writes.clone(),
                    capabilities: interactive_capabilities(),
                };
                let (tx, rx) = mpsc::channel();
                let (ack, ack_rx) = mpsc::channel();
                tx.send(if suspend {
                    RenderCommand::Suspend(ack)
                } else {
                    RenderCommand::Stop
                })
                .unwrap();
                let size = Arc::new(Mutex::new((80, 24)));
                if native {
                    let hello = octet_tern::frame::decode_body("r", &serde_json::json!({"r":"hello", "v":1, "term":"test", "kinds":octet_tern::wire::TSP_KINDS, "credits":4, "dark":true}).to_string()).unwrap();
                    state.native().lock().unwrap().messages.push_back(hello);
                    let client = octet_tern::client::TernClient::with_writer(
                        "octet",
                        None,
                        &["edit"],
                        std::io::sink(),
                    )
                    .unwrap();
                    let surface = super::super::tern::TernSurface::with_client(client);
                    render_native_loop_with_surface(
                        &mut terminal,
                        &state,
                        &size,
                        &rx,
                        &|_, _| false,
                        surface,
                    )
                    .unwrap();
                } else {
                    render_loop_with_terminal(
                        terminal,
                        state,
                        size,
                        rx,
                        RenderLoopOptions::default(),
                        |_, _| false,
                    );
                }
                if suspend {
                    ack_rx.try_recv().unwrap();
                }
                let output = writes.lock().unwrap();
                let records = status_records(&output);
                let expected = if status.is_transient() {
                    ProgramStatus::Idle
                } else {
                    status
                };
                let sequence = expected.sequence("octet").unwrap();
                assert_eq!(
                    records.last(),
                    status_records(&sequence).last(),
                    "native={native} suspend={suspend} {status:?}"
                );
                assert_eq!(records.len(), if status.is_transient() { 2 } else { 1 });
                assert!(!output.contains("state=clear"));
            }
        }
    }
}
