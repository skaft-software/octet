//! Use the production renderer factory, not a separately configured test TUI.
use std::cell::{Cell, RefCell};

use super::super::renderer_runtime::{
    reconcile_terminal_size, render_loop_with_terminal, RenderLoopOptions,
};
use super::support::*;
use super::*;

fn check_native_repair(
    resizes: &[(u16, u16)],
    insert_historical: bool,
    appended: usize,
    wait_for_live_paint: bool,
) {
    let dimensions = resizes.last().copied().unwrap_or((80, 24));
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    shell.tui.take().unwrap().stop();
    for index in 0..256 {
        shell.notice(format!("SAVED-{index:03}"));
    }
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let repair_start = Cell::new(None);
    let live_repair = Cell::new(false);
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    render_loop_with_terminal(
        EmulatedTerminal {
            size: shell.size.clone(),
            bytes: bytes.clone(),
            synchronized_output: true,
            status_frames: None,
        },
        shell.state.clone(),
        shell.size.clone(),
        rx,
        RenderLoopOptions::default(),
        |state, size| {
            if let Some(start) = repair_start.get() {
                let painted = bytes.lock().unwrap()[start..]
                    .windows(b"\x1b[?2026l".len())
                    .any(|frame| frame == b"\x1b[?2026l");
                if painted {
                    live_repair.set(true);
                    tx.send(RenderCommand::Stop).unwrap();
                } else {
                    assert!(started.elapsed() < Duration::from_secs(2));
                }
                return false;
            }
            repair_start.set(Some(bytes.lock().unwrap().len()));
            if insert_historical {
                state
                    .borrow_mut()
                    .insert_block(0, TranscriptBlock::Notice("INSERTED-HISTORY".into()));
            }
            // Accepted output may overtake the resize's settled paint. It
            // must enter saved history, even when it exceeds the live grid.
            for index in 0..appended {
                state
                    .borrow_mut()
                    .push_block(TranscriptBlock::Notice(format!("ACCEPTED-{index:03}")));
            }
            let mut changed = false;
            for dimensions in resizes {
                changed |= reconcile_terminal_size(state, size, *dimensions);
            }
            // Exercise both the settled live paint and a Stop overtaking it.
            // An unchanged shutdown flush must not replay a live repair again.
            tx.send(if wait_for_live_paint {
                RenderCommand::Render
            } else {
                RenderCommand::Stop
            })
            .unwrap();
            changed
        },
    );
    assert_eq!(live_repair.get(), wait_for_live_paint);
    let bytes = bytes.lock().unwrap();
    let repair_start = repair_start.get().expect("renderer polled the resize");
    let mut terminal = vt100::Parser::new(24, 80, 4096);
    process_vt100_with_saved_line_clear(&mut terminal, &bytes[..repair_start], 24, 80, 4096);
    for dimensions in resizes {
        terminal.set_size(dimensions.1, dimensions.0);
    }
    process_vt100_with_saved_line_clear(
        &mut terminal,
        &bytes[repair_start..],
        dimensions.1,
        dimensions.0,
        4096,
    );
    // Inspect saved lines and the live grid in one continuous VT history.
    // A fresh parser for only the repair would miss silently skipped rows.
    terminal.set_size(4096, dimensions.0);
    terminal.set_scrollback(usize::MAX);
    let saved = terminal.screen().scrollback();
    let mut rows = terminal
        .screen()
        .rows(0, dimensions.0)
        .take(saved)
        .collect::<Vec<_>>();
    terminal.set_scrollback(0);
    rows.extend(
        terminal
            .screen()
            .rows(0, dimensions.0)
            .take(usize::from(dimensions.1)),
    );
    let physical = rows.join("\n");
    for (prefix, count) in [("SAVED", 256), ("ACCEPTED", appended)] {
        for index in 0..count {
            let marker = format!("{prefix}-{index:03}");
            assert_eq!(
                physical.matches(&marker).count(),
                1,
                "{resizes:?}: missing or duplicated {marker}"
            );
        }
    }
    assert_eq!(
        physical.matches("INSERTED-HISTORY").count(),
        usize::from(insert_historical)
    );
    let repair = String::from_utf8_lossy(&bytes[repair_start..]);
    assert_eq!(repair.matches("\x1b[3J").count(), 1, "one canonical repair");
    assert_eq!(repair.matches("\x1b[?2026h").count(), 1);
    assert_eq!(repair.matches("\x1b[?2026l").count(), 1);
    assert!(
        repair.contains("\x1b[?25h\x1b[?2026l"),
        "cursor restoration must be atomic"
    );
}

