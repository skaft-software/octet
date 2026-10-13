//! Regressions found by comparing real renderer traces with Pi 1.0.2.
use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;

use super::TUI;
use crate::{ColorDepth, Component, Terminal, TerminalCapabilities, TerminalInput};

#[derive(Clone)]
struct Lines(Rc<RefCell<Vec<String>>>);
impl Component for Lines {
    fn render(&self, _: u16) -> Vec<String> {
        self.0.borrow().clone()
    }
    fn invalidate(&mut self) {}
}

struct Capture {
    size: Rc<Cell<(u16, u16)>>,
    writes: Rc<RefCell<Vec<String>>>,
}
impl Terminal for Capture {
    fn start_events(&mut self, _: Box<dyn FnMut(TerminalInput)>, _: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_owned());
    }
    fn columns(&self) -> u16 {
        self.size.get().0
    }
    fn rows(&self) -> u16 {
        self.size.get().1
    }
    fn move_by(&mut self, lines: i16) {
        if lines != 0 {
            self.write(&format!(
                "\x1b[{}{}",
                lines.unsigned_abs(),
                if lines > 0 { 'B' } else { 'A' }
            ));
        }
    }
    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }
    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }
    fn clear_line(&mut self) {
        self.write("\x1b[2K");
    }
    fn clear_from_cursor(&mut self) {
        self.write("\x1b[0J");
    }
    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }
    fn capabilities(&self) -> TerminalCapabilities {
        TerminalCapabilities::interactive(ColorDepth::Ansi16, true)
    }
}

struct Harness {
    tui: TUI<'static>,
    lines: Rc<RefCell<Vec<String>>>,
    size: Rc<Cell<(u16, u16)>>,
    writes: Rc<RefCell<Vec<String>>>,
}
impl Harness {
    fn new(lines: Vec<String>) -> Self {
        let lines = Rc::new(RefCell::new(lines));
        let size = Rc::new(Cell::new((16, 6)));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let mut tui = TUI::new(Box::new(Capture {
            size: size.clone(),
            writes: writes.clone(),
        }));
        tui.set_clear_on_shrink(false);
        tui.set_show_hardware_cursor(false);
        tui.add_child(Box::new(Lines(lines.clone())));
        // Configuration may emit a cursor change when PI_HARDWARE_CURSOR=1.
        writes.borrow_mut().clear();
        Self {
            tui,
            lines,
            size,
            writes,
        }
    }
}

// Environment-dependent cases run in isolated processes, not parallel env mutation.
fn child_with_termux(test: &str, value: &str) -> bool {
    const CHILD: &str = "OCTET_RELEASE_PARITY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("tui::release_parity_tests::{test}"),
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("TERMUX_VERSION", value)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    true
}

#[test]
fn empty_termux_variable_does_not_suppress_resize_replay() {
    if child_with_termux("empty_termux_variable_does_not_suppress_resize_replay", "") {
        return;
    }
    let mut h = Harness::new(vec!["hello".into()]);
    h.tui.start();
    h.writes.borrow_mut().clear();
    h.size.set((16, 10));
    h.tui.request_render();
    assert!(h.writes.borrow().join("").contains("\x1b[2J\x1b[H\x1b[3J"));
}

#[test]
fn empty_termux_resize_then_append_preserves_saved_lines() {
    if child_with_termux("empty_termux_resize_then_append_preserves_saved_lines", "1") {
        return;
    }
    let mut h = Harness::new(vec![]);
    h.tui.start();
    h.size.set((16, 1));
    h.tui.request_render();
    h.writes.borrow_mut().clear();
    *h.lines.borrow_mut() = vec!["new".into()];
    h.tui.request_render();
    let wire = h.writes.borrow().join("");
    assert!(
        !wire.contains("\x1b[3J"),
        "append destroyed saved lines: {wire:?}"
    );
    assert!(wire.contains("new"));
}

#[test]
fn iterm_image_lines_do_not_receive_text_resets() {
    let image = "\x1b]1337;File=inline=1:AAAA\x07";
    let mut h = Harness::new(vec![image.into(), "after".into()]);
    h.tui.start();
    let wire = h.writes.borrow().join("");
    assert!(wire.contains(&format!("{image}\r\nafter")), "{wire:?}");
    h.writes.borrow_mut().clear();
    *h.lines.borrow_mut() = vec!["before".into(), image.into(), "after".into()];
    h.tui.request_render();
    let wire = h.writes.borrow().join("");
    assert!(wire.contains(&format!("{image}\r\n")), "{wire:?}");
}

fn assert_bounded(writes: &[String]) {
    assert!(writes.len() > 2, "large frame was not split");
    assert!(
        writes.iter().all(|w| w.len() <= 1024 * 1024),
        "write exceeds 1 MiB"
    );
    // Every write is a valid Rust string; rejoining must also preserve codepoints.
    assert!(writes
        .iter()
        .all(|w| w.encode_utf16().count() <= 1024 * 1024));
}

#[test]
fn large_full_render_has_bounded_unicode_safe_writes() {
    let image = format!("\x1b_Ga=T,f=100;{}\x1b\\", "🙂".repeat(600_000));
    let mut h = Harness::new(vec![image.clone()]);
    h.tui.start();
    let writes = h.writes.borrow();
    assert_bounded(&writes);
    assert_eq!(
        writes.join(""),
        format!("\x1b[?25l\x1b[?2026h{image}\x1b[?25l\x1b[?2026l")
    );
}

#[test]
fn large_differential_render_has_bounded_writes_without_replay() {
    let mut h = Harness::new(vec!["before".into()]);
    h.tui.start();
    let redraws = h.tui.full_redraws();
    h.writes.borrow_mut().clear();
    let image = format!("\x1b_Ga=T,f=100;{}\x1b\\", "A".repeat(2_400_000));
    *h.lines.borrow_mut() = vec!["before".into(), image.clone()];
    h.tui.request_render();
    let writes = h.writes.borrow();
    assert_bounded(&writes);
    assert_eq!(h.tui.full_redraws(), redraws);
    assert_eq!(
        writes.join(""),
        format!("\x1b[?2026h\r\n\x1b[2K{image}\x1b[?25l\x1b[?2026l")
    );
}
