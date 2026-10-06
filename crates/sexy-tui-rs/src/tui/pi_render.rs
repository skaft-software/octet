//! The normative Pi differential renderer.
//!
//! This is a Rust port of Pi TUI's `doRender()` at revision
//! `20be4b18d4c57487f8993d2762bace129f0cf7c6`. It runs on the primary screen
//! for any terminal that can address the cursor, clear lines and is not in
//! `plain` mode, and it is the algorithm design §11 specifies: repaint the
//! smallest set of logical rows whose bytes changed, never scroll to find a
//! row, and keep terminal-owned scrollback for everything that has scrolled off
//! the top.
//!
//! It also owns the alternate-screen variant, which renders the same document
//! with the same code and then hands a fixed-height window to the alternate
//! session to paint. It is separate from the inline-scrollback compatibility
//! path and from the extended legacy path because those two exist only to
//! support terminals that cannot be addressed, and they carry explicitly
//! different contracts (destructive replay, optional full redraw on shrink).
//! Keeping the normative algorithm alone in one file is what makes it auditable
//! against `packages/tui/src/tui.ts`; named upstream cases live in
//! `tests/pi_tui_render.rs`.
//!
//! Kitty placements are handled by [`super::kitty`] and cursor geometry by
//! [`super::cursor`]; this module decides *when* to apply each.

use crate::utils::visible_width;

use super::cursor::{
    extract_logical_cursor_position_from, is_termux_session, pi_cursor_position,
    pi_line_difference, push_cursor_down, push_cursor_up, push_vertical_move, signed_difference,
};
use super::kitty::is_image_line;
use super::TUI;

/// Reset emitted after every normalized row so a styled or hyperlink-carrying
/// row cannot bleed its attributes into the next one.
const PI_LINE_RESET: &str = "\x1b[0m\x1b]8;;\x07";
const PI_MAX_WRITE_BYTES: usize = 1024 * 1024;

// Most component rows are already normalized. Retain their owned allocation
// rather than building a normalization temporary and copying it a second time.
fn normalize_pi_row(line: &mut String) {
    if line.contains('\t') || line.contains('\u{0e33}') || line.contains('\u{0eb3}') {
        // The normalizer, not raw replacement, preserves escape payload bytes.
        *line = crate::utils::normalize_terminal_output(line);
    }
    line.push_str(PI_LINE_RESET);
}

#[cfg(test)]
mod tests;

/// Main-screen image rows bypass text normalization; legacy paths remain Kitty-only.
fn is_pi_image_line(line: &str) -> bool {
    is_image_line(line) || line.contains("\x1b]1337;File=")
}