#[test]
fn threaded_native_resize_keeps_every_row_accepted_before_the_paint() {
    for resizes in [
        &[(64, 24)][..],
        &[(80, 8)][..],
        &[(80, 40)][..],
        &[(64, 8)][..],
        &[(120, 40)][..],
        &[(64, 8), (80, 24)][..],
    ] {
        for appended in [0, 64] {
            check_native_repair(resizes, false, appended, false);
        }
    }
}

#[test]
fn threaded_native_historical_insertion_keeps_the_complete_new_tail() {
    check_native_repair(&[], true, 64, false);
}

#[test]
fn threaded_native_settled_resize_repairs_once_before_unchanged_stop() {
    check_native_repair(&[(64, 8)], false, 64, true);
    check_native_repair(&[(64, 8), (80, 24)], false, 64, true);
}

#[test]
fn threaded_native_tool_contraction_keeps_working_clocks_alive_without_resize() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 8);
    shell.tui.take().unwrap().stop();
    shell.state.borrow_mut().verbose_tools = true;
    for index in 0..30 {
        shell.notice(format!("CLOCK-HISTORY-{index:02}"));
    }
    let run = shell.begin_run("openai");
    let id = ToolCallId("clock-tool".into());
    shell.on_run_event(
        run,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "clock-fixture"}),
        },
    );
    shell.on_run_event(
        run,
        &AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Output {
                stream: octet_agent::OutputStream::Stdout,
                bytes: bytes::Bytes::from("long mutable progress\n".repeat(24)),
            },
        },
    );
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let settled = Cell::new(false);
    let painted_phase = Cell::new(None);
    let advanced = Cell::new(false);
    let state = shell.state.clone();
    let size = shell.size.clone();
    let shell = RefCell::new(shell);
    render_loop_with_terminal(
        EmulatedTerminal {
            size: size.clone(),
            bytes: bytes.clone(),
            synchronized_output: true,
            status_frames: None,
        },
        state,
        size,
        rx,
        RenderLoopOptions::default(),
        |state, _size| {
            if !settled.get() {
                assert!(state.borrow().has_active_status_shimmer());
                shell.borrow_mut().on_run_event(
                    run,
                    &AgentEvent::ToolFinished {
                        id: id.clone(),
                        result: Ok(octet_agent::ToolOutput::new("short result")),
                        duration: Duration::from_millis(10),
                    },
                );
                tx.send(RenderCommand::Render).unwrap();
                settled.set(true);
            } else {
                let state = state.borrow();
                assert!(
                    state.has_active_status_shimmer(),
                    "a visible Working row must not wait for resize to resume its clock"
                );
                assert!(state.has_active_status_timer());
                match painted_phase.get() {
                    None => painted_phase.set(Some(state.status_shimmer_frame)),
                    Some(phase) if state.status_shimmer_frame != phase => {
                        advanced.set(true);
                        tx.send(RenderCommand::Stop).unwrap();
                    }
                    _ => assert!(started.elapsed() < Duration::from_secs(2)),
                }
            }
            // This is an idle-poll hook, not a resize. The production clock
            // must advance without another model/tool render notification.
            false
        },
    );
    assert!(
        advanced.get(),
        "the production animation clock must wake itself"
    );
    let bytes = bytes.lock().unwrap();
    let mut terminal = vt100::Parser::new(8, 80, 2048);
    terminal.process(&bytes);
    assert!(terminal.screen().contents().contains("Working"));
    // The result may already be in native history in an eight-row pane.
    assert!(String::from_utf8_lossy(&bytes).contains("short result"));
}
