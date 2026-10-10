//! Native scrollback: first paint, growth and shrink past the saved
//! lines, destructive replay on resize, and the kitty graphic
//! placements that must be deleted before any of it.
//!
//! These assertions were extracted from `tui.rs`; each module is a
//! child of `crate::tui::tests`, so `use super::*` reaches exactly
//! the private items it reached while the tests were inline.

use super::*;

#[test]
fn inline_resize_deletes_kitty_placements_before_destructive_replay() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    const KITTY_IMAGE: &str = "\x1b_Ga=T,f=100,i=2,s=1,v=1,c=1,r=1;AAAA\x1b\\";
    let size = Rc::new(Cell::new((41, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::TrueColor,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "unchanged top".to_owned(),
        "resized plain row".to_owned(),
        "unchanged bottom".to_owned(),
        "footer".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines)));
    tui.previous_frame = ["unchanged top", KITTY_IMAGE, "unchanged bottom", "footer"]
        .into_iter()
        .map(|line| format!("{line}{RESET}"))
        .collect();
    tui.previous_size = Some((40, 4));
    tui.first_render = false;
    tui.inline_bottom_row = 3;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    let delete_at = output
        .find(&delete_all_kitty_images())
        .expect("Kitty placements were deleted during resize");
    let replacement_at = output
        .find("resized plain row")
        .expect("resized replacement row was painted");
    assert!(delete_at < replacement_at, "{output:?}");
    assert_eq!(clears.get(), 1, "resize must clear the reflowed grid");
    assert!(
        output.contains("\x1b[H\x1b[3J"),
        "resize must discard terminal-owned saved lines: {output:?}"
    );
}

#[test]
fn inline_length_change_deletes_kitty_placements_and_restores_the_viewport() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    const KITTY_IMAGE: &str = "\x1b_Ga=T,f=100,i=3,s=1,v=1,c=1,r=1;AAAA\x1b\\";
    let size = Rc::new(Cell::new((40, 5)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::TrueColor,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "unchanged top".to_owned(),
        "length-changed plain row".to_owned(),
        "inserted row".to_owned(),
        "unchanged bottom".to_owned(),
        "footer".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyFixedLines { lines }));
    tui.previous_frame = ["unchanged top", KITTY_IMAGE, "unchanged bottom", "footer"]
        .into_iter()
        .map(|line| format!("{line}{RESET}"))
        .collect();
    tui.previous_size = Some((40, 5));
    tui.first_render = false;
    tui.inline_bottom_row = 3;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    let delete_at = output
        .find(&delete_all_kitty_images())
        .expect("Kitty placements were deleted during the length change");
    let replacement_at = output
        .find("length-changed plain row")
        .expect("length-change replacement row was painted");
    assert!(delete_at < replacement_at, "{output:?}");
    assert!(
        output.contains("unchanged top") && output.contains("unchanged bottom"),
        "the viewport must be restored after deleting all placements: {output:?}"
    );
}

