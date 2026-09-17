//! Alternate-screen session: enter/exit, autowrap ownership, a fixed viewport,
//! and the final document restored to the main screen.
//!
//! Rust port of Pi's fullscreen terminal lifecycle
//! (`packages/tui/src/tui-alt-screen.ts`: `beforeTerminalStart`,
//! `beforeTerminalStop`, `afterTerminalStop`, and `doRender`). A session owns
//! three terminal-level facts the primary-screen renderer never has to state:
//!
//! 1. Autowrap is disabled for the whole session (`\x1b[?7l`). With a fixed
//!    viewport every row is written at an absolute address, so a row of exactly
//!    the terminal width must never wrap and steal the next row's address.
//! 2. The viewport is fixed: each frame paints rows `1..=height` absolutely and
//!    crops over-wide rows to the terminal width, so the terminal's own
//!    scrollback is never used as storage and nothing is scrolled implicitly.
//! 3. On exit the document is handed back to the *main* screen, so the reader's
//!    terminal keeps the complete rendered transcript after the session ends.
//!
//! This module performs no I/O of its own; it writes only through the
//! [`Terminal`] the caller owns, and `enter` refuses a terminal that cannot
//! address the cursor (the caller then stays on the primary screen).

use crate::terminal::Terminal;
use crate::utils::slice_by_column;

/// Enter the alternate screen.
pub const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
/// Leave the alternate screen, restoring the main screen.
pub const EXIT_ALT_SCREEN: &str = "\x1b[?1049l";
/// Disable autowrap for the session.
pub const DISABLE_AUTOWRAP: &str = "\x1b[?7l";
/// Restore the terminal's autowrap policy.
pub const ENABLE_AUTOWRAP: &str = "\x1b[?7h";
/// Mouse reporting for click, drag-button, focus, and SGR coordinates.
pub const ENABLE_BUTTON_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h";
/// Mouse reporting that also forwards pointer motion with no button held.
pub const ENABLE_ALL_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
/// Disable every mouse reporting mode this session may have enabled.
pub const DISABLE_MOUSE: &str = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
/// Begin a synchronized output transaction.
pub const BEGIN_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026h";
/// End a synchronized output transaction.
pub const END_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026l";

/// Session options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AltScreenOptions {
    /// Enable mouse reporting with the session.
    pub mouse: bool,
    /// Forward pointer motion with no button held (multiplexers lag when every
    /// movement is forwarded, so this stays opt-in).
    pub all_motion_mouse: bool,
    /// Wrap frames in synchronized output when the terminal supports it.
    pub synchronized_output: bool,
}

/// Owns one alternate-screen session.
pub struct AlternateScreen {
    options: AltScreenOptions,
    active: bool,
    window: Vec<String>,
    last_size: Option<(u16, u16)>,
}

impl AlternateScreen {
    /// A session that has not entered the alternate screen yet.
    pub fn new(options: AltScreenOptions) -> Self {
        Self {
            options,
            active: false,
            window: Vec::new(),
            last_size: None,
        }
    }

    /// Whether the session currently owns the alternate screen.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The rows painted by the last [`AlternateScreen::paint`].
    pub fn window(&self) -> &[String] {
        &self.window
    }

    /// Drop the retained window so the next [`AlternateScreen::paint`] is a
    /// full redraw. Used by [`crate::tui::TUI::request_render_force`].
    pub fn invalidate(&mut self) {
        self.window.clear();
        self.last_size = None;
    }

    /// Enter the alternate screen with autowrap disabled and the viewport
    /// cleared. Returns `false` (and writes nothing) when the terminal cannot
    /// address the cursor, which is the caller's signal to stay on the main
    /// screen.
    pub fn enter(&mut self, terminal: &mut dyn Terminal) -> bool {
        if self.active {
            return true;
        }
        if !terminal.capabilities().cursor_addressing {
            return false;
        }
        let mouse = if self.options.mouse {
            if self.options.all_motion_mouse {
                ENABLE_ALL_MOTION_MOUSE
            } else {
                ENABLE_BUTTON_MOTION_MOUSE
            }
        } else {
            ""
        };
        terminal.write(&format!(
            "{ENTER_ALT_SCREEN}{DISABLE_AUTOWRAP}{mouse}\x1b[2J\x1b[H\x1b[?25l"
        ));
        self.active = true;
        self.window.clear();
        self.last_size = None;
        true
    }

