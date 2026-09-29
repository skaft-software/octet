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
                    column.min(usize::from(width)) + 1
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
mod tests;