impl<'a> TUI<'a> {
    /// Paint one fixed-viewport alternate-screen frame.
    ///
    /// The complete document is rendered exactly as the primary-screen Pi
    /// renderer renders it and retained as [`TUI::rendered_frame`], so the
    /// hidden `/debug` frame seam and the exit handoff both see the whole
    /// logical transcript. Only the visible tail window is painted, at absolute
    /// viewport rows; the session diffs it against the previous window, so a
    /// status tick repaints its own row and nothing else.
    pub(super) fn render_alternate_frame(&mut self) {
        let width_u16 = self.terminal.columns().max(1);
        let height_u16 = self.terminal.rows().max(1);
        let height = usize::from(height_u16);
        let update = (!self.first_render && self.previous_size == Some((width_u16, height_u16)))
            .then(|| self.root_render_update_without_cursor(width_u16))
            .flatten()
            .filter(|update| {
                update.stable_prefix <= self.previous_frame.len()
                    && !update.reanchor_viewport
                    && !update.rebuild_scrollback
                    && update.resize_replay.is_none()
            });
        // Retain the complete document for exit/debug, but prepare only the
        // changed tail. Animation must not clone or normalize settled history.
        let (stable_prefix, mut replacement) = update.map_or_else(
            || (0, self.root_render(width_u16)),
            |update| (update.stable_prefix, update.replacement),
        );
        let logical_cursor_position =
            extract_logical_cursor_position_from(&mut replacement, stable_prefix).or_else(|| {
                self.logical_cursor_position
                    .filter(|cursor| cursor.row < stable_prefix)
            });
        for line in &mut replacement {
            if !is_pi_image_line(line) {
                normalize_pi_row(line);
            }
        }
        let mut rendered = std::mem::take(&mut self.previous_frame);
        rendered.truncate(stable_prefix);
        rendered.extend(replacement);
        let window_top = rendered.len().saturating_sub(height);
        let window = rendered[window_top..].to_vec();
        let cursor = logical_cursor_position
            .filter(|cursor| cursor.row >= window_top)
            .map(|cursor| (cursor.row - window_top, cursor.column));
        if let Some(session) = self.alternate_screen_session.as_mut() {
            session.paint(
                &mut *self.terminal,
                &window,
                width_u16,
                height_u16,
                cursor,
                self.show_hardware_cursor,
            );
        }
        self.logical_cursor_position = logical_cursor_position;
        self.cursor_row =
            logical_cursor_position.map_or(rendered.len().saturating_sub(1), |cursor| cursor.row);
        self.hardware_cursor_row = self.cursor_row;
        self.max_lines_rendered = rendered.len();
        self.previous_viewport_top = window_top;
        self.previous_frame = rendered;
        self.previous_size = Some((width_u16, height_u16));
        self.first_render = false;
    }

