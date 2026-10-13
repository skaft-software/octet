//! The non-differential write primitives every renderer falls back to.
//!
//! Three renderers exist in this crate and all three need the same two escape
//! hatches: give up on the current presentation and put the whole document on
//! the screen again, or open and close one synchronized-output transaction so a
//! frame becomes visible atomically. Those primitives are here rather than in
//! any one renderer because they are defined by the terminal, not by a strategy:
//! if the addressed path and the inline path each owned their own "replay
//! everything" loop, a resize in one and a timeline replacement in the other
//! would produce subtly different screens for the same document.
//!
//! Synchronized output is reference-counted rather than a simple on/off so
//! nested render helpers share a single transaction — cursor placement must not
//! become visible on its own, in the gap between a frame and its cursor.

use super::kitty::{delete_all_kitty_images, is_image_line};
use super::TUI;

impl<'a> TUI<'a> {
    pub(super) fn begin_synchronized_output(&mut self) {
        if !self.capabilities.synchronized_output {
            return;
        }
        if self.synchronized_output_depth == 0 {
            self.terminal.write("\x1b[?2026h");
        }
        self.synchronized_output_depth = self.synchronized_output_depth.saturating_add(1);
    }

    pub(super) fn end_synchronized_output(&mut self) {
        if !self.capabilities.synchronized_output || self.synchronized_output_depth == 0 {
            return;
        }
        self.synchronized_output_depth -= 1;
        if self.synchronized_output_depth == 0 {
            self.terminal.write("\x1b[?2026l");
        }
    }

    pub(super) fn redraw_all_from_home(&mut self, lines: &[String]) {
        // `Clear(All)` does not universally home the cursor. Do both before
        // repainting so resize and line-count redraws cannot append a frame.
        if self.capabilities.cursor_addressing {
            self.terminal.write("\x1b[H");
        }
        self.terminal.clear_screen();
        if self.capabilities.cursor_addressing {
            self.terminal.write("\x1b[H");
        }
        self.write_all_lines(lines);
    }

    pub(super) fn write_all_lines(&mut self, lines: &[String]) {
        self.begin_synchronized_output();
        if !self.first_render && self.previous_frame.iter().any(|line| is_image_line(line)) {
            self.terminal.write(&delete_all_kitty_images());
        }
        for (index, line) in lines.iter().enumerate() {
            self.terminal.write(line);
            // Keep append-only/non-addressable terminals line-delimited. An
            // addressable retained frame deliberately leaves its cursor on the
            // last row so a full-height frame cannot scroll by one line.
            if index + 1 < lines.len() || !self.capabilities.cursor_addressing {
                self.terminal.write("\n");
            }
        }
        self.end_synchronized_output();
    }
}
