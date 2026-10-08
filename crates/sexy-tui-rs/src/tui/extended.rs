//! The extended legacy renderer for terminals that cannot be addressed.
//!
//! `render_extended_frame` is the compatibility path taken when the terminal
//! lacks cursor addressing or line clearing, or when Pi's inline scrollback
//! extension is explicitly enabled. It has no notion of addressing an existing
//! row: every update is either a full repaint from home or an append of the
//! rows below the first difference, and a resize discards the presentation and
//! rebuilds it.
//!
//! It is separate from [`super::pi_render`] because its contract is genuinely
//! different, not merely worse: without cursor addressing there is no way to
//! repaint a row in place, so the "smallest changed set" invariant cannot hold
//! and the shrink policy (`clear_on_shrink`) is the only lever left. A full
//! redraw in this path also has to place the hardware cursor itself, which the
//! addressed path never does.

use super::cursor::{extract_cursor_position, extract_cursor_position_from};
use super::frame::frame_change_hints;
use super::kitty::is_image_line;
use super::TUI;

impl<'a> TUI<'a> {
    pub(super) fn render_extended_frame(&mut self) {
        let width = self.terminal.columns();
        let height = self.terminal.rows();
        let size_changed = self
            .previous_size
            .is_some_and(|size| size != (width, height));
        let width_changed = self
            .previous_size
            .is_some_and(|(previous_width, _)| previous_width != width);

        let reset_scrollback_on_resize = size_changed
            && self.inline_scrollback
            && self.capabilities.cursor_addressing
            && self.capabilities.line_clearing;
        let render_commit_cursor = if reset_scrollback_on_resize {
            // The old cursor described history that the resize reset replaces.
            // Ask the component for a fresh boundary in the new layout.
            None
        } else {
            self.inline_commit_cursor
        };
        let lazy_update = (self.capabilities.plain
            || (self.inline_scrollback
                && self.capabilities.cursor_addressing
                && self.capabilities.line_clearing))
            .then(|| self.root_render_update(width, render_commit_cursor))
            .flatten();
        let previous_len = self.previous_frame.len();
        // A prefix beyond the retained frame cannot be validated. Fall back to
        // the component's full renderer rather than pairing its replacement
        // with the wrong historic rows. Width reflow likewise requires a full
        // replacement, but retaining a zero-prefix update preserves pinned
        // viewport metadata across both width and height changes.
        let mut lazy_update = lazy_update.filter(|update| {
            update.stable_prefix <= previous_len && (!width_changed || update.stable_prefix == 0)
        });
        let resize_replay = lazy_update
            .as_mut()
            .and_then(|update| update.resize_replay.take())
            .map(|mut lines| {
                let _ = extract_cursor_position(&mut lines, width, height);
                let mut lines = lines
                    .into_iter()
                    .map(|line| self.prepare_line(line, width))
                    .collect::<Vec<_>>();
                if !self.capabilities.plain {
                    for line in &mut lines {
                        line.push_str("\x1b[0m\x1b]8;;\x1b\\");
                    }
                }
                lines
            });
        let reanchor_viewport = lazy_update
            .as_ref()
            .is_some_and(|update| update.reanchor_viewport);
        let rebuild_scrollback = lazy_update
            .as_ref()
            .is_some_and(|update| update.rebuild_scrollback);
        // Kitty placements only need a full-frame presence check when a
        // destructive inline replay may erase them. Ordinary differential
        // frames inspect just the rows they repaint below.
        let previous_frame_has_image = (reset_scrollback_on_resize || rebuild_scrollback)
            && self.previous_frame.iter().any(|line| is_image_line(line));
        let pinned = lazy_update.as_ref().and_then(|update| update.pinned);
        // Lazy frame assembly reuses `previous_frame` with `mem::take` below.
        // Preserve only the old physical viewport needed by pinned diffing;
        // cloning the complete retained transcript would defeat lazy updates.
        let pinned_previous_window = pinned.map_or_else(Vec::new, |_| {
            let rows = usize::from(height.max(1));
            (0..rows)
                .map(|screen_row| {
                    let logical_row = self.inline_window_top.saturating_add(screen_row);
                    if logical_row < self.inline_history_rows {
                        String::new()
                    } else {
                        self.previous_frame
                            .get(logical_row)
                            .cloned()
                            .unwrap_or_default()
                    }
                })
                .collect()
        });
        let mut first_changed_hint = None;
        let mut lazy_change_hints = None;
        let cursor;

        let new_lines: Vec<String> = if let Some(update) = lazy_update {
            let stable_prefix = update.stable_prefix.min(previous_len);
            let mut replacement = update.replacement;
            let total_len = stable_prefix.saturating_add(replacement.len());
            cursor = extract_cursor_position_from(
                &mut replacement,
                stable_prefix,
                total_len,
                width,
                height,
            );
            let mut replacement = replacement
                .into_iter()
                .map(|line| self.prepare_line(line, width))
                .collect::<Vec<_>>();
            if !self.capabilities.plain {
                for line in &mut replacement {
                    line.push_str("\x1b[0m\x1b]8;;\x1b\\");
                }
            }
            let hints = frame_change_hints(&self.previous_frame, stable_prefix, &replacement);
            first_changed_hint = Some(hints.first_changed);
            lazy_change_hints = Some(hints);

            // Reuse the committed prefix in place. No historic String is
            // cloned and no committed row is compared on an active-run tick.
            let mut reused = std::mem::take(&mut self.previous_frame);
            reused.truncate(stable_prefix);
            reused.extend(replacement);
            reused
        } else {
            let mut rendered = self.root_render(width);
            // Extract the typed cursor marker before clipping or sanitizing the
            // line. It is a trusted library control token, never accepted from
            // semantic text.
            cursor = extract_cursor_position(&mut rendered, width, height);

            // Prepare the children in order. Plain/log mode is escape-free and
            // does not right-pad every row with terminal-width spaces. Inline
            // scrollback also skips padding: every repaint erases before
            // writing, and padded rows would put trailing spaces into native
            // text selection.
            rendered = rendered
                .into_iter()
                .map(|line| self.prepare_line(line, width))
                .collect();

            // Apply per-line resets only in terminal-control mode. Plain/log
            // backends receive escape-free chronological output.
            if !self.capabilities.plain {
                rendered = rendered
                    .into_iter()
                    .map(|line| format!("{}\x1b[0m\x1b]8;;\x1b\\", line))
                    .collect();
            }
            rendered
        };

        // Cursor movement caused by frame writes must not become visible before
        // the final hardware-cursor address. This is especially noticeable as
        // a transient hollow cursor in the terminal's bottom-right cell.
        self.begin_synchronized_output();

        // A terminal reflows the old grid and saved lines before delivering
        // its resize event. Rebuilding octet-owned history below avoids trying to
        // repair terminal-dependent physical rows after that reflow.
        if self.capabilities.plain {
            self.write_plain_changes(&new_lines, first_changed_hint, previous_len);
            self.first_render = false;
        } else if self.inline_scrollback
            && self.capabilities.cursor_addressing
            && self.capabilities.line_clearing
        {
            self.write_inline_changes(
                &new_lines,
                height,
                size_changed,
                reanchor_viewport,
                rebuild_scrollback,
                pinned,
                &pinned_previous_window,
                first_changed_hint,
                previous_len,
                previous_frame_has_image,
                lazy_change_hints.as_ref(),
                resize_replay.as_deref(),
            );
            self.first_render = false;
        } else if self.first_render {
            if self.capabilities.cursor_addressing {
                self.terminal.write("\x1b[H");
            }
            self.write_all_lines(&new_lines);
            self.first_render = false;
        } else if previous_len == 0 {
            self.write_all_lines(&new_lines);
        } else if size_changed {
            self.redraw_all_from_home(&new_lines);
        } else {
            // Strategy 3: update only the changed tail. This handles pure
            // append, replacement, shrink, and empty frames.
            let first_changed = first_changed_hint.unwrap_or_else(|| {
                self.previous_frame
                    .iter()
                    .zip(&new_lines)
                    .position(|(prev, new)| prev != new)
                    .unwrap_or(previous_len.min(new_lines.len()))
            });

            let old_viewport_start = previous_len.saturating_sub(usize::from(height));
            let new_viewport_start = new_lines.len().saturating_sub(usize::from(height));
            let viewport_shifted = old_viewport_start != new_viewport_start;
            if !self.capabilities.cursor_addressing || !self.capabilities.line_clearing {
                // A styled but non-addressable backend behaves like an append-only
                // log: never emit cursor/erase controls it did not advertise.
                self.write_all_lines(&new_lines);
            } else if (first_changed == 0 && previous_len != new_lines.len())
                || viewport_shifted
                || first_changed < new_viewport_start
            {
                self.redraw_all_from_home(&new_lines);
            } else if first_changed < previous_len || first_changed < new_lines.len() {
                self.begin_synchronized_output();
                let screen_row = first_changed.saturating_sub(new_viewport_start);
                self.terminal
                    .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                self.terminal.clear_from_cursor();
                let changed = &new_lines[first_changed..];
                for (index, line) in changed.iter().enumerate() {
                    self.terminal.write(line);
                    // A newline after the terminal's bottom row scrolls the
                    // alternate screen and invalidates every absolute row in
                    // the retained frame. Cursor-addressed updates do not need
                    // a trailing newline after their final row.
                    if index + 1 < changed.len() {
                        self.terminal.write("\n");
                    }
                }
                if new_lines.len() < previous_len {
                    self.terminal.clear_from_cursor();
                }
                self.end_synchronized_output();
            }
        }

        if let Some((row, column)) = cursor.filter(|_| self.capabilities.cursor_addressing) {
            let row = if self.inline_scrollback && !self.capabilities.plain {
                // Re-anchor from the bottom-aligned viewport model to the
                // frame's true on-screen bottom row (a shrink can leave the
                // tail above the screen's last row).
                let viewport_start = new_lines.len().saturating_sub(usize::from(height));
                let logical = usize::from(row) + viewport_start;
                let from_end = new_lines.len().saturating_sub(1).saturating_sub(logical);
                self.inline_bottom_row.saturating_sub(from_end) as u16
            } else {
                row
            };
            self.terminal.write(&format!(
                "\x1b[{};{}H",
                row.saturating_add(1),
                column.saturating_add(1)
            ));
            self.terminal.show_cursor();
        } else if self.capabilities.cursor_addressing {
            self.terminal.hide_cursor();
        }
        self.end_synchronized_output();
        self.previous_frame = new_lines;
        self.previous_size = Some((width, height));
    }

    pub(super) fn write_plain_changes(
        &mut self,
        lines: &[String],
        first_changed_hint: Option<usize>,
        previous_len: usize,
    ) {
        let first_changed = if self.first_render {
            0
        } else {
            first_changed_hint.unwrap_or_else(|| {
                self.previous_frame
                    .iter()
                    .zip(lines)
                    .position(|(previous, next)| previous != next)
                    .unwrap_or(previous_len.min(lines.len()))
            })
        };
        for line in &lines[first_changed..] {
            self.terminal.write(line);
            self.terminal.write("\n");
        }
    }
}