    /// Rust port of Pi TUI's `doRender()` at revision
    /// `20be4b18d4c57487f8993d2762bace129f0cf7c6`.
    /// Keep this control flow structurally aligned with
    /// `packages/tui/src/tui.ts`; named upstream cases live in
    /// `tests/pi_tui_render.rs`. Saved-history preservation is an explicit
    /// opt-in; callers otherwise retain the upstream clear/replay policy.
    pub(super) fn render_pi_frame(&mut self) {
        let width_u16 = self.terminal.columns();
        let height_u16 = self.terminal.rows().max(1);
        let width = usize::from(width_u16);
        let height = usize::from(height_u16);
        let previous_width = self.previous_size.map_or(0, |size| size.0);
        let previous_height = self.previous_size.map_or(0, |size| size.1);
        let width_changed = previous_width != 0 && previous_width != width_u16;
        let height_changed = previous_height != 0 && previous_height != height_u16;
        let previous_buffer_length = if previous_height > 0 {
            self.previous_viewport_top
                .saturating_add(usize::from(previous_height))
        } else {
            height
        };
        let mut previous_viewport_top = if height_changed {
            previous_buffer_length.saturating_sub(height)
        } else {
            self.previous_viewport_top
        };
        let mut viewport_top = previous_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        let previous_len = self.previous_frame.len();
        let mut lazy_stable_prefix = None;
        let mut lazy_previous_tail = None;
        let mut reanchor_requested = false;
        #[cfg(test)]
        {
            self.last_pi_lazy_inspected_rows = 0;
        }
        let (new_lines, logical_cursor_position) = if !self.first_render
            && !width_changed
            && (!height_changed || self.preserve_scrollback)
        {
            let update = self.root_render_update_without_cursor(width_u16);
            if self.preserve_scrollback {
                // A full-component fallback (including Kitty frames) must not
                // consume and lose a physical reanchor requested by the root.
                reanchor_requested = update
                    .as_ref()
                    .is_some_and(|update| update.reanchor_viewport || update.rebuild_scrollback);
            }
            let update = update.filter(|update| {
                update.stable_prefix <= previous_len
                        // History-preserving repair consumes the component's
                        // reanchor flags instead of rejecting its lazy prefix.
                        && (self.preserve_scrollback
                            || (!update.reanchor_viewport
                                && !update.rebuild_scrollback
                                && update.resize_replay.is_none()))
                        // Kitty row reservations and delete-by-ID semantics are
                        // already covered by the full-component Pi path. Until
                        // a seam-aware metadata algorithm has equivalent
                        // coverage, keep image-bearing frames on that path.
                        && !self.previous_frame_has_kitty
                        && !update.replacement.iter().any(|line| is_image_line(line))
            });
            if let Some(update) = update {
                let stable_prefix = update.stable_prefix;
                let mut replacement = update.replacement;
                let replacement_cursor =
                    extract_logical_cursor_position_from(&mut replacement, stable_prefix);
                let retained_cursor = self
                    .logical_cursor_position
                    .filter(|cursor| cursor.row < stable_prefix);
                let logical_cursor_position = replacement_cursor.or(retained_cursor);
                #[cfg(test)]
                {
                    self.last_pi_lazy_inspected_rows = replacement.len();
                }
                let replacement = replacement
                    .into_iter()
                    .map(|mut line| {
                        if !is_pi_image_line(&line) {
                            normalize_pi_row(&mut line);
                        }
                        line
                    })
                    .collect::<Vec<_>>();
                // Move the old frame's strings into two owned vectors rather
                // than cloning the stable history. The old tail is retained
                // only for bounded comparisons.
                let mut previous = std::mem::take(&mut self.previous_frame);
                let previous_tail = previous.split_off(stable_prefix);
                previous.extend(replacement);
                lazy_stable_prefix = Some(stable_prefix);
                lazy_previous_tail = Some(previous_tail);
                (previous, logical_cursor_position)
            } else {
                let mut rendered = self.root_render(width_u16);
                let logical_cursor_position =
                    extract_logical_cursor_position_from(&mut rendered, 0);
                for line in &mut rendered {
                    if !is_pi_image_line(line) {
                        normalize_pi_row(line);
                    }
                }
                (rendered, logical_cursor_position)
            }
        } else {
            let mut rendered = self.root_render(width_u16);
            let logical_cursor_position = extract_logical_cursor_position_from(&mut rendered, 0);
            for line in &mut rendered {
                if !is_pi_image_line(line) {
                    normalize_pi_row(line);
                }
            }
            (rendered, logical_cursor_position)
        };
        self.logical_cursor_position = logical_cursor_position;
        let cursor_position = pi_cursor_position(logical_cursor_position, new_lines.len(), height);
        // An empty retained document uses Pi's initial paint path, even after
        // a no-op Termux resize. Do not clear terminal-owned saved lines.
        if previous_len == 0 && !width_changed && !height_changed {
            self.pi_full_render(
                new_lines,
                width_u16,
                height_u16,
                false,
                cursor_position,
                lazy_stable_prefix.is_some(),
            );
            return;
        }
        if width_changed || reanchor_requested {
            self.pi_full_render(
                new_lines,
                width_u16,
                height_u16,
                true,
                cursor_position,
                lazy_stable_prefix.is_some(),
            );
            return;
        }
        if height_changed && !is_termux_session() {
            self.pi_full_render(
                new_lines,
                width_u16,
                height_u16,
                true,
                cursor_position,
                lazy_stable_prefix.is_some(),
            );
            return;
        }
        if self.clear_on_shrink && new_lines.len() < self.max_lines_rendered && !self.first_render {
            self.pi_full_render(
                new_lines,
                width_u16,
                height_u16,
                true,
                cursor_position,
                lazy_stable_prefix.is_some(),
            );
            return;
        }

        let mut first_changed = None;
        let mut last_changed = None;
        let max_lines = new_lines.len().max(previous_len);
        // A lazy component has already proven that rows before this boundary
        // are unchanged. Avoid walking every retained historical String just to
        // rediscover that fact on every animation frame.
        let stable_prefix = lazy_stable_prefix.unwrap_or(0);
        let compare_start = stable_prefix.min(max_lines);
        #[cfg(test)]
        if lazy_stable_prefix.is_some() {
            self.last_pi_lazy_inspected_rows = self
                .last_pi_lazy_inspected_rows
                .saturating_add(max_lines.saturating_sub(compare_start));
        }
        for index in compare_start..max_lines {
            let old_line = if let Some(previous_tail) = lazy_previous_tail.as_deref() {
                previous_tail
                    .get(index.saturating_sub(stable_prefix))
                    .map_or("", String::as_str)
            } else {
                self.previous_frame.get(index).map_or("", String::as_str)
            };
            let new_line = new_lines.get(index).map_or("", String::as_str);
            if old_line != new_line {
                first_changed.get_or_insert(index);
                last_changed = Some(index);
            }
        }
        let appended_lines = new_lines.len() > previous_len;
        if appended_lines {
            first_changed.get_or_insert(previous_len);
            last_changed = new_lines.len().checked_sub(1);
        }
        if let (Some(first), Some(last)) = (first_changed, last_changed) {
            let (expanded_first, expanded_last) = if lazy_stable_prefix.is_some() {
                (first, last)
            } else if let Some(previous_tail) = lazy_previous_tail.as_deref() {
                self.pi_expand_changed_range_for_kitty_images(
                    first,
                    last,
                    &new_lines,
                    previous_tail,
                    stable_prefix,
                )
            } else {
                self.pi_expand_changed_range_for_kitty_images(
                    first,
                    last,
                    &new_lines,
                    &self.previous_frame,
                    0,
                )
            };
            first_changed = Some(expanded_first);
            last_changed = Some(expanded_last);
        }
        let append_start = appended_lines
            && first_changed == Some(previous_len)
            && first_changed.is_some_and(|index| index > 0);

        if first_changed.is_none() {
            self.pi_position_hardware_cursor(cursor_position, new_lines.len());
            self.pi_record_kitty_state(&new_lines, lazy_stable_prefix.is_some());
            self.previous_frame = new_lines;
            self.previous_viewport_top = previous_viewport_top;
            self.previous_size = Some((width_u16, height_u16));
            self.first_render = false;
            return;
        }
        let first_changed = first_changed.expect("checked above");
        let last_changed = last_changed.expect("a changed frame has a last row");

        // All changes are deleted rows. Clear those cells without scrolling
        // unless the target moved above the old viewport, where Pi rebuilds.
        if first_changed >= new_lines.len() {
            if previous_len > new_lines.len() {
                let target_row = new_lines.len().saturating_sub(1);
                if target_row < previous_viewport_top {
                    self.pi_full_render(
                        new_lines,
                        width_u16,
                        height_u16,
                        true,
                        cursor_position,
                        lazy_stable_prefix.is_some(),
                    );
                    return;
                }
                let extra_lines = previous_len.saturating_sub(new_lines.len());
                if extra_lines > height {
                    self.pi_full_render(
                        new_lines,
                        width_u16,
                        height_u16,
                        true,
                        cursor_position,
                        lazy_stable_prefix.is_some(),
                    );
                    return;
                }

                let mut buffer = String::from("\x1b[?2026h");
                let deleted_images = if lazy_stable_prefix.is_some() {
                    String::new()
                } else {
                    let previous_tail = lazy_previous_tail.as_deref();
                    previous_tail.map_or_else(
                        || {
                            self.pi_delete_changed_kitty_images(
                                first_changed,
                                last_changed,
                                &self.previous_frame,
                                0,
                            )
                        },
                        |tail| {
                            self.pi_delete_changed_kitty_images(
                                first_changed,
                                last_changed,
                                tail,
                                stable_prefix,
                            )
                        },
                    )
                };
                buffer.push_str(&deleted_images);
                push_vertical_move(
                    &mut buffer,
                    pi_line_difference(
                        hardware_cursor_row,
                        previous_viewport_top,
                        target_row,
                        viewport_top,
                    ),
                );
                buffer.push('\r');
                let clear_start_offset = usize::from(!new_lines.is_empty());
                if extra_lines > 0 && clear_start_offset > 0 {
                    push_cursor_down(&mut buffer, clear_start_offset);
                }
                for index in 0..extra_lines {
                    buffer.push_str("\r\x1b[2K");
                    if index + 1 < extra_lines {
                        push_cursor_down(&mut buffer, 1);
                    }
                }
                let move_back = extra_lines
                    .saturating_sub(1)
                    .saturating_add(clear_start_offset);
                if move_back > 0 {
                    push_cursor_up(&mut buffer, move_back);
                }
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
                self.pi_append_hardware_cursor(&mut buffer, cursor_position, new_lines.len());
                buffer.push_str("\x1b[?2026l");
                self.pi_write_frame(&buffer);
            }
            self.pi_record_kitty_state(&new_lines, lazy_stable_prefix.is_some());
            self.previous_frame = new_lines;
            self.previous_size = Some((width_u16, height_u16));
            self.previous_viewport_top = previous_viewport_top;
            self.first_render = false;
            return;
        }

        if first_changed < previous_viewport_top {
            self.pi_full_render(
                new_lines,
                width_u16,
                height_u16,
                true,
                cursor_position,
                lazy_stable_prefix.is_some(),
            );
            return;
        }

        let mut buffer = String::from("\x1b[?2026h");
        let deleted_images = if lazy_stable_prefix.is_some() {
            String::new()
        } else {
            lazy_previous_tail.as_deref().map_or_else(
                || {
                    self.pi_delete_changed_kitty_images(
                        first_changed,
                        last_changed,
                        &self.previous_frame,
                        0,
                    )
                },
                |tail| {
                    self.pi_delete_changed_kitty_images(
                        first_changed,
                        last_changed,
                        tail,
                        stable_prefix,
                    )
                },
            )
        };
        buffer.push_str(&deleted_images);
        let previous_viewport_bottom = previous_viewport_top.saturating_add(height - 1);
        let move_target_row = if append_start {
            first_changed.saturating_sub(1)
        } else {
            first_changed
        };
        if move_target_row > previous_viewport_bottom {
            let current_screen_row = hardware_cursor_row
                .saturating_sub(previous_viewport_top)
                .min(height - 1);
            let move_to_bottom = height.saturating_sub(1).saturating_sub(current_screen_row);
            if move_to_bottom > 0 {
                push_cursor_down(&mut buffer, move_to_bottom);
            }
            let scroll = move_target_row.saturating_sub(previous_viewport_bottom);
            buffer.push_str(&"\r\n".repeat(scroll));
            previous_viewport_top = previous_viewport_top.saturating_add(scroll);
            viewport_top = viewport_top.saturating_add(scroll);
            hardware_cursor_row = move_target_row;
        }

        push_vertical_move(
            &mut buffer,
            pi_line_difference(
                hardware_cursor_row,
                previous_viewport_top,
                move_target_row,
                viewport_top,
            ),
        );
        buffer.push_str(if append_start { "\r\n" } else { "\r" });

        let render_end = last_changed.min(new_lines.len().saturating_sub(1));
        let mut index = first_changed;
        while index <= render_end {
            if index > first_changed {
                buffer.push_str("\r\n");
            }
            let line = &new_lines[index];
            let image = is_pi_image_line(line);
            let image_reserved_rows = if image {
                self.pi_kitty_image_reserved_rows(&new_lines, index, render_end)
            } else {
                1
            };
            if image_reserved_rows > 1 {
                let image_start_screen_row = index.checked_sub(viewport_top);
                if image_start_screen_row
                    .is_none_or(|row| row.saturating_add(image_reserved_rows) > height)
                {
                    self.pi_full_render(
                        new_lines,
                        width_u16,
                        height_u16,
                        true,
                        cursor_position,
                        lazy_stable_prefix.is_some(),
                    );
                    return;
                }
                buffer.push_str("\x1b[2K");
                for _ in 1..image_reserved_rows {
                    buffer.push_str("\r\n\x1b[2K");
                }
                push_cursor_up(&mut buffer, image_reserved_rows - 1);
                buffer.push_str(line);
                push_cursor_down(&mut buffer, image_reserved_rows - 1);
                index = index.saturating_add(image_reserved_rows);
                continue;
            }

            buffer.push_str("\x1b[2K");
            if !image && visible_width(line) > width {
                self.stop();
                panic!(
                    "rendered line {index} exceeds terminal width ({} > {width}); components must wrap or truncate to the supplied width",
                    visible_width(line)
                );
            }
            buffer.push_str(line);
            index = index.saturating_add(1);
        }

        let mut final_cursor_row = render_end;
        if previous_len > new_lines.len() {
            if render_end < new_lines.len().saturating_sub(1) {
                let move_down = new_lines.len() - 1 - render_end;
                push_cursor_down(&mut buffer, move_down);
                final_cursor_row = new_lines.len() - 1;
            }
            let extra_lines = previous_len.saturating_sub(new_lines.len());
            for _ in new_lines.len()..previous_len {
                buffer.push_str("\r\n\x1b[2K");
            }
            push_cursor_up(&mut buffer, extra_lines);
        }
        self.cursor_row = new_lines.len().saturating_sub(1);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(new_lines.len());
        self.previous_viewport_top =
            previous_viewport_top.max(final_cursor_row.saturating_sub(height.saturating_sub(1)));
        self.pi_append_hardware_cursor(&mut buffer, cursor_position, new_lines.len());
        buffer.push_str("\x1b[?2026l");
        self.pi_write_frame(&buffer);
        self.pi_record_kitty_state(&new_lines, lazy_stable_prefix.is_some());
        self.previous_frame = new_lines;
        self.previous_size = Some((width_u16, height_u16));
        self.first_render = false;
    }

