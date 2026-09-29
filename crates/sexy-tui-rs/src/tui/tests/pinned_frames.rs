//! Pinned history rows: how a frame diff addresses physical rows
//! once the viewport window shifts, and when a reanchor stages a new
//! target without leaving a partially painted grid behind.
//!
//! These assertions were extracted from `tui.rs`; each module is a
//! child of `crate::tui::tests`, so `use super::*` reaches exactly
//! the private items it reached while the tests were inline.

use super::*;

#[test]
fn pinned_window_diffs_by_physical_row_after_window_shift() {
    let size = Rc::new(Cell::new((40, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = ["history 0", "history 1", "screen A", "screen B", "screen C"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    tui.inline_window_top = 2;
    tui.inline_bottom_row = 2;

    let shifted = [
        "new history 0",
        "new history 1",
        "new history 2",
        "screen A",
        "screen B",
        "screen C",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let previous_window = tui.previous_frame[2..].to_vec();
    tui.write_inline_pinned(
        &shifted,
        3,
        test_pinned_frame(None, None),
        false,
        false,
        &previous_window,
    );

    assert!(
        writes.borrow().is_empty(),
        "unchanged physical cells were repainted: {:?}",
        writes.borrow()
    );
}

#[test]
fn pinned_stable_rows_scroll_naturally_in_one_multi_row_append() {
    let size = Rc::new(Cell::new((40, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = [
        "history 0",
        "history 1",
        "stable 2",
        "stable 3",
        "tail 4",
        "tail 5",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    tui.inline_history_rows = 2;
    tui.inline_window_top = 2;
    tui.inline_bottom_row = 3;

    let grown = tui
        .previous_frame
        .iter()
        .cloned()
        .chain(["tail 6".to_owned(), "tail 7".to_owned()])
        .collect::<Vec<_>>();
    let previous_window = tui.previous_frame[2..].to_vec();
    tui.write_inline_pinned(
        &grown,
        4,
        test_pinned_frame_with_stable(None, None, 4),
        false,
        false,
        &previous_window,
    );

    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[4;1H\r\n\r\n"), "{output:?}");
    assert!(!output.contains("\x1b[H"), "{output:?}");
    assert!(!output.contains("stable 2"), "{output:?}");
    assert!(!output.contains("stable 3"), "{output:?}");
    assert_eq!(output.matches("tail 6").count(), 1, "{output:?}");
    assert_eq!(output.matches("tail 7").count(), 1, "{output:?}");
    assert_eq!(tui.inline_history_rows, 4);
    assert_eq!(tui.inline_window_top, 4);
}

#[test]
fn pinned_physical_stability_can_run_ahead_of_semantic_acknowledgement() {
    let size = Rc::new(Cell::new((40, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = ["row 0", "row 1", "row 2", "row 3"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    tui.inline_window_top = 0;
    tui.inline_bottom_row = 3;

    let grown = tui
        .previous_frame
        .iter()
        .cloned()
        .chain(
            ["tail 4", "tail 5", "tail 6"]
                .into_iter()
                .map(str::to_owned),
        )
        .collect::<Vec<_>>();
    let previous_window = tui.previous_frame.clone();
    tui.write_inline_pinned(
        &grown,
        4,
        test_pinned_frame_with_stable(None, Some(2), 3),
        false,
        false,
        &previous_window,
    );

    let cursor = test_commit_position(2).cursor;
    assert_eq!(tui.inline_history_rows, 3);
    assert_eq!(tui.inline_committed_rows, 2);
    assert_eq!(tui.inline_commit_cursor, Some(cursor));

    // The next acknowledgement maps only row two. The separately retained
    // physical seam must prevent row two from being appended a second time.
    writes.borrow_mut().clear();
    tui.previous_frame = grown.clone();
    let previous_window = grown[3..].to_vec();
    let mut next = grown;
    next.push("tail 7".to_owned());
    tui.write_inline_pinned(
        &next,
        4,
        test_pinned_frame_with_stable(Some(cursor), None, 4),
        false,
        false,
        &previous_window,
    );

    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[4;1H\r\n"), "{output:?}");
    assert!(!output.contains("row 2"), "{output:?}");
    assert!(!output.contains("row 3"), "{output:?}");
    assert_eq!(tui.inline_history_rows, 4);
    assert_eq!(tui.inline_committed_rows, 2);
    assert_eq!(tui.inline_commit_cursor, Some(cursor));
}

#[test]
fn pinned_reanchor_stages_an_atomic_target_and_one_complete_grid() {
    let size = Rc::new(Cell::new((40, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.inline_history_rows = 1;
    tui.inline_committed_rows = 1;
    tui.inline_commit_cursor = Some(test_commit_position(1).cursor);
    tui.inline_generation = Some(0);
    tui.inline_window_top = 4;
    tui.inline_bottom_row = 2;

    let replacement = (0..8).map(|row| format!("row {row}")).collect::<Vec<_>>();
    tui.write_inline_pinned(
        &replacement,
        3,
        test_pinned_frame_with_stable(tui.inline_commit_cursor, Some(3), 1),
        true,
        false,
        &[],
    );

    let output = writes.borrow().join("");
    assert_eq!(clears.get(), 0, "reanchor must preserve saved lines");
    assert!(output.starts_with("\x1b[H"), "{output:?}");
    for row in [1, 2, 5, 6, 7] {
        assert_eq!(
            output.matches(&format!("row {row}")).count(),
            1,
            "row {row} was not staged exactly once: {output:?}"
        );
    }
    for row in [0, 3, 4] {
        assert!(!output.contains(&format!("row {row}")), "{output:?}");
    }
    assert_eq!(output.matches("\r\n").count(), 4, "{output:?}");
    assert_eq!(tui.inline_history_rows, 3);
    assert_eq!(tui.inline_committed_rows, 3);
    assert_eq!(
        tui.inline_commit_cursor,
        Some(test_commit_position(3).cursor)
    );
    assert_eq!(tui.inline_window_top, 5);
}

#[test]
fn pinned_window_shrink_repaints_a_temporary_surface_before_the_history_seam() {
    let size = Rc::new(Cell::new((40, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = [
        "settled 0",
        "settled 1",
        "settled 2",
        "settled 3",
        "old visible 4",
        "old visible 5",
        "empty composer",
        "footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    tui.inline_history_rows = 4;
    tui.inline_committed_rows = 4;
    tui.inline_commit_cursor = Some(test_commit_position(4).cursor);
    tui.inline_window_top = 4;
    tui.inline_bottom_row = 3;

    // Finalizing streamed Markdown can reduce the logical row count after
    // rows above the old viewport have already entered native scrollback.
    // The new bottom-aligned top retreats to row three, which is already
    // terminal-owned. Paint the complete semantic tail as a temporary
    // cursor-addressed surface instead of committing it or punching a blank
    // row into the live grid.
    let shrunk = [
        "settled 0",
        "settled 1",
        "settled 2",
        "new visible 3",
        "new visible 4",
        "typed composer",
        "footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let previous_window = tui.previous_frame[4..].to_vec();
    let cursor = tui.inline_commit_cursor;
    tui.write_inline_pinned(
        &shrunk,
        4,
        test_pinned_frame(cursor, Some(7)),
        false,
        false,
        &previous_window,
    );

    let output = writes.borrow().join("");
    assert!(output.contains("new visible 3"), "{output:?}");
    assert!(output.contains("new visible 4"), "{output:?}");
    assert!(output.contains("typed composer"), "{output:?}");
    assert!(output.contains("footer"), "{output:?}");
    assert!(
        !output.contains('\n'),
        "surface repaint scrolled: {output:?}"
    );
    assert_eq!(tui.inline_history_rows, 4);
    assert_eq!(tui.inline_committed_rows, 4);
    assert_eq!(tui.inline_window_top, 4);
    assert!(tui.inline_surface_active);
    assert_eq!(tui.inline_bottom_row, 3);
}

#[test]
fn pinned_window_clears_rows_left_by_a_shorter_frame() {
    let size = Rc::new(Cell::new((40, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = ["screen A", "screen B", "stale C"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    tui.inline_window_top = 0;
    tui.inline_bottom_row = 2;
    let shorter = ["screen A", "screen B"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let previous_window = tui.previous_frame.clone();

    tui.write_inline_pinned(
        &shorter,
        3,
        test_pinned_frame(None, None),
        false,
        false,
        &previous_window,
    );

    let output = writes.borrow().join("");
    assert!(
        output.contains("\x1b[3;1H"),
        "stale trailing row was not addressed for clearing: {output:?}"
    );
    assert!(!output.contains("stale C"), "{output:?}");
}

#[test]
fn pinned_reanchor_erases_rows_without_clear_screen() {
    let size = Rc::new(Cell::new((40, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.first_render = false;
    tui.previous_frame = ["old A", "old composer", "old footer"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    tui.inline_window_top = 0;
    tui.inline_bottom_row = 2;
    let replacement = ["new A", "new composer", "new footer"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let previous_window = tui.previous_frame.clone();

    tui.write_inline_pinned(
        &replacement,
        3,
        test_pinned_frame(None, None),
        true,
        false,
        &previous_window,
    );

    assert_eq!(
        clears.get(),
        0,
        "tmux records clear-screen contents in native scrollback"
    );
    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[H"), "{output:?}");
    assert!(output.contains("new composer"), "{output:?}");
    assert!(!output.contains("old composer"), "{output:?}");
}

#[test]
fn fixed_height_middle_update_does_not_repaint_pinned_tail() {
    let size = Rc::new(Cell::new((40, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, tail_clears, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "history".to_owned(),
        String::new(),
        String::new(),
        String::new(),
        "composer top".to_owned(),
        "composer input".to_owned(),
        "composer bottom".to_owned(),
        "footer telemetry".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();

    writes.borrow_mut().clear();
    lines.borrow_mut()[1] = "new event".to_owned();
    tui.request_render();

    let output = writes.borrow().join("");
    assert!(output.contains("new event"), "{output:?}");
    assert!(!output.contains("composer"), "{output:?}");
    assert!(!output.contains("footer telemetry"), "{output:?}");
    assert_eq!(tail_clears.get(), 0, "the pinned tail must not be erased");
}

#[test]
fn text_only_pinned_resize_preserves_native_history_and_repaints_one_grid() {
    let size = Rc::new(Cell::new((30, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    )
    .with_overrides(&crate::capabilities::CapabilityOverrides {
        synchronized_output: Some(true),
        ..crate::capabilities::CapabilityOverrides::default()
    });
    let (terminal, clears, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let mut initial_lines = [
        "history 0",
        "history 1",
        "history 2",
        "history 3",
        "visible 4",
        "visible 5",
        "empty composer",
        "footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    initial_lines[6].push_str(CURSOR_MARKER);
    let lines = Rc::new(RefCell::new(initial_lines));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyPinnedLines {
        lines,
        commit_boundary: Rc::new(Cell::new(2)),
        rebuild_scrollback: Rc::new(Cell::new(false)),
        generation: Rc::new(Cell::new(0)),
    }));
    tui.start();
    let clears_after_start = clears.get();

    writes.borrow_mut().clear();
    size.set((50, 6));
    tui.request_render();
    let resized = writes.borrow().join("");

    assert_eq!(clears.get(), clears_after_start);
    assert!(!resized.contains("\x1b[3J"), "{resized:?}");
    assert!(!resized.contains("history 0"), "{resized:?}");
    assert!(!resized.contains("history 1"), "{resized:?}");
    assert!(resized.contains("history 2"), "{resized:?}");
    assert!(resized.contains("footer"), "{resized:?}");
    assert_eq!(tui.inline_history_rows, 2);
    assert_eq!(tui.inline_window_top, 2);
    assert_eq!(tui.inline_bottom_row, 5);
}

#[test]
fn pinned_resize_resets_scrollback_and_updates_the_next_diff_origin() {
    const KITTY_IMAGE: &str = "\x1b_Ga=T,f=100,i=9,s=1,v=1,c=1,r=1;AAAA\x1b\\";
    let size = Rc::new(Cell::new((30, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    )
    .with_overrides(&crate::capabilities::CapabilityOverrides {
        synchronized_output: Some(true),
        ..crate::capabilities::CapabilityOverrides::default()
    });
    let (terminal, clears, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let mut initial_lines = [
        "history 0",
        "history 1",
        "history 2",
        "history 3",
        "visible 4",
        "visible 5",
        "empty composer",
        "footer",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    initial_lines[1].push_str(KITTY_IMAGE);
    initial_lines[6].push_str(CURSOR_MARKER);
    let lines = Rc::new(RefCell::new(initial_lines));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyPinnedLines {
        lines: lines.clone(),
        commit_boundary: Rc::new(Cell::new(2)),
        rebuild_scrollback: Rc::new(Cell::new(false)),
        generation: Rc::new(Cell::new(0)),
    }));
    tui.start();
    assert_eq!(tui.inline_window_top, 4);
    let clears_after_start = clears.get();

    // Growing and widening the terminal invalidates both grid and saved
    // row coordinates. The reset must replay every owned row, then retain
    // the new window origin for the next composer-only update.
    writes.borrow_mut().clear();
    size.set((50, 6));
    tui.request_render();
    let resized = writes.borrow().join("");
    assert_eq!(tui.inline_window_top, 2);
    assert_eq!(tui.inline_bottom_row, 5);
    assert_eq!(clears.get(), clears_after_start + 1);
    assert!(
        resized.contains("\x1b[H\x1b[3J"),
        "pinned resize did not erase saved lines: {resized:?}"
    );
    for index in 0..=3 {
        assert!(resized.contains(&format!("history {index}")), "{resized:?}");
    }
    assert!(resized.contains("visible 4"), "{resized:?}");
    assert!(resized.contains("visible 5"), "{resized:?}");
    let delete_at = resized
        .find(&delete_all_kitty_images())
        .expect("pinned resize did not delete Kitty placements");
    let image_at = resized
        .find(KITTY_IMAGE)
        .expect("pinned resize did not retransmit the image");
    let begin_at = resized
        .find("\x1b[?2026h")
        .expect("resize replay did not begin synchronized output");
    let clear_saved_at = resized.find("\x1b[3J").expect("saved-line clear");
    let end_at = resized
        .rfind("\x1b[?2026l")
        .expect("resize replay did not end synchronized output");
    assert!(
        begin_at < delete_at
            && delete_at < clear_saved_at
            && clear_saved_at < image_at
            && image_at < end_at,
        "resize replay was not one ordered synchronized transaction: {resized:?}"
    );
    assert!(
        resized.contains("\x1b[5;15H"),
        "composer cursor must follow the resized pinned window: {resized:?}"
    );

    writes.borrow_mut().clear();
    lines.borrow_mut()[6] = format!("typed composer{CURSOR_MARKER}");
    tui.request_render();
    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[5;1H"), "{output:?}");
    assert!(output.contains("typed composer"), "{output:?}");
}
