//! Lazy and fixed-height updates: the paths that must not re-render
//! or re-compare unchanged rows, and the kitty placement teardown
//! that has to happen before such a redraw.
//!
//! These assertions were extracted from `tui.rs`; each module is a
//! child of `crate::tui::tests`, so `use super::*` reaches exactly
//! the private items it reached while the tests were inline.

use super::*;

#[test]
fn pi_lazy_update_does_not_render_or_compare_stable_history() {
    const HISTORY: usize = 100_000;
    const RESET: &str = "\x1b[0m\x1b]8;;\x07";
    let size = Rc::new(Cell::new((80, 24)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let tail = Rc::new(RefCell::new("new mutable tail".to_owned()));
    let full_renders = Rc::new(Cell::new(0));
    let replacement_rows = Rc::new(Cell::new(0));
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(LazyTail {
        stable_prefix: HISTORY,
        tail,
        full_renders: full_renders.clone(),
        replacement_rows,
    }));
    tui.previous_frame = (0..HISTORY)
        .map(|index| format!("historic row {index}{RESET}"))
        .chain(std::iter::once(format!("old mutable tail{RESET}")))
        .collect();
    tui.previous_size = Some((80, 24));
    tui.previous_viewport_top = HISTORY + 1 - 24;
    tui.hardware_cursor_row = HISTORY;
    tui.first_render = false;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    assert_eq!(full_renders.get(), 0, "the full component renderer ran");
    assert_eq!(
        tui.last_pi_lazy_inspected_rows, 2,
        "stable history was inspected"
    );
    assert!(output.contains("new mutable tail"), "{output:?}");
    assert!(
        !output.contains("historic row"),
        "stable history was emitted"
    );
}

#[test]
fn pi_lazy_updates_with_kitty_use_the_full_component_path() {
    const KITTY_IMAGE: &str = "\x1b_Ga=T,f=100,i=9,s=1,v=1,c=1,r=2;AAAA\x1b\\";
    let size = Rc::new(Cell::new((40, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::TrueColor,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec!["before".to_owned()]));
    let full_renders = Rc::new(Cell::new(0));
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(LazyFallbackLines {
        lines: lines.clone(),
        full_renders: full_renders.clone(),
    }));

    tui.start();
    assert_eq!(full_renders.get(), 1);
    writes.borrow_mut().clear();

    *lines.borrow_mut() = vec![KITTY_IMAGE.to_owned(), String::new()];
    tui.request_render();
    assert_eq!(
        full_renders.get(),
        2,
        "new images must reject the lazy seam"
    );
    assert!(writes.borrow().join("").contains(KITTY_IMAGE));
    assert!(tui.previous_frame_has_kitty);
    writes.borrow_mut().clear();

    *lines.borrow_mut() = vec!["after".to_owned()];
    tui.request_render();
    assert_eq!(
        full_renders.get(),
        3,
        "old images must reject the lazy seam"
    );
    let output = writes.borrow().join("");
    assert!(output.contains(&delete_kitty_image(9)), "{output:?}");
    assert!(output.contains("after"), "{output:?}");
    assert!(!tui.previous_frame_has_kitty);
}

#[test]
fn lazy_update_does_not_render_or_emit_a_hundred_thousand_stable_rows() {
    const HISTORY: usize = 100_000;
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    let size = Rc::new(Cell::new((80, 24)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let tail = Rc::new(RefCell::new("new mutable tail".to_owned()));
    let full_renders = Rc::new(Cell::new(0));
    let replacement_rows = Rc::new(Cell::new(0));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyTail {
        stable_prefix: HISTORY,
        tail,
        full_renders: full_renders.clone(),
        replacement_rows: replacement_rows.clone(),
    }));
    tui.previous_frame = (0..HISTORY)
        .map(|index| format!("historic row {index}{RESET}"))
        .chain(std::iter::once(format!("old mutable tail{RESET}")))
        .collect();
    tui.previous_size = Some((80, 24));
    tui.first_render = false;
    tui.inline_bottom_row = 23;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    assert_eq!(full_renders.get(), 0, "the full component renderer ran");
    assert_eq!(replacement_rows.get(), 1);
    assert!(output.contains("new mutable tail"), "{output:?}");
    assert!(
        !output.contains("historic row"),
        "stable history was emitted"
    );
    assert_eq!(tui.previous_frame.len(), HISTORY + 1);
}

#[test]
fn lazy_fixed_height_update_emits_only_exact_changed_rows() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    let size = Rc::new(Cell::new((40, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, tail_clears, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "history".to_owned(),
        "new event".to_owned(),
        String::new(),
        String::new(),
        "composer top".to_owned(),
        "composer input".to_owned(),
        "composer bottom".to_owned(),
        "footer telemetry".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyFixedLines { lines }));
    tui.previous_frame = [
        "history",
        "",
        "",
        "",
        "composer top",
        "composer input",
        "composer bottom",
        "footer telemetry",
    ]
    .into_iter()
    .map(|line| format!("{line}{RESET}"))
    .collect();
    tui.previous_size = Some((40, 8));
    tui.first_render = false;
    tui.inline_bottom_row = 7;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    assert!(output.contains("new event"), "{output:?}");
    assert!(!output.contains("composer"), "{output:?}");
    assert!(!output.contains("footer telemetry"), "{output:?}");
    assert_eq!(tail_clears.get(), 0, "the pinned tail must not be erased");
    assert!(
        output.len() < 80,
        "unexpected repaint payload: {} B",
        output.len()
    );
}

#[test]
fn lazy_fixed_height_image_removal_deletes_kitty_placements_before_redraw() {
    const RESET: &str = "\x1b[0m\x1b]8;;\x1b\\";
    const KITTY_IMAGE: &str = "\x1b_Ga=T,f=100,i=1,s=1,v=1,c=1,r=1;AAAA\x1b\\";
    let size = Rc::new(Cell::new((40, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::TrueColor,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "unchanged top".to_owned(),
        "image replaced by text".to_owned(),
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
    tui.previous_size = Some((40, 4));
    tui.first_render = false;
    tui.inline_bottom_row = 3;
    tui.running = true;

    tui.request_render();

    let output = writes.borrow().join("");
    let delete = delete_all_kitty_images();
    let delete_at = output.find(&delete).expect("Kitty placements were deleted");
    let replacement_at = output
        .find("image replaced by text")
        .expect("replacement row was painted");
    assert!(delete_at < replacement_at, "{output:?}");
    assert!(
        output.contains("unchanged top") && output.contains("unchanged bottom"),
        "the complete viewport must be restored after a global image delete: {output:?}"
    );
}

#[test]
fn pi_pure_append_and_shrink_update_only_the_changed_tail() {
    let size = Rc::new(Cell::new((20, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec!["first".to_owned()]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();
    writes.borrow_mut().clear();

    lines.borrow_mut().push("second".to_owned());
    tui.request_render();
    let append = writes.borrow().join("");
    assert!(append.contains("\r\n"), "{append:?}");
    assert!(append.contains("second"), "{append:?}");
    assert!(!append.contains("\x1b[3J"), "{append:?}");

    writes.borrow_mut().clear();
    lines.borrow_mut().truncate(1);
    tui.request_render();
    let shrink = writes.borrow().join("");
    assert!(shrink.contains("\x1b[2K"), "{shrink:?}");
    assert!(!shrink.contains("\x1b[3J"), "{shrink:?}");
}

#[test]
fn lazy_replacement_extracts_cursor_before_width_clipping() {
    let size = Rc::new(Cell::new((3, 2)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, shows, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![format!("abcdef{CURSOR_MARKER}")]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(LazyFixedLines { lines }));
    tui.start();

    let output = writes.borrow().join("");
    assert!(!output.contains(CURSOR_MARKER), "{output:?}");
    assert!(output.contains("\x1b[1;3H"), "{output:?}");
    assert_eq!(shows.get(), 1);
}