#[test]
fn inline_scrollback_first_render_preserves_screen_and_appends_scroll() {
    let size = Rc::new(Cell::new((20, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, tail_clears, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "one".to_owned(),
        "two".to_owned(),
        "three".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();

    // First render scrolls the shell's screen into scrollback (one
    // newline per row) before clearing and painting from home.
    let strip = |text: String| text.replace("\u{1b}[0m\u{1b}]8;;\u{1b}\\", "");
    let first = strip(writes.borrow().join(""));
    assert!(first.starts_with("\n\n\n\n"), "{first:?}");
    assert_eq!(clears.get(), 1);
    assert!(first.contains("one\ntwo\nthree"), "{first:?}");
    assert!(!first.ends_with('\n'));

    // A pure append repaints from the last on-screen line — never a
    // full-screen clear, so scrollback history is never rewritten.
    writes.borrow_mut().clear();
    lines.borrow_mut().push("four".to_owned());
    tui.request_render();
    let appended = strip(writes.borrow().join(""));
    assert_eq!(clears.get(), 1, "append must not clear the screen");
    assert_eq!(tail_clears.get(), 1);
    assert!(appended.contains("three\nfour"), "{appended:?}");
    assert!(!appended.contains("one"), "history must not be rewritten");

    // Growing past the screen height keeps repainting only the tail.
    writes.borrow_mut().clear();
    lines
        .borrow_mut()
        .extend(["five".to_owned(), "six".to_owned()]);
    tui.request_render();
    let grown = strip(writes.borrow().join(""));
    assert!(grown.contains("four\nfive\nsix"), "{grown:?}");
    assert!(!grown.contains("one"));
    assert_eq!(clears.get(), 1);
}

#[test]
fn inline_first_paint_is_bounded_to_the_terminal_viewport() {
    const HISTORY: usize = 10_000;
    let size = Rc::new(Cell::new((120, 24)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::TrueColor,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(
        (0..HISTORY)
            .map(|index| format!("historic row {index}"))
            .collect::<Vec<_>>(),
    ));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines)));

    tui.start();

    let output = writes.borrow().join("");
    let painted = output
        .rsplit_once("\x1b[H")
        .map_or(output.as_str(), |(_, painted)| painted);
    assert_eq!(clears.get(), 1);
    assert_eq!(painted.matches('\n').count(), 23, "{painted:?}");
    assert!(painted.contains("historic row 9999"), "{painted:?}");
    assert!(!painted.contains("historic row 0\x1b"), "{painted:?}");
    assert!(
        output.len() < 4_096,
        "first paint unexpectedly emitted {} bytes",
        output.len()
    );
    assert_eq!(tui.previous_frame.len(), HISTORY);
}

#[test]
fn large_growth_above_native_viewport_never_replays_displaced_history() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    for synchronized_output in [false, true] {
        let size = Rc::new(Cell::new((40, 4)));
        let capabilities = crate::capabilities::TerminalCapabilities::interactive(
            crate::capabilities::ColorDepth::Ansi16,
            true,
        )
        .with_overrides(&crate::capabilities::CapabilityOverrides {
            synchronized_output: Some(synchronized_output),
            ..crate::capabilities::CapabilityOverrides::default()
        });
        let (terminal, clears, tail_clears, _, _, writes) = recording_terminal(size, capabilities);
        let mut next = (0..372)
            .map(|index| format!("inserted row {index}"))
            .collect::<Vec<_>>();
        next.extend(
            [
                "history 0",
                "history 1",
                "history 2",
                "history 3",
                "visible a",
                "visible b",
                "visible C updated",
                "footer",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let lines = Rc::new(RefCell::new(next));
        let mut tui = TUI::new(Box::new(terminal));
        tui.set_inline_scrollback(true);
        tui.add_child(Box::new(MutableLines(lines)));
        tui.previous_frame = [
            "history 0",
            "history 1",
            "history 2",
            "history 3",
            "visible a",
            "visible b",
            "visible c",
            "footer",
        ]
        .into_iter()
        .map(|line| format!("{line}{RESET}"))
        .collect();
        tui.previous_size = Some((40, 4));
        tui.first_render = false;
        tui.inline_bottom_row = 3;
        tui.running = true;

        tui.request_render();

        let output = writes.borrow().join("");
        assert_eq!(
            output.contains("\x1b[?2026h"),
            synchronized_output,
            "{output:?}"
        );
        assert!(!output.contains("\x1b[3J"), "{output:?}");
        assert!(!output.contains('\n'), "{output:?}");
        assert_eq!(clears.get(), 0);
        assert_eq!(tail_clears.get(), 0);
        assert!(output.contains("\x1b[3;1H"), "{output:?}");
        assert!(output.contains("visible C updated"), "{output:?}");
        assert!(output.len() < 256, "unbounded repaint: {} B", output.len());
        for replayed in [
            "inserted row",
            "history 0",
            "history 3",
            "visible a",
            "visible b",
            "footer",
        ] {
            assert!(!output.contains(replayed), "{replayed:?}: {output:?}");
        }
    }
}

#[test]
fn offscreen_mutation_preserves_short_frame_screen_row_anchor() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    let size = Rc::new(Cell::new((40, 5)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut next = (0..64)
        .map(|index| format!("new offscreen row {index}"))
        .collect::<Vec<_>>();
    next.extend(
        [
            "history 0",
            "history 1",
            "history 2",
            "history 3",
            "history 4",
            "visible a",
            "visible B updated",
            "footer",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(Rc::new(RefCell::new(next)))));
    tui.previous_frame = [
        "history 0",
        "history 1",
        "history 2",
        "history 3",
        "history 4",
        "visible a",
        "visible b",
        "footer",
    ]
    .into_iter()
    .map(|line| format!("{line}{RESET}"))
    .collect();
    tui.previous_size = Some((40, 5));
    tui.first_render = false;
    // A prior shrink left the three-row frame tail at rows 0..=2 rather
    // than bottom-aligning it to the five-row terminal.
    tui.inline_bottom_row = 2;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[2;1H"), "{output:?}");
    assert!(!output.contains("\x1b[4;1H"), "{output:?}");
    assert!(output.contains("visible B updated"), "{output:?}");
    assert!(!output.contains('\n'), "{output:?}");
    assert!(!output.contains("new offscreen row"), "{output:?}");
}

#[test]
fn inline_scrollback_shrink_past_native_history_resets_and_replays_frame() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    let size = Rc::new(Cell::new((30, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(
        (0..5)
            .map(|index| format!("new row {index}"))
            .collect::<Vec<_>>(),
    ));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines)));
    tui.previous_frame = (0..8)
        .map(|index| format!("old row {index}{RESET}"))
        .collect();
    tui.previous_size = Some((30, 4));
    tui.first_render = false;
    // Only the old frame's final three rows are still on screen. The new
    // frame ends at the native-history seam and cannot be reconciled by a
    // cursor-addressed tail repaint.
    tui.inline_bottom_row = 2;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    assert_eq!(clears.get(), 1, "history reset was required");
    assert!(output.contains("\x1b[H\x1b[3J"), "{output:?}");
    assert!(
        !output.contains("old row"),
        "old frame was replayed: {output:?}"
    );
    for index in 0..5 {
        assert_eq!(
            output.matches(&format!("new row {index}")).count(),
            1,
            "new row {index} was replayed exactly once: {output:?}",
        );
    }
    assert_eq!(tui.inline_window_top, 1);
    assert_eq!(tui.inline_bottom_row, 3);
}

#[test]
fn inline_scrollback_shrink_keeps_row_anchoring_for_later_repaints() {
    // Regression: growing suggestion lists then shrinking them (e.g.
    // slash-command completion after a resumed session) must not leave
    // stale rows behind — after a shrink the frame's tail sits above the
    // bottom row and later repaints must anchor to it, not to a
    // bottom-aligned viewport.
    let size = Rc::new(Cell::new((20, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "a".to_owned(),
        "b".to_owned(),
        "c".to_owned(),
        "d".to_owned(),
        "e".to_owned(),
        "f".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();
    // Screen shows c d e f on rows 0..4.

    // Shrink: drop e and f. Repaint starts at the row that held e.
    writes.borrow_mut().clear();
    lines.borrow_mut().truncate(4);
    tui.request_render();
    assert!(writes.borrow().join("").contains("\x1b[3;1H"));

    // Append after the shrink: the new line must paint directly below
    // "d" (row 2), not at the bottom of the screen.
    writes.borrow_mut().clear();
    lines.borrow_mut().push("g".to_owned());
    tui.request_render();
    let strip = |text: String| text.replace("\u{1b}[0m\u{1b}]8;;\u{1b}\\", "");
    let appended = strip(writes.borrow().join(""));
    assert!(appended.contains("\x1b[2;1H"), "{appended:?}");
    assert!(appended.contains("d\ng"), "{appended:?}");
}

#[test]
fn inline_scrollback_resize_destructively_replays_the_complete_frame() {
    let size = Rc::new(Cell::new((20, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, clears, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let lines = Rc::new(RefCell::new(
        (0..6)
            .map(|index| format!("row {index}"))
            .collect::<Vec<_>>(),
    ));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();

    writes.borrow_mut().clear();
    let clears_before = clears.get();
    size.set((30, 4));
    tui.request_render();
    let repaint = writes
        .borrow()
        .join("")
        .replace("\u{1b}[0m\u{1b}]8;;\u{1b}\\", "");
    assert_eq!(clears.get(), clears_before + 1);
    assert!(
        repaint.contains("\x1b[H\x1b[3J"),
        "resize did not clear saved lines: {repaint:?}"
    );
    for index in 0..6 {
        assert_eq!(
            repaint.matches(&format!("row {index}")).count(),
            1,
            "row {index} was not replayed exactly once: {repaint:?}"
        );
    }
    assert_eq!(tui.inline_window_top, 2);
}

#[test]
fn resize_replays_an_unobscured_frame_before_repainting_its_screen_surface() {
    let size = Rc::new(Cell::new((20, 3)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let displayed = Rc::new(RefCell::new(
        ["owned 0", "owned 1", "overlay 0", "overlay 1", "overlay 2"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ));
    let replay = Rc::new(RefCell::new(
        ["owned 0", "owned 1", "owned 2", "owned 3", "owned 4"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyObscuredLines { displayed, replay }));
    tui.start();

    writes.borrow_mut().clear();
    size.set((30, 3));
    tui.request_render();
    let output = writes.borrow().join("");
    let clear_saved = output.find("\x1b[3J").expect("saved-line clear");
    let owned_tail = output.find("owned 4").expect("unobscured replay tail");
    let overlay = output.rfind("overlay 0").expect("screen-surface repaint");
    assert!(
        clear_saved < owned_tail && owned_tail < overlay,
        "{output:?}"
    );
    for index in 0..5 {
        assert_eq!(
            output.matches(&format!("owned {index}")).count(),
            1,
            "owned row {index} was not replayed exactly once: {output:?}"
        );
    }
    for index in 0..3 {
        assert_eq!(
            output.matches(&format!("overlay {index}")).count(),
            1,
            "overlay row {index} was not repainted exactly once: {output:?}"
        );
    }
    assert_eq!(tui.inline_window_top, 2);
}

#[test]
fn pi_resize_clears_saved_lines_and_replays_the_complete_frame() {
    let size = Rc::new(Cell::new((20, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(OneLine));
    tui.start();
    let redraws = tui.full_redraws();
    writes.borrow_mut().clear();
    size.set((80, 24));
    tui.request_render();

    assert!(tui.full_redraws() > redraws);
    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[2J\x1b[H\x1b[3J"), "{output:?}");
    assert!(output.contains("line"), "{output:?}");
}

#[test]
fn inline_scrollback_stop_anchors_after_the_final_frame_row() {
    let size = Rc::new(Cell::new((20, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, stops, shows, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableLines(Rc::new(RefCell::new(vec![
        "header".into(),
        format!("composer{CURSOR_MARKER}"),
        "footer".into(),
    ])))));
    tui.start();
    assert_eq!(tui.inline_bottom_row, 2);
    writes.borrow_mut().clear();

    tui.stop();

    assert_eq!(stops.get(), 1);
    assert_eq!(shows.get(), 2);
    assert_eq!(
        writes.borrow().join(""),
        "\x1b[3;1H\x1b[0m\x1b]8;;\x1b\\",
        "shutdown must leave the caller below the complete inline frame"
    );
}