    /// Bound each write without changing the frame stream or its sync transaction.
    /// The frame is still assembled in memory; this is not a total memory bound.
    fn pi_write_frame(&mut self, mut buffer: &str) {
        while !buffer.is_empty() {
            let mut end = buffer.len().min(PI_MAX_WRITE_BYTES);
            while !buffer.is_char_boundary(end) {
                end -= 1;
            }
            self.terminal.write(&buffer[..end]);
            buffer = &buffer[end..];
        }
    }

    pub(super) fn pi_full_render(
        &mut self,
        new_lines: Vec<String>,
        width: u16,
        height: u16,
        clear: bool,
        cursor_position: Option<(usize, usize)>,
        known_image_free: bool,
    ) {
        if clear && self.preserve_scrollback {
            self.pi_reanchor_visible(new_lines, width, height, cursor_position, known_image_free);
            return;
        }
        self.full_redraw_count = self.full_redraw_count.saturating_add(1);
        let height_rows = usize::from(height.max(1));
        let mut buffer = String::from("\x1b[?2026h");
        if clear {
            buffer.push_str(&self.pi_delete_kitty_images(&self.previous_kitty_image_ids));
            buffer.push_str("\x1b[2J\x1b[H\x1b[3J");
        }
        let mut index = 0;
        while index < new_lines.len() {
            if index > 0 {
                buffer.push_str("\r\n");
            }
            let line = &new_lines[index];
            let image_reserved_rows = if is_image_line(line) {
                self.pi_kitty_image_reserved_rows(
                    &new_lines,
                    index,
                    new_lines.len().saturating_sub(1),
                )
            } else {
                1
            };
            if image_reserved_rows > 1 && image_reserved_rows <= height_rows {
                buffer.push_str(&"\r\n".repeat(image_reserved_rows - 1));
                push_cursor_up(&mut buffer, image_reserved_rows - 1);
                buffer.push_str(line);
                push_cursor_down(&mut buffer, image_reserved_rows - 1);
                index = index.saturating_add(image_reserved_rows);
                continue;
            }
            buffer.push_str(line);
            index = index.saturating_add(1);
        }
        self.cursor_row = new_lines.len().saturating_sub(1);
        self.hardware_cursor_row = self.cursor_row;
        self.max_lines_rendered = if clear {
            new_lines.len()
        } else {
            self.max_lines_rendered.max(new_lines.len())
        };
        let buffer_length = height_rows.max(new_lines.len());
        self.previous_viewport_top = buffer_length.saturating_sub(height_rows);
        self.pi_append_hardware_cursor(&mut buffer, cursor_position, new_lines.len());
        buffer.push_str("\x1b[?2026l");
        self.pi_write_frame(&buffer);
        self.pi_record_kitty_state(&new_lines, false);
        self.previous_frame = new_lines;
        self.previous_size = Some((width, height));
        self.first_render = false;
    }

