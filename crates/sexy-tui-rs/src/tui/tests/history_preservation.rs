//! Octet's opt-in native-history policy; generic Pi parity stays covered separately.
use super::*;

fn capabilities() -> crate::TerminalCapabilities {
    crate::TerminalCapabilities::interactive(crate::ColorDepth::TrueColor, true)
}

#[test]
fn preserved_scrollback_resize_output_is_viewport_bounded_and_cursor_atomic() {
    for count in [32, 256, 2048, 16_384] {
        let size = Rc::new(Cell::new((80, 24)));
        let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities());
        let lines = Rc::new(RefCell::new(
            (0..count)
                .map(|i| format!("row-{i:05}"))
                .collect::<Vec<_>>(),
        ));
        lines.borrow_mut()[count - 2].push_str(CURSOR_MARKER);
        let mut tui = TUI::new(Box::new(terminal));
        tui.set_preserve_scrollback(true);
        tui.set_show_hardware_cursor(true);
        tui.add_child(Box::new(MutableLines(lines.clone())));
        tui.start();
        writes.borrow_mut().clear();
        size.set((64, 18));
        tui.request_render();
        let output = writes.borrow().join("");
        assert!(!output.contains("\x1b[2J") && !output.contains("\x1b[3J"));
        assert!(!output.contains("row-00000"));
        assert!(
            output.len() < 1400,
            "history={count}, bytes={}",
            output.len()
        );
        assert_eq!(output.matches("row-").count(), 18);
        assert!(output.starts_with("\x1b[?2026h"));
        assert!(output.ends_with("\x1b[?25h\x1b[?2026l"));
        let mut parser = vt100::Parser::new(18, 64, 0);
        parser.process(output.as_bytes());
        assert_eq!(parser.screen().cursor_position(), (16, 9));
        assert_eq!(tui.rendered_frame().len(), count);
        assert!(parser
            .screen()
            .contents()
            .contains(&format!("row-{:05}", count - 1)));
        writes.borrow_mut().clear();
        tui.request_render();
        assert!(!writes.borrow().join("").contains("row-"));
    }
}

#[test]
fn preserved_history_historical_edit_and_large_shrink_repair_only_live_cells() {
    let size = Rc::new(Cell::new((40, 6)));
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities());
    let lines = Rc::new(RefCell::new((0..100).map(|i| format!("old-{i}")).collect()));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_preserve_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();
    writes.borrow_mut().clear();
    lines.borrow_mut()[0] = "canonical correction".into();
    tui.request_render();
    let output = writes.borrow().join("");
    assert!(!output.contains("\x1b[3J") && !output.contains("canonical correction"));
    assert!(tui.rendered_frame()[0].contains("canonical correction"));
    writes.borrow_mut().clear();
    *lines.borrow_mut() = vec![format!("short{CURSOR_MARKER}")];
    tui.request_render();
    let output = writes.borrow().join("");
    let mut parser = vt100::Parser::new(6, 40, 0);
    parser.process(b"stale\r\nstale\r\nstale");
    parser.process(output.as_bytes());
    assert_eq!(parser.screen().contents(), "short");
    assert_eq!(parser.screen().cursor_position(), (0, 5));
    assert!(!output.contains("\x1b[2J") && !output.contains("\x1b[3J"));
}

#[test]
fn preserved_history_height_resize_reuses_a_lazy_prefix() {
    let size = Rc::new(Cell::new((80, 24)));
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities());
    let full_renders = Rc::new(Cell::new(0));
    struct LazyHistory(Rc<Cell<usize>>);
    impl Component for LazyHistory {
        fn render(&self, _: u16) -> Vec<String> {
            self.0.set(self.0.get() + 1);
            let mut rows = (0..4095).map(|i| format!("row-{i}")).collect::<Vec<_>>();
            rows.push("tail".into());
            rows
        }
        fn render_update(&self, _: u16) -> Option<FrameUpdate> {
            Some(FrameUpdate {
                stable_prefix: 4095,
                replacement: vec!["tail".into()],
                pinned: None,
                resize_replay: None,
                reanchor_viewport: false,
                rebuild_scrollback: false,
            })
        }
        fn invalidate(&mut self) {}
    }
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_preserve_scrollback(true);
    tui.add_child(Box::new(LazyHistory(full_renders.clone())));
    tui.start();
    writes.borrow_mut().clear();
    size.set((80, 18));
    tui.request_render();
    assert_eq!(full_renders.get(), 1);
    assert!(tui.last_pi_lazy_inspected_rows < 32);
    assert!(writes.borrow().join("").len() < 1400);
}