    /// Paint one fixed-viewport frame, diffing against the previous one.
    ///
    /// `lines` is the viewport window from top to bottom. It is padded to
    /// `height` and its last `height` rows are used when it is longer, each row
    /// is cropped to `width`, and only changed rows are addressed. `cursor` is
    /// a zero-based `(row, column)` inside the window for IME placement.
    pub fn paint(
        &mut self,
        terminal: &mut dyn Terminal,
        lines: &[String],
        width: u16,
        height: u16,
        cursor: Option<(usize, usize)>,
        show_hardware_cursor: bool,
    ) {
        if !self.active {
            return;
        }
        // Terminal dimensions stay `u16` (their source type); only row indices
        // and lengths are `usize`, so each comparison converts at the point of
        // use instead of narrowing an already-widened value.
        let width = width.max(1);
        let height = height.max(1);
        let rows = window_rows(lines, usize::from(height));
        let full_redraw =
            self.window.len() != usize::from(height) || self.last_size != Some((width, height));
        let mut buffer = String::new();
        if self.options.synchronized_output {
            buffer.push_str(BEGIN_SYNCHRONIZED_OUTPUT);
        }
        if full_redraw {
            buffer.push_str("\x1b[2J");
        }
        for (row, line) in rows.iter().enumerate() {
            if !full_redraw && self.window.get(row) == Some(line) {
                continue;
            }
            buffer.push_str(&format!(
                "\x1b[{};1H\x1b[2K{}",
                row + 1,
                crop_to_width(line, width)
            ));
        }
        match cursor.filter(|(row, _)| *row < usize::from(height)) {
            Some((row, column)) => {
                buffer.push_str(&format!(
                    "\x1b[{};{}H",
                    row + 1,
                    usize::from(column).min(usize::from(width)) + 1
                ));
                buffer.push_str(if show_hardware_cursor {
                    "\x1b[?25h"
                } else {
                    "\x1b[?25l"
                });
            }
            None => buffer.push_str("\x1b[?25l"),
        }
        if self.options.synchronized_output {
            buffer.push_str(END_SYNCHRONIZED_OUTPUT);
        }
        terminal.write(&buffer);
        self.window = rows;
        self.last_size = Some((width, height));
    }

    /// Leave the alternate screen.
    ///
    /// With `preserve_screen` the main screen is simply restored and the cursor
    /// shown. Otherwise the complete `document` is replayed onto the main
    /// screen: autowrap stays disabled while each row is written at column one
    /// with a clear-line, then the terminal's autowrap policy is restored and
    /// the cursor moves past the document. Rows are cropped to the terminal
    /// width exactly as Pi crops its final document.
    pub fn exit(
        &mut self,
        terminal: &mut dyn Terminal,
        document: &[String],
        preserve_screen: bool,
    ) {
        if !self.active {
            return;
        }
        let width = terminal.columns().max(1);
        let mut buffer = String::new();
        if self.options.synchronized_output {
            buffer.push_str(BEGIN_SYNCHRONIZED_OUTPUT);
        }
        if preserve_screen {
            buffer.push_str(EXIT_ALT_SCREEN);
            buffer.push_str("\x1b[?25h");
        } else {
            buffer.push_str(EXIT_ALT_SCREEN);
            buffer.push_str(DISABLE_AUTOWRAP);
            for (row, line) in document.iter().enumerate() {
                if row > 0 {
                    buffer.push_str("\r\n");
                }
                buffer.push_str("\r\x1b[2K");
                buffer.push_str(&crop_to_width(line, width));
            }
            buffer.push_str("\x1b[0m");
            buffer.push_str(ENABLE_AUTOWRAP);
            buffer.push_str("\r\n\x1b[?25h");
        }
        if self.options.synchronized_output {
            buffer.push_str(END_SYNCHRONIZED_OUTPUT);
        }
        terminal.write(&buffer);
        self.active = false;
        self.window.clear();
        self.last_size = None;
    }
}

fn window_rows(lines: &[String], height: usize) -> Vec<String> {
    let mut rows = if lines.len() > height {
        lines[lines.len() - height..].to_vec()
    } else {
        lines.to_vec()
    };
    rows.resize(height, String::new());
    rows
}