    /// Repair only the addressable grid. Neither ED 2 (which some multiplexers
    /// save as history) nor ED 3 is history-neutral. Absolute row erases also
    /// prevent a shrink or ownership transition from scrolling mutable chrome.
    fn pi_reanchor_visible(
        &mut self,
        new_lines: Vec<String>,
        width: u16,
        height: u16,
        cursor_position: Option<(usize, usize)>,
        known_image_free: bool,
    ) {
        let rows = usize::from(height.max(1));
        let top = new_lines.len().saturating_sub(rows);
        let mut buffer = String::from("\x1b[?2026h");
        if self.previous_frame_has_kitty {
            let old_top = self.previous_viewport_top.min(self.previous_frame.len());
            let ids = Self::pi_collect_kitty_image_ids(&self.previous_frame[old_top..]);
            buffer.push_str(&self.pi_delete_kitty_images(&ids));
        }
        for row in 0..rows {
            buffer.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
        }
        for (row, line) in new_lines[top..].iter().enumerate() {
            if !is_pi_image_line(line) && visible_width(line) > usize::from(width) {
                self.stop();
                panic!("rendered line exceeds terminal width; components must wrap or truncate");
            }
            buffer.push_str(&format!("\x1b[{};1H", row + 1));
            buffer.push_str(line);
        }
        // Establish a known physical origin even for an empty/short document
        // or an image command that moved the terminal cursor.
        buffer.push_str(&format!("\x1b[{rows};1H"));
        self.cursor_row = new_lines.len().saturating_sub(1);
        self.hardware_cursor_row = top + rows - 1;
        self.max_lines_rendered = new_lines.len();
        self.previous_viewport_top = top;
        self.pi_append_hardware_cursor(&mut buffer, cursor_position, new_lines.len());
        buffer.push_str("\x1b[?2026l");
        self.pi_write_frame(&buffer);
        self.pi_record_kitty_state(&new_lines, known_image_free);
        self.previous_frame = new_lines;
        self.previous_size = Some((width, height));
        self.first_render = false;
    }

    pub(super) fn pi_position_hardware_cursor(
        &mut self,
        cursor_position: Option<(usize, usize)>,
        total_lines: usize,
    ) {
        let mut buffer = String::new();
        self.pi_append_hardware_cursor(&mut buffer, cursor_position, total_lines);
        self.pi_write_frame(&buffer);
    }

    /// Finish cursor geometry and visibility in the caller's frame transaction.
    fn pi_append_hardware_cursor(
        &mut self,
        buffer: &mut String,
        cursor_position: Option<(usize, usize)>,
        total_lines: usize,
    ) {
        let Some((row, column)) = cursor_position.filter(|_| total_lines > 0) else {
            buffer.push_str("\x1b[?25l");
            return;
        };
        let target_row = row.min(total_lines.saturating_sub(1));
        push_vertical_move(
            buffer,
            signed_difference(target_row, self.hardware_cursor_row),
        );
        buffer.push_str(&format!("\x1b[{}G", column.saturating_add(1)));
        self.hardware_cursor_row = target_row;
        buffer.push_str(if self.show_hardware_cursor {
            "\x1b[?25h"
        } else {
            "\x1b[?25l"
        });
    }
}
