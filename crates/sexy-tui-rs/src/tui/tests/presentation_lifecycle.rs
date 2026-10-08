//! Presentation lifecycle: resize, generation changes, and explicit
//! presentation resets, each of which decides whether committed
//! native history survives or the whole frame is replayed.
//!
//! These assertions were extracted from `tui.rs`; each module is a
//! child of `crate::tui::tests`, so `use super::*` reaches exactly
//! the private items it reached while the tests were inline.

use super::*;

#[test]
fn generation_change_after_resize_does_not_inherit_the_replayed_row_seam() {
    let size = Rc::new(Cell::new((40, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = ["old 0", "old 1", "old 2"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    // A destructive replay knows its timeline even though it deliberately
    // clears the semantic commit cursor until the next handshake.
    tui.inline_generation = Some(7);
    tui.inline_history_rows = 3;
    tui.inline_commit_cursor = None;
    tui.inline_window_top = 0;
    tui.inline_bottom_row = 2;

    let replacement = ["new 0", "new 1", "new 2"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let previous_window = tui.previous_frame.clone();
    tui.write_inline_pinned(
        &replacement,
        3,
        PinnedFrame {
            generation: 8,
            acknowledged: None,
            target: None,
            stable_rows: 0,
            viewport_surface: false,
        },
        false,
        false,
        &previous_window,
    );

    let output = writes.borrow().join("");
    assert!(output.contains("new 0"), "{output:?}");
    assert!(output.contains("new 2"), "{output:?}");
    assert_eq!(tui.inline_history_rows, 0);
    assert_eq!(tui.inline_generation, Some(8));
}

#[test]
fn pinned_presentation_reset_preserves_history_without_replay() {
    let size = Rc::new(Cell::new((40, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = vec!["old presentation".to_owned()];
    tui.inline_commit_cursor = Some(test_commit_position(2).cursor);
    tui.inline_committed_rows = 2;
    tui.inline_window_top = 4;
    let replacement = [
        "settled 0",
        "settled 1",
        "mutable omitted 2",
        "mutable omitted 3",
        "mutable visible 4",
        "mutable visible 5",
        "composer",
        "footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let cursor = tui.inline_commit_cursor;

    tui.write_inline_pinned(
        &replacement,
        4,
        test_pinned_frame(cursor, None),
        true,
        false,
        &[],
    );

    let output = writes.borrow().join("");
    assert!(!output.contains("\x1b[3J"), "{output:?}");
    assert!(!output.contains("settled 0"), "{output:?}");
    assert!(!output.contains("settled 1"), "{output:?}");
    assert!(!output.contains("mutable omitted 2"), "{output:?}");
    assert!(!output.contains("mutable omitted 3"), "{output:?}");
    assert!(output.contains("mutable visible 4"), "{output:?}");
    assert!(output.contains("composer"), "{output:?}");
    assert_eq!(tui.inline_committed_rows, 2);
    assert_eq!(tui.inline_window_top, 4);
    assert_eq!(tui.inline_bottom_row, 3);
}

#[test]
fn logical_timeline_reset_reanchors_later_panel_updates_to_the_bottom() {
    // Regression: after replacing a long resumed conversation with `/new`,
    // the shorter frame was painted relative to the old scrollback origin.
    // The composer landed near the top and opening `/model` kept expanding
    // relative to that incorrect physical bottom.
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    let size = Rc::new(Cell::new((30, 6)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        String::new(),
        String::new(),
        String::new(),
        "composer top".to_owned(),
        format!("prompt {CURSOR_MARKER}"),
        "footer".to_owned(),
    ]));
    let reanchor = Rc::new(Cell::new(true));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyReanchoredLines {
        lines: lines.clone(),
        reanchor,
        rebuild_scrollback: Rc::new(Cell::new(false)),
    }));
    tui.previous_frame = (0..10)
        .map(|index| format!("historic row {index}{RESET}"))
        .collect();
    tui.previous_size = Some((30, 6));
    tui.first_render = false;
    tui.inline_bottom_row = 5;
    tui.running = true;

    tui.request_render();
    assert_eq!(
        clears.get(),
        0,
        "timeline replacement must repaint by row without mutating history"
    );
    assert!(writes.borrow().join("").contains("\x1b[H"));
    assert_eq!(tui.inline_bottom_row, 5);
    assert!(writes.borrow().join("").contains("\x1b[5;8H"));

    writes.borrow_mut().clear();
    *lines.borrow_mut() = vec![
        String::new(),
        "Models".to_owned(),
        "  model-a".to_owned(),
        "composer top".to_owned(),
        format!("prompt {CURSOR_MARKER}"),
        "footer".to_owned(),
    ];
    tui.request_render();

    let panel_frame = writes.borrow().join("");
    assert!(panel_frame.contains("Models"), "{panel_frame:?}");
    assert!(panel_frame.contains("\x1b[5;8H"), "{panel_frame:?}");
    assert_eq!(tui.inline_bottom_row, 5);
}

#[test]
fn presentation_reset_clears_and_rebuilds_native_scrollback_once() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    const KITTY_IMAGE: &str = "\x1b_Ga=T,f=100,i=4,s=1,v=1,c=1,r=1;AAAA\x1b\\";
    let size = Rc::new(Cell::new((30, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(
        (0..6)
            .map(|index| format!("new-theme row {index}"))
            .collect::<Vec<_>>(),
    ));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyReanchoredLines {
        lines,
        reanchor: Rc::new(Cell::new(true)),
        rebuild_scrollback: Rc::new(Cell::new(true)),
    }));
    tui.previous_frame = (0..6)
        .map(|index| {
            if index == 2 {
                format!("{KITTY_IMAGE}{RESET}")
            } else {
                format!("old-theme row {index}{RESET}")
            }
        })
        .collect();
    tui.previous_size = Some((30, 4));
    tui.first_render = false;
    tui.inline_bottom_row = 3;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    let delete_images = output
        .find(&delete_all_kitty_images())
        .expect("presentation reset did not delete Kitty placements");
    let clear_saved = output
        .find("\x1b[3J")
        .expect("presentation reset did not erase saved lines");
    assert!(delete_images < clear_saved, "{output:?}");
    let rebuilt = &output[clear_saved + "\x1b[3J".len()..];
    assert_eq!(clears.get(), 1);
    assert!(!rebuilt.contains("old-theme"), "{rebuilt:?}");
    for index in 0..6 {
        assert_eq!(
            rebuilt.matches(&format!("new-theme row {index}")).count(),
            1,
            "row {index} was not rebuilt exactly once: {rebuilt:?}"
        );
    }
    assert_eq!(tui.inline_bottom_row, 3);
}

#[test]
fn pinned_presentation_rebuild_preserves_committed_native_history() {
    let size = Rc::new(Cell::new((30, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "committed history 0".to_owned(),
        "committed history 1".to_owned(),
        "expanded summary 0".to_owned(),
        "expanded summary 1".to_owned(),
        "expanded summary 2".to_owned(),
        "later event 3".to_owned(),
        "later event 4".to_owned(),
        "later event 5".to_owned(),
        format!("composer {CURSOR_MARKER}"),
        "footer".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyPinnedLines {
        lines,
        commit_boundary: Rc::new(Cell::new(5)),
        rebuild_scrollback: Rc::new(Cell::new(true)),
        generation: Rc::new(Cell::new(0)),
    }));
    tui.previous_frame = [
        "committed history 0",
        "committed history 1",
        "collapsed summary",
        "later event 3",
        "later event 4",
        "later event 5",
        "composer",
        "footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    tui.previous_size = Some((30, 4));
    tui.first_render = false;
    tui.inline_history_rows = 2;
    tui.inline_committed_rows = 2;
    tui.inline_commit_cursor = Some(test_commit_position(2).cursor);
    tui.inline_generation = Some(0);
    tui.inline_window_top = 4;
    tui.inline_bottom_row = 3;
    tui.running = true;

    tui.request_render();

    let expanded = writes.borrow().join("");
    assert!(!expanded.contains("\x1b[2J"), "{expanded:?}");
    assert!(!expanded.contains("\x1b[3J"), "{expanded:?}");
    assert!(!expanded.contains("committed history"), "{expanded:?}");
    for row in 0..=2 {
        assert!(
            expanded.contains(&format!("expanded summary {row}")),
            "{expanded:?}"
        );
    }
    assert_eq!(clears.get(), 0);
    assert_eq!(tui.inline_history_rows, 5);
    assert_eq!(tui.inline_window_top, 6);
}

#[test]
fn resize_and_generation_change_start_a_new_tape_without_clearing_old_history() {
    let size = Rc::new(Cell::new((30, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let lines = Rc::new(RefCell::new(
        [
            "old history 0",
            "old history 1",
            "old visible 2",
            "old visible 3",
            "old composer",
            "old footer",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    ));
    let boundary = Rc::new(Cell::new(2));
    let generation = Rc::new(Cell::new(0));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyPinnedLines {
        lines: Rc::clone(&lines),
        commit_boundary: Rc::clone(&boundary),
        rebuild_scrollback: Rc::new(Cell::new(false)),
        generation: Rc::clone(&generation),
    }));
    tui.start();
    let clears_after_start = clears.get();

    *lines.borrow_mut() = [
        "new history 0",
        "new history 1",
        "new visible 2",
        "new visible 3",
        "new composer",
        "new footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    boundary.set(0);
    generation.set(1);
    writes.borrow_mut().clear();
    size.set((50, 6));
    tui.request_render();

    let resized = writes.borrow().join("");
    assert_eq!(clears.get(), clears_after_start);
    assert!(!resized.contains("\x1b[3J"), "{resized:?}");
    assert!(!resized.contains("old history"), "{resized:?}");
    assert!(resized.contains("new history 0"), "{resized:?}");
    assert!(resized.contains("new footer"), "{resized:?}");
    assert_eq!(tui.inline_history_rows, 0);
    assert_eq!(tui.inline_generation, Some(1));
}