fn crop_to_width(line: &str, width: u16) -> String {
    // An image payload is not text: cropping one would corrupt the placement
    // sequence, so it is handed through whole (Pi's `isImageLine` fast path).
    if line.contains("\x1bP") || line.contains("\x1b_G") {
        return line.to_owned();
    }
    if crate::utils::visible_width(line) <= usize::from(width) {
        return line.to_owned();
    }
    slice_by_column(line, 0, usize::from(width), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::TerminalCapabilities;
    use crate::terminal::TerminalInput;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct RecordingTerminal {
        clear_on_write: Rc<Cell<bool>>,
        size: (u16, u16),
        writes: Rc<RefCell<String>>,
        capabilities: TerminalCapabilities,
    }

    impl Terminal for RecordingTerminal {
        fn start_events(
            &mut self,
            _on_input: Box<dyn FnMut(TerminalInput)>,
            _on_resize: Box<dyn FnMut()>,
        ) {
        }

        fn stop(&mut self) {}

        fn write(&mut self, data: &str) {
            let mut buffer = self.writes.borrow_mut();
            if self.clear_on_write.get() {
                buffer.clear();
            }
            buffer.push_str(data);
        }

        fn columns(&self) -> u16 {
            self.size.0
        }

        fn rows(&self) -> u16 {
            self.size.1
        }

        fn move_by(&mut self, _lines: i16) {}

        fn hide_cursor(&mut self) {}

        fn show_cursor(&mut self) {}

        fn clear_line(&mut self) {}

        fn clear_from_cursor(&mut self) {}

        fn clear_screen(&mut self) {}

        fn capabilities(&self) -> TerminalCapabilities {
            self.capabilities
        }
    }

    fn recording(size: (u16, u16)) -> (RecordingTerminal, Rc<RefCell<String>>, Rc<Cell<bool>>) {
        let writes: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let clear_on_write = Rc::new(Cell::new(false));
        (
            RecordingTerminal {
                clear_on_write: clear_on_write.clone(),
                size,
                writes: writes.clone(),
                capabilities: TerminalCapabilities::interactive(crate::ColorDepth::None, true),
            },
            writes,
            clear_on_write,
        )
    }

    fn options(synchronized_output: bool) -> AltScreenOptions {
        AltScreenOptions {
            mouse: false,
            all_motion_mouse: false,
            synchronized_output,
        }
    }

    #[test]
    fn enter_disables_autowrap_and_clears_the_fixed_viewport() {
        let (mut terminal, writes, _) = recording((80, 24));
        let mut session = AlternateScreen::new(options(false));
        assert!(session.enter(&mut terminal));
        assert_eq!(
            writes.borrow().as_str(),
            "\x1b[?1049h\x1b[?7l\x1b[2J\x1b[H\x1b[?25l"
        );
        assert!(session.is_active());
    }

    #[test]
    fn enter_refuses_a_terminal_without_cursor_addressing() {
        let (mut terminal, writes, _) = recording((80, 24));
        let mut capabilities = terminal.capabilities;
        capabilities.cursor_addressing = false;
        terminal.capabilities = capabilities;
        let mut session = AlternateScreen::new(options(false));
        assert!(!session.enter(&mut terminal));
        assert!(writes.borrow().is_empty());
        assert!(!session.is_active());
    }

    #[test]
    fn paint_addresses_every_row_absolutely_and_crops_to_width() {
        let (mut terminal, writes, clear) = recording((10, 4));
        let mut session = AlternateScreen::new(options(false));
        assert!(session.enter(&mut terminal));
        clear.set(true);
        let frame = vec![
            "0123456789ABCDEF".to_owned(),
            "second".to_owned(),
            "third".to_owned(),
            "fourth".to_owned(),
        ];
        session.paint(&mut terminal, &frame, 10, 4, None, false);
        let output = writes.borrow().clone();
        assert_eq!(
            output,
            "\x1b[2J\x1b[1;1H\x1b[2K0123456789\x1b[2;1H\x1b[2Ksecond\x1b[3;1H\x1b[2Kthird\x1b[4;1H\x1b[2Kfourth\x1b[?25l",
            "{output:?}"
        );
        assert_eq!(crop_to_width("0123456789ABCDEF", 10), "0123456789");

        // The next frame only repaints changed rows and keeps the window.
        clear.set(true);
        let mut next = frame.clone();
        next[2] = "THIRD".to_owned();
        session.paint(&mut terminal, &next, 10, 4, None, false);
        let output = writes.borrow().clone();
        assert_eq!(output, "\x1b[3;1H\x1b[2KTHIRD\x1b[?25l", "{output:?}");
        assert_eq!(session.window()[2], "THIRD");
    }

    #[test]
    fn paint_uses_the_tail_window_and_places_the_hardware_cursor() {
        let (mut terminal, writes, clear) = recording((10, 2));
        let mut session = AlternateScreen::new(options(false));
        assert!(session.enter(&mut terminal));
        clear.set(true);
        let document = vec![
            "older".to_owned(),
            "middle".to_owned(),
            "newest".to_owned(),
        ];
        session.paint(&mut terminal, &document, 10, 2, Some((1, 3)), true);
        let output = writes.borrow().clone();
        assert!(output.contains("\x1b[1;1H\x1b[2Kmiddle"), "{output:?}");
        assert!(output.contains("\x1b[2;1H\x1b[2Knewest"), "{output:?}");
        assert!(output.contains("\x1b[2;4H\x1b[?25h"), "{output:?}");
    }

    #[test]
    fn exit_restores_the_final_document_to_the_main_screen() {
        let (mut terminal, writes, clear) = recording((10, 4));
        let mut session = AlternateScreen::new(options(false));
        assert!(session.enter(&mut terminal));
        let document = vec![
            "first".to_owned(),
            "0123456789ABCDEF".to_owned(),
            "third".to_owned(),
        ];
        clear.set(true);
        session.exit(&mut terminal, &document, false);
        assert_eq!(
            writes.borrow().as_str(),
            "\x1b[?1049l\x1b[?7l\r\x1b[2Kfirst\r\n\r\x1b[2K0123456789\r\n\r\x1b[2Kthird\x1b[0m\x1b[?7h\r\n\x1b[?25h",
            "{:?}",
            writes.borrow()
        );
        assert!(!session.is_active());
    }

    #[test]
    fn exit_without_the_transcript_only_restores_the_main_screen() {
        let (mut terminal, writes, clear) = recording((10, 4));
        let mut session = AlternateScreen::new(options(false));
        assert!(session.enter(&mut terminal));
        clear.set(true);
        session.exit(&mut terminal, &["ignored".to_owned()], true);
        assert_eq!(writes.borrow().as_str(), "\x1b[?1049l\x1b[?25h");
    }

    #[test]
    fn synchronized_output_wraps_enter_frames_and_exit() {
        let (mut terminal, writes, _) = recording((20, 3));
        let mut session = AlternateScreen::new(options(true));
        assert!(session.enter(&mut terminal));
        assert!(writes.borrow().ends_with("\x1b[?25l"));
        let mut writes_guard = writes.borrow_mut();
        writes_guard.clear();
        drop(writes_guard);
        session.paint(
            &mut terminal,
            &["row".to_owned(), "two".to_owned(), "three".to_owned()],
            20,
            3,
            None,
            false,
        );
        assert!(writes.borrow().starts_with(BEGIN_SYNCHRONIZED_OUTPUT));
        assert!(writes.borrow().ends_with(END_SYNCHRONIZED_OUTPUT));
        let mut writes_guard = writes.borrow_mut();
        writes_guard.clear();
        drop(writes_guard);
        session.exit(&mut terminal, &["done".to_owned()], false);
        let output = writes.borrow().clone();
        assert!(output.starts_with(BEGIN_SYNCHRONIZED_OUTPUT), "{output:?}");
        assert!(output.ends_with(END_SYNCHRONIZED_OUTPUT), "{output:?}");
        assert!(output.contains(ENABLE_AUTOWRAP), "{output:?}");
    }

    #[test]
    fn a_paint_before_enter_is_a_no_op() {
        let (mut terminal, writes, _) = recording((20, 3));
        let mut session = AlternateScreen::new(options(false));
        session.paint(&mut terminal, &["row".to_owned()], 20, 3, None, false);
        assert!(writes.borrow().is_empty());
    }
}
