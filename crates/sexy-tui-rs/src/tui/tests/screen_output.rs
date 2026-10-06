//! What reaches the wire: escape-free plain output, OSC 2 window
//! titles, cursor addressing that never ends in a scrolling
//! newline, synchronized-update framing, and alternate-screen
//! entry and exit.
//!
//! These assertions were extracted from `tui.rs`; each module is a
//! child of `crate::tui::tests`, so `use super::*` reaches exactly
//! the private items it reached while the tests were inline.

use super::*;

#[test]
fn desktop_notification_uses_host_osc777_and_refuses_controls_or_plain_output() {
    let size = Rc::new(Cell::new((20, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    assert!(tui.desktop_notification("π", "Hello; world"));
    assert_eq!(
        writes.borrow().join(""),
        "\x1b]777;notify;π;Hello; world\x07"
    );
    for body in ["bad\x07", "bad\x1b]2;injection", "bad\nline"] {
        assert!(!tui.desktop_notification("title", body));
    }
    assert!(!tui.desktop_notification("bad;title", "body"));
    assert!(!tui.desktop_notification("title", &"x".repeat(4097)));
    assert_eq!(writes.borrow().len(), 1);
    let (terminal, _, _, _, _, writes) =
        recording_terminal(size, crate::capabilities::TerminalCapabilities::plain());
    let mut plain = TUI::new(Box::new(terminal));
    assert!(!plain.desktop_notification("title", "body"));
    assert!(writes.borrow().is_empty());
}

#[test]
fn window_title_is_osc2_with_controls_stripped_and_silent_when_plain() {
    let size = Rc::new(Cell::new((20, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_window_title("octet · model\x07\x1b · thinking");
    assert_eq!(
        writes.borrow().join(""),
        "\x1b]2;octet · model · thinking\x07"
    );

    let (terminal, _, _, _, _, writes) =
        recording_terminal(size, crate::capabilities::TerminalCapabilities::plain());
    let mut plain = TUI::new(Box::new(terminal));
    plain.set_window_title("octet");
    assert!(writes.borrow().is_empty());
}

#[test]
fn cursor_addressed_frames_never_end_with_a_scrolling_newline() {
    let size = Rc::new(Cell::new((20, 2)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec!["first".to_owned(), "second".to_owned()]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();
    assert!(!writes.borrow().join("").ends_with('\n'));

    writes.borrow_mut().clear();
    lines.borrow_mut()[1] = "changed".into();
    tui.request_render();
    assert!(!writes.borrow().join("").ends_with('\n'));
}

#[test]
fn plain_backend_is_escape_free_ascii_structured_and_not_right_padded() {
    let size = Rc::new(Cell::new((20, 8)));
    let (terminal, _, _, _, _, writes) =
        recording_terminal(size, crate::TerminalCapabilities::plain());
    let lines = Rc::new(RefCell::new(vec!["safe\x1b]52;c;bad\x07".into()]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(MutableLines(lines)));
    tui.start();
    let output = writes.borrow().join("");
    assert!(!output.contains('\x1b'));
    assert!(!output.contains('\x07'));
    assert!(output.starts_with("safe^["));
    assert!(!output.contains("                    "));
}

#[test]
fn pi_cursor_marker_uses_unclipped_display_cells() {
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let cases = [
        (2, format!("界{CURSOR_MARKER}x"), "\x1b[3G"),
        (3, format!("abcdef{CURSOR_MARKER}"), "\x1b[7G"),
    ];

    for (width, line, expected_cursor) in cases {
        let size = Rc::new(Cell::new((width, 2)));
        let (terminal, _, _, _, shows, writes) = recording_terminal(size, capabilities);
        let mut tui = TUI::new(Box::new(terminal));
        tui.set_show_hardware_cursor(true);
        tui.add_child(Box::new(MutableLines(Rc::new(RefCell::new(vec![line])))));
        tui.start();

        let output = writes.borrow().join("");
        assert!(!output.contains(CURSOR_MARKER), "{output:?}");
        assert!(output.contains(expected_cursor), "{output:?}");
        assert_eq!(shows.get(), 1, "width {width} did not reveal the cursor");
    }
}

#[test]
fn pi_synchronized_frame_includes_ime_cursor_positioning() {
    let size = Rc::new(Cell::new((20, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, stops, shows, writes) = recording_terminal(size, capabilities);
    let lines = Rc::new(RefCell::new(vec![format!("界{CURSOR_MARKER}x")]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.add_child(Box::new(MutableLines(lines)));
    tui.start();
    let output = writes.borrow().join("");
    assert!(!output.contains(CURSOR_MARKER));
    let begin = output.find("\x1b[?2026h").expect("Pi frame begin");
    let end = output.find("\x1b[?2026l").expect("Pi frame end");
    let cursor = output.rfind("\x1b[3G").expect("IME cursor column");
    assert!(begin < cursor && cursor < end, "{output:?}");
    assert_eq!(shows.get(), 0, "Pi hides the hardware cursor by default");
    drop(output);

    tui.stop();
    assert_eq!(stops.get(), 1);
    assert_eq!(shows.get(), 1, "stop restores the user's cursor");
    assert!(writes.borrow().join("").contains("\r\n"));
}

#[test]
fn pi_cursor_and_visibility_are_atomic_for_full_diff_shrink_and_resize() {
    let size = Rc::new(Cell::new((20, 8)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        true,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities);
    let lines = Rc::new(RefCell::new(vec![
        "heading".to_owned(),
        format!("draft{CURSOR_MARKER}"),
        "tail".to_owned(),
    ]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_show_hardware_cursor(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    for phase in 0..4 {
        writes.borrow_mut().clear();
        match phase {
            0 => tui.start(),
            1 => {
                lines.borrow_mut()[0] = "changed".into();
                tui.request_render();
            }
            2 => {
                lines.borrow_mut().pop();
                tui.request_render();
            }
            _ => {
                size.set((24, 9));
                tui.request_render();
            }
        }
        let output = writes.borrow().join("");
        let begin = output.find("\x1b[?2026h").unwrap();
        let end = output.find("\x1b[?2026l").unwrap();
        let cursor = output.rfind("\x1b[6G").unwrap();
        let visible = output.rfind("\x1b[?25h").unwrap();
        assert!(
            begin < cursor && cursor < visible && visible < end,
            "phase={phase}: {output:?}"
        );
        assert_eq!(output.matches("\x1b[?2026h").count(), 1);
        assert!(
            writes
                .borrow()
                .iter()
                .any(|write| write.contains("\x1b[?2026h")
                    && write.contains("\x1b[6G")
                    && write.contains("\x1b[?2026l")),
            "split frame write"
        );
    }
}

#[test]
fn alternate_screen_enters_disables_autowrap_and_restores_the_document() {
    let size = Rc::new(Cell::new((24, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, stops, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_alternate_screen(true);
    tui.set_show_hardware_cursor(false);
    tui.add_child(Box::new(MutableLines(Rc::new(RefCell::new(vec![
        "alpha".to_owned(),
        "beta".to_owned(),
        "gamma".to_owned(),
        "delta".to_owned(),
        "epsilon".to_owned(),
    ])))));
    tui.start();
    let entered = writes.borrow().join("");
    assert!(entered.starts_with("\x1b[?1049h\x1b[?7l"), "{entered:?}");
    assert!(
        entered.contains("\x1b[1;1H\x1b[2Kbeta"),
        "the fixed viewport paints the tail window at absolute rows: {entered:?}"
    );
    assert!(entered.contains("\x1b[4;1H\x1b[2Kepsilon"), "{entered:?}");
    assert!(
        !entered.contains("\x1b[?7h"),
        "autowrap stays off while active"
    );
    assert!(tui.alternate_screen_active());
    assert_eq!(
        tui.rendered_frame().len(),
        5,
        "the retained frame is the complete document for /debug and the exit handoff"
    );

    writes.borrow_mut().clear();
    tui.stop();

    assert_eq!(stops.get(), 1);
    assert!(!tui.alternate_screen_active());
    let output = writes.borrow().join("");
    assert!(output.starts_with("\x1b[?1049l"), "{output:?}");
    assert!(
        output.contains("\r\x1b[2Kalpha") && output.contains("\r\x1b[2Kepsilon"),
        "the final document is restored to the main screen: {output:?}"
    );
    assert!(
        output.contains("\x1b[?7h"),
        "autowrap returns to the terminal: {output:?}"
    );
    assert!(output.contains("\x1b[?25h"), "{output:?}");
}

#[test]
fn alternate_screen_reuses_history_on_tail_updates() {
    let size = Rc::new(Cell::new((24, 4)));
    let capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_alternate_screen(true);
    let history = 10_000;
    let rows = Rc::new(RefCell::new(vec!["history".to_owned(); history]));
    tui.add_child(Box::new(MutableLines(rows)));
    tui.start();
    let retained = tui.previous_frame[0].as_ptr();
    let full_renders = Rc::new(Cell::new(0));
    let tail = Rc::new(RefCell::new("Working".to_owned()));
    tui.children[0] = Box::new(LazyTail {
        stable_prefix: history,
        tail: tail.clone(),
        full_renders: full_renders.clone(),
        replacement_rows: Rc::new(Cell::new(0)),
    });
    for tick in 0..10 {
        *tail.borrow_mut() = format!("Working {tick}");
        writes.borrow_mut().clear();
        tui.request_render();
        assert_eq!(tui.previous_frame.len(), history + 1);
        assert_eq!(tui.previous_frame[0].as_ptr(), retained);
        assert!(tui.previous_frame[history].contains(&format!("Working {tick}")));
        assert!(writes.borrow().join("").len() < 512);
    }
    assert_eq!(full_renders.get(), 0);
}

#[test]
fn alternate_screen_stays_on_the_primary_screen_without_cursor_addressing() {
    let size = Rc::new(Cell::new((24, 4)));
    let mut capabilities = crate::capabilities::TerminalCapabilities::interactive(
        crate::capabilities::ColorDepth::Ansi16,
        false,
    );
    capabilities.cursor_addressing = false;
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities);
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_alternate_screen(true);
    tui.add_child(Box::new(MutableLines(Rc::new(RefCell::new(vec![
        "primary".to_owned(),
    ])))));
    tui.start();
    let output = writes.borrow().join("");
    assert!(
        !output.contains("\x1b[?1049h"),
        "no alternate-screen escape without cursor addressing: {output:?}"
    );
    assert!(!tui.alternate_screen_active());
    assert!(output.contains("primary"), "{output:?}");
}