#[test]
fn preserved_history_image_fallback_keeps_the_roots_reanchor_request() {
    struct ImageReanchor {
        reanchor: Rc<Cell<bool>>,
        lines: Vec<String>,
    }
    impl Component for ImageReanchor {
        fn render(&self, _: u16) -> Vec<String> {
            self.lines.clone()
        }
        fn render_update(&self, _: u16) -> Option<FrameUpdate> {
            Some(FrameUpdate {
                stable_prefix: 0,
                replacement: self.lines.clone(),
                pinned: None,
                resize_replay: None,
                reanchor_viewport: self.reanchor.replace(false),
                rebuild_scrollback: false,
            })
        }
        fn invalidate(&mut self) {}
    }
    let size = Rc::new(Cell::new((40, 6)));
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities());
    let reanchor = Rc::new(Cell::new(false));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_preserve_scrollback(true);
    tui.add_child(Box::new(ImageReanchor {
        reanchor: reanchor.clone(),
        lines: vec!["\x1b_Ga=T,i=77,c=2,r=2;AAAA\x1b\\".into(), String::new()],
    }));
    tui.start();
    writes.borrow_mut().clear();
    // Same final dimensions/bytes after an away-and-back resize still require
    // repair, even though the image guard rejects the lazy frame.
    reanchor.set(true);
    tui.request_render();
    let output = writes.borrow().join("");
    assert!(output.contains("\x1b[1;1H\x1b[2K"));
    assert!(output.contains(&delete_kitty_image(77)));
    assert!(output.contains("\x1b_Ga=T,i=77"));
    assert!(!output.contains("\x1b[3J"));
}

#[test]
fn preserved_history_forced_reanchor_retires_visible_images_before_repainting() {
    let size = Rc::new(Cell::new((40, 6)));
    let (terminal, _, _, _, _, writes) = recording_terminal(size, capabilities());
    let mut document = (0..32).map(|i| format!("saved-{i}")).collect::<Vec<_>>();
    document.extend([
        "\x1b_Ga=T,i=77,c=2,r=2;AAAA\x1b\\".into(),
        String::new(),
        format!("old composer{CURSOR_MARKER}"),
    ]);
    let lines = Rc::new(RefCell::new(document));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_preserve_scrollback(true);
    tui.set_show_hardware_cursor(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();
    writes.borrow_mut().clear();
    *lines.borrow_mut() = vec![format!("restored{CURSOR_MARKER}")];
    tui.request_render_force(true);
    let output = writes.borrow().join("");
    assert!(output.contains(&delete_kitty_image(77)));
    assert!(!output.contains("saved-") && !output.contains("\x1b[3J"));
    let mut parser = vt100::Parser::new(6, 40, 0);
    parser.process(output.as_bytes());
    assert_eq!(parser.screen().contents(), "restored");
    assert_eq!(parser.screen().cursor_position(), (0, 8));
    assert!(!parser.screen().hide_cursor());
}

#[test]
fn preserved_history_reanchor_restores_new_kitty_placements_and_deletes_visible_old_ids() {
    let size = Rc::new(Cell::new((40, 6)));
    let (terminal, _, _, _, _, writes) = recording_terminal(size.clone(), capabilities());
    let lines = Rc::new(RefCell::new(vec!["plain".into()]));
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_preserve_scrollback(true);
    tui.add_child(Box::new(MutableLines(lines.clone())));
    tui.start();
    *lines.borrow_mut() = vec!["\x1b_Ga=T,i=77,c=2,r=2;AAAA\x1b\\".into(), String::new()];
    size.set((32, 5));
    tui.request_render();
    assert!(tui.previous_kitty_image_ids.contains(&77));
    writes.borrow_mut().clear();
    *lines.borrow_mut() = vec!["replacement".into()];
    size.set((30, 4));
    tui.request_render();
    let output = writes.borrow().join("");
    assert!(output.contains(&delete_kitty_image(77)));
    assert!(!output.contains("\x1b[3J"));
}
