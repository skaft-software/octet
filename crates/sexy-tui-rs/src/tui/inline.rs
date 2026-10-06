//! The inline-scrollback renderer: one mutable frame on the primary screen.
//!
//! Inline scrollback is the opt-in path that keeps a full-height editable frame
//! on the primary screen while letting rows above it flow into the terminal's
//! own history. It is a third contract alongside the addressed Pi renderer and
//! the legacy extended path, and it is the only one that has to reconcile two
//! authorities over the same rows: the application owns a retained transcript
//! tape, and the terminal owns a history buffer that grows by scrolling.
//!
//! The reconciliation ledger lives in the `inline_*` fields of [`super::TUI`]:
//! how many current-layout rows are already in terminal history, which semantic
//! commit cursor produced them, which generation the tape belongs to, and
//! whether a temporary screen-relative surface currently overlays the bottom.
//! Getting that wrong does not produce a wrong frame — it produces a duplicated
//! or half-committed history, which is why every method here states which
//! invariant it is maintaining.
//!
//! Three update strategies live here, and the choice between them is the
//! interesting part:
//!
//! * [`TUI::write_inline_pinned`] repaints a fixed visible window and lets the
//!   terminal's own reflow keep history, preserving the physical rows.
//! * [`TUI::write_inline_viewport_surface`] and
//!   [`TUI::repaint_inline_visible_rows`] repaint a temporary screen-relative
//!   surface without advancing the tape at all.
//! * [`TUI::reset_inline_scrollback`] gives up on the terminal's presentation
//!   entirely and replays the application tape. This is the fallback for the
//!   bounded cases that cannot be reconstructed from reflow: a Kitty placement,
//!   or a resize that arrived together with a timeline replacement.

use crate::scrollback::reset_and_replay;

use super::frame::FrameChangeHints;
use super::kitty::{delete_all_kitty_images, is_image_line};
use super::PinnedFrame;
use super::TUI;

impl<'a> TUI<'a> {
    /// Differential update against the primary screen. Logical rows above the
    /// visible region are never repainted; rows appended after first paint can
    /// enter native scrollback when a bottom-row newline scrolls naturally.
    /// `inline_bottom_row` anchors all cursor addressing because a frame shrink
    /// leaves the tail above the bottom row (the screen cannot scroll back down).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn write_inline_changes(
        &mut self,
        new_lines: &[String],
        height: u16,
        size_changed: bool,
        reanchor_viewport: bool,
        rebuild_scrollback: bool,
        pinned: Option<PinnedFrame>,
        pinned_previous_window: &[String],
        first_changed_hint: Option<usize>,
        previous_len: usize,
        previous_frame_has_image: bool,
        frame_change_hints: Option<&FrameChangeHints>,
        resize_replay: Option<&[String]>,
    ) {
        let rows = usize::from(height.max(1));
        if size_changed && !self.first_render {
            let displayed_window_top = new_lines.len().saturating_sub(rows);
            let retained_generation = self
                .inline_generation
                .or_else(|| self.inline_commit_cursor.map(|cursor| cursor.generation));
            let generation_continues = pinned.is_none_or(|frame| {
                retained_generation.is_none_or(|generation| generation == frame.generation)
            });
            let preserve_native_history = pinned.filter(|frame| {
                generation_continues
                    && !frame.viewport_surface
                    && frame.stable_rows >= displayed_window_top
                    && resize_replay.is_none()
                    && !rebuild_scrollback
                    && !previous_frame_has_image
                    && !new_lines.iter().any(|line| is_image_line(line))
            });
            if let Some(pinned) = preserve_native_history {
                // The terminal has already reflowed its grid and saved lines.
                // Treat that reflowed prefix as the new physical history seam
                // and repaint only one complete visible grid. Replaying the
                // application tape here duplicates history in multiplexers and
                // makes resize cost proportional to the whole conversation.
                self.inline_history_rows = displayed_window_top;
                self.inline_committed_rows = 0;
                self.inline_commit_cursor = None;
                self.inline_generation = Some(pinned.generation);
                self.inline_window_top = displayed_window_top;
                self.inline_surface_active = false;
                self.inline_surface_window.clear();
                self.write_inline_pinned(
                    new_lines,
                    rows,
                    pinned,
                    true,
                    false,
                    pinned_previous_window,
                );
                return;
            }

            let reanchor_replacement_timeline = pinned.filter(|frame| {
                !generation_continues
                    && !frame.viewport_surface
                    && resize_replay.is_none()
                    && !previous_frame_has_image
                    && !new_lines.iter().any(|line| is_image_line(line))
            });
            if let Some(pinned) = reanchor_replacement_timeline {
                // The terminal may reflow its old saved lines, but they belong
                // to another semantic tape. Preserve that native history while
                // starting the replacement generation at row zero and repainting
                // its live grid; never claim the old off-screen prefix as new
                // history and never clear scrollback merely because resize and
                // timeline replacement arrived in the same frame.
                self.write_inline_pinned(
                    new_lines,
                    rows,
                    pinned,
                    true,
                    rebuild_scrollback,
                    pinned_previous_window,
                );
                return;
            }

            // A temporary surface or Kitty placement cannot be reconstructed
            // safely from terminal reflow alone. Keep the destructive replay
            // fallback for those bounded exceptional paths.
            // Modern terminals reflow both the grid and saved lines before the
            // application observes a resize. Physical-row repair cannot be
            // made terminal-independent, so discard that presentation and
            // replay the complete application-owned tape. A temporary screen
            // surface may provide the unobscured tape, then repaint its visible
            // frame without scrolling any additional rows.
            let displayed_window_top = new_lines.len().saturating_sub(rows);
            let replay = resize_replay.filter(|replay| {
                replay.len().saturating_sub(rows) == displayed_window_top
                    && !replay
                        .iter()
                        .chain(new_lines)
                        .any(|line| is_image_line(line))
            });
            let pinned_surface = pinned.is_some_and(|frame| frame.viewport_surface);
            self.reset_inline_scrollback(
                replay.unwrap_or(new_lines),
                rows,
                previous_frame_has_image,
            );
            self.inline_generation = pinned.map(|frame| frame.generation);
            if replay.is_some() {
                self.terminal.write("\x1b[H");
                let visible = &new_lines[displayed_window_top..];
                for (index, line) in visible.iter().enumerate() {
                    self.terminal.clear_line();
                    self.terminal.write(line);
                    if index + 1 < visible.len() {
                        self.terminal.write("\n");
                    }
                }
                for row in visible.len()..rows {
                    self.terminal
                        .write(&format!("\x1b[{};1H", row.saturating_add(1)));
                    self.terminal.clear_line();
                }
                self.inline_history_rows = displayed_window_top;
                self.inline_window_top = displayed_window_top;
                self.inline_bottom_row = visible.len().saturating_sub(1);
            }
            if pinned_surface {
                let visible = &new_lines[displayed_window_top..];
                self.inline_surface_window = visible.to_vec();
                self.inline_surface_window.resize(rows, String::new());
                self.inline_surface_active = true;
                self.inline_bottom_row = visible.len().saturating_sub(1);
            }
            return;
        }
        if rebuild_scrollback && !self.first_render && pinned.is_none() {
            // Generic inline frames have no semantic commit boundary, so a
            // disclosure rebuild must replace their complete presentation.
            self.reset_inline_scrollback(new_lines, rows, previous_frame_has_image);
            return;
        }
        if let Some(pinned) = pinned {
            self.write_inline_pinned(
                new_lines,
                rows,
                pinned,
                reanchor_viewport || rebuild_scrollback,
                rebuild_scrollback,
                pinned_previous_window,
            );
            return;
        }
        if self.first_render {
            // Push the caller's existing screen content into scrollback
            // instead of erasing it, then paint the visible tail from home.
            // The complete logical frame remains retained for differential
            // updates, but restoring a large session must not synchronously
            // stream megabytes of off-screen history through the PTY before
            // the composer becomes usable.
            self.terminal.write(&"\n".repeat(rows));
            self.terminal.write("\x1b[H");
            self.terminal.clear_screen();
            self.terminal.write("\x1b[H");
            let visible = &new_lines[new_lines.len().saturating_sub(rows)..];
            self.write_all_lines(visible);
            self.inline_bottom_row = visible.len().saturating_sub(1);
            return;
        }

        let prev_len = previous_len;
        // Frame lines currently on screen span [visible_start, prev_len).
        let visible_start = prev_len.saturating_sub(self.inline_bottom_row + 1);
        let removed_history = !reanchor_viewport && new_lines.len() <= visible_start;
        if removed_history {
            // A generic frame has no semantic cursor with which to prove that
            // rows already in native history still belong to the new frame. A
            // shrink that removes that prefix therefore needs destructive
            // reconciliation rather than a tail repaint.
            let has_image = previous_frame_has_image
                || self.previous_frame.iter().any(|line| is_image_line(line))
                || new_lines.iter().any(|line| is_image_line(line));
            self.reset_inline_scrollback(new_lines, rows, has_image);
            return;
        }
        if reanchor_viewport || prev_len == 0 {
            // Reflow or an explicit logical-timeline replacement invalidates
            // every row assumption. Repaint the visible tail from home;
            // replacement timelines intentionally leave the old session's
            // native history reachable.
            self.begin_synchronized_output();
            let erased_has_image = frame_change_hints.map_or_else(
                || {
                    self.previous_frame[visible_start..]
                        .iter()
                        .any(|line| is_image_line(line))
                },
                |hints| hints.affected_tail_has_image,
            );
            if erased_has_image {
                // Erasing text cells does not remove Kitty placements. The
                // complete new visible tail is painted below, so a global
                // delete cannot strand any unchanged on-screen image.
                self.terminal.write(&delete_all_kitty_images());
            }
            self.terminal.write("\x1b[H");
            let start = new_lines.len().saturating_sub(rows);
            let visible = &new_lines[start..];
            // ED 2 is not history-neutral in multiplexers such as tmux: cells
            // erased from the grid are retained as native scrollback. Erase
            // each physical row instead so a transient overlay, resize, or
            // timeline reanchor cannot commit mutable chrome.
            for (index, line) in visible.iter().enumerate() {
                self.terminal.clear_line();
                self.terminal.write(line);
                if index + 1 < visible.len() {
                    self.terminal.write("\n");
                }
            }
            for row in visible.len()..rows {
                self.terminal
                    .write(&format!("\x1b[{};1H", row.saturating_add(1)));
                self.terminal.clear_line();
            }
            self.end_synchronized_output();
            self.inline_bottom_row = visible.len().saturating_sub(1);
            return;
        }

        let first_changed = first_changed_hint.unwrap_or_else(|| {
            self.previous_frame
                .iter()
                .zip(new_lines)
                .position(|(prev, new)| prev != new)
                .unwrap_or(prev_len.min(new_lines.len()))
        });
        if first_changed >= prev_len && new_lines.len() == prev_len {
            return;
        }

        if first_changed < visible_start {
            // Rows already owned by native scrollback cannot be edited. Do not
            // clear and replay the retained timeline here: multiplexers may
            // preserve the old history and append that replay, while terminals
            // without synchronized paint expose it as a full-screen flash.
            // Align the old and new visible tails instead and repaint only the
            // physical rows whose final cells differ. Off-screen history keeps
            // the version that was committed when it originally scrolled out.
            self.repaint_inline_visible_rows(new_lines, rows);
            return;
        }

        let mut delete_images_before_repaint = false;

        // A fixed-height frame can change in the middle when an application
        // replaces elastic viewport padding with a newly arrived event. Repaint
        // only the changed rows in that case: clearing the entire tail would
        // needlessly erase and redraw pinned composer/footer rows and visibly
        // flickers on terminals without synchronized-output support.
        if new_lines.len() == prev_len {
            let fixed_height_hint =
                frame_change_hints.and_then(|hints| hints.fixed_height.as_ref());
            let last_changed = fixed_height_hint.map_or_else(
                || {
                    self.previous_frame
                        .iter()
                        .zip(new_lines)
                        .rposition(|(previous, next)| previous != next)
                },
                |hints| hints.last_changed,
            );
            if let Some(last_changed) = last_changed {
                let repaint_from = first_changed.max(visible_start);
                if repaint_from > last_changed {
                    return;
                }
                let changed_has_image = fixed_height_hint.map_or_else(
                    || {
                        (repaint_from..=last_changed).any(|index| {
                            self.previous_frame[index] != new_lines[index]
                                && (is_image_line(&self.previous_frame[index])
                                    || is_image_line(&new_lines[index]))
                        })
                    },
                    |hints| {
                        hints
                            .image_rows
                            .iter()
                            .any(|row| *row >= repaint_from && *row <= last_changed)
                    },
                );
                if !changed_has_image {
                    self.begin_synchronized_output();
                    if let Some(hints) = fixed_height_hint {
                        for &index in hints
                            .changed_rows
                            .iter()
                            .filter(|row| **row >= repaint_from && **row <= last_changed)
                        {
                            let from_end = prev_len.saturating_sub(1).saturating_sub(index);
                            let screen_row = self.inline_bottom_row.saturating_sub(from_end);
                            self.terminal
                                .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                            self.terminal.clear_line();
                            self.terminal.write(&new_lines[index]);
                        }
                    } else {
                        let mut index = repaint_from;
                        while index <= last_changed {
                            if self.previous_frame[index] != new_lines[index] {
                                let from_end = prev_len.saturating_sub(1).saturating_sub(index);
                                let screen_row = self.inline_bottom_row.saturating_sub(from_end);
                                self.terminal
                                    .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                                self.terminal.clear_line();
                                self.terminal.write(&new_lines[index]);
                            }
                            index = index.saturating_add(1);
                        }
                    }
                    self.end_synchronized_output();
                    return;
                }
                // Text erase controls do not remove Kitty graphics
                // placements. Delete them before the generic tail redraw and
                // repaint the complete visible viewport so unchanged images
                // removed by the global delete are restored as well.
                delete_images_before_repaint = true;
            }
        } else {
            // Length changes clear and rewrite the affected tail. Kitty image
            // placements survive those text controls, and retransmitting an
            // affected image without first deleting it can also leave stacked
            // placements. Repaint the complete visible viewport after a
            // global delete so unchanged visible images are restored too.
            let affected_from = first_changed
                .min(prev_len.saturating_sub(1))
                .max(visible_start);
            delete_images_before_repaint = frame_change_hints.map_or_else(
                || {
                    let affected_old_has_image = self.previous_frame[affected_from..]
                        .iter()
                        .any(|line| is_image_line(line));
                    let affected_new_has_image = new_lines[affected_from.min(new_lines.len())..]
                        .iter()
                        .any(|line| is_image_line(line));
                    affected_old_has_image || affected_new_has_image
                },
                |hints| hints.affected_tail_has_image,
            );
        }

        // Start at or before the last existing line so appends write a
        // newline from the current tail (scrolling as needed) rather than
        // addressing a row past the screen. A change above the visible
        // region cannot be painted (those rows are scrollback); clamp and
        // accept the stale history.
        let repaint_from = if delete_images_before_repaint {
            visible_start
        } else {
            first_changed
                .min(prev_len.saturating_sub(1))
                .max(visible_start)
        };
        let screen_row = self.inline_bottom_row - (prev_len - 1 - repaint_from);
        self.begin_synchronized_output();
        if delete_images_before_repaint {
            self.terminal.write(&delete_all_kitty_images());
        }
        self.terminal
            .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
        self.terminal.clear_from_cursor();
        let changed = &new_lines[repaint_from.min(new_lines.len())..];
        for (index, line) in changed.iter().enumerate() {
            self.terminal.write(line);
            if index + 1 < changed.len() {
                self.terminal.write("\n");
            }
        }
        self.end_synchronized_output();
        self.inline_bottom_row = if changed.is_empty() {
            screen_row.saturating_sub(1)
        } else {
            (screen_row + changed.len() - 1).min(rows - 1)
        };
    }

    /// Append-only native-scrollback renderer for a frame with semantic commit
    /// points. The acknowledged cursor is remapped into the current width, so
    /// no physical row coordinate survives terminal reflow. The mutable grid
    /// always starts at or after the independently tracked physical-history
    /// seam.
    pub(super) fn write_inline_pinned(
        &mut self,
        new_lines: &[String],
        rows: usize,
        pinned: PinnedFrame,
        mut reanchor: bool,
        atomic_presentation_rebuild: bool,
        previous_window: &[String],
    ) {
        let desired_window_top = new_lines.len().saturating_sub(rows);
        let same_generation = self
            .inline_generation
            .or_else(|| self.inline_commit_cursor.map(|cursor| cursor.generation))
            .is_none_or(|generation| generation == pinned.generation);
        if !same_generation {
            // A replacement timeline starts a new append-only tape after the
            // old terminal-owned history. Its row zero is unrelated to the old
            // cursor, but the old history itself remains untouched.
            self.inline_commit_cursor = None;
            self.inline_history_rows = 0;
            self.inline_committed_rows = 0;
            self.inline_surface_active = false;
            self.inline_surface_window.clear();
            reanchor = true;
        }

        let acknowledged = self.inline_commit_cursor.and_then(|cursor| {
            pinned
                .acknowledged
                .filter(|position| position.cursor == cursor)
        });
        let cursor_unmapped = self.inline_commit_cursor.is_some() && acknowledged.is_none();
        debug_assert!(
            !cursor_unmapped,
            "component did not map the retained semantic commit cursor"
        );

        // The semantic cursor can lag a large finalized block while immutable
        // physical rows from that block move into history one at a time.
        let prior_history_rows = self
            .inline_history_rows
            .max(acknowledged.map_or(0, |position| position.row.min(new_lines.len())));

        // Temporary chrome and reports are physical-screen surfaces, not new
        // transcript tape. An ordinary streaming frame can also contract after
        // Markdown reparses. In either case, advancing or repainting before the
        // monotonic history seam would either commit chrome, duplicate history,
        // or punch blank rows into the live grid. Keep the append ledger frozen
        // and repaint the complete visible tail in place. Explicit presentation
        // rebuilds still honor their atomic semantic commit boundary below.
        if pinned.viewport_surface
            || (!atomic_presentation_rebuild && desired_window_top < prior_history_rows)
        {
            self.write_inline_viewport_surface(new_lines, rows, previous_window);
            self.inline_generation = Some(pinned.generation);
            return;
        }
        if self.inline_surface_active {
            reanchor = true;
        }

        let mut commit_row = acknowledged
            .map(|position| position.row.min(new_lines.len()))
            .unwrap_or_else(|| {
                if cursor_unmapped {
                    reanchor = true;
                    self.inline_committed_rows.min(new_lines.len())
                } else {
                    0
                }
            });
        let mut commit_cursor = self.inline_commit_cursor;
        let target = if cursor_unmapped { None } else { pinned.target }.filter(|target| {
            target.cursor.generation == pinned.generation
                && target.row >= commit_row
                && target.row <= desired_window_top
                && commit_cursor.is_none_or(|cursor| target.cursor > cursor)
        });

        // Stable rows may cross the seam incrementally. A semantic target is
        // also safe to stage once its complete boundary is above the live
        // viewport, even when rows inside that block are disclosure-sensitive.
        let append_limit = target.map_or(pinned.stable_rows, |target| {
            pinned.stable_rows.max(target.row)
        });
        let stable_rows = if cursor_unmapped {
            prior_history_rows
        } else {
            append_limit
                .max(acknowledged.map_or(0, |position| position.row))
                .min(desired_window_top)
                .max(prior_history_rows)
        };
        let append_start = prior_history_rows.min(new_lines.len());
        let append_end = stable_rows.min(new_lines.len());
        let appended = &new_lines[append_start..append_end];
        let history_rows = prior_history_rows.max(append_end);

        // Advance semantic identity only after its complete boundary is known
        // to be in physical history. A resize replay may already have put that
        // boundary there without an append in this frame.
        if let Some(target) = target.filter(|target| target.row <= history_rows) {
            commit_row = target.row;
            commit_cursor = Some(target.cursor);
        }

        // Ordinary streaming retreats use the temporary-surface path above.
        // An explicit semantic presentation rebuild may still contract behind
        // its atomic history boundary; those terminal-owned rows stay blank in
        // the live grid rather than being duplicated.
        let window_top = desired_window_top;
        let window_line = |screen_row: usize| {
            let logical_row = window_top.saturating_add(screen_row);
            (logical_row >= history_rows)
                .then(|| new_lines.get(logical_row))
                .flatten()
                .map(String::as_str)
                .unwrap_or("")
        };

        // When the old grid begins exactly at the physical history seam, its
        // stable top rows can enter scrollback with bottom-row newlines. This
        // is the terminal's native append operation: it preserves a reader's
        // scrollback anchor and avoids repainting the whole live grid.
        let can_scroll_naturally = !appended.is_empty()
            && !self.first_render
            && !reanchor
            && self.inline_window_top == prior_history_rows
            && appended.len() <= rows
            && previous_window.get(..appended.len()) == Some(appended);

        self.begin_synchronized_output();
        if self.first_render {
            // Preserve whatever preceded the application, then establish a
            // clean grid without erasing terminal-owned history.
            self.terminal.write(&"\n".repeat(rows));
        }

        if self.first_render || reanchor || (!appended.is_empty() && !can_scroll_naturally) {
            self.terminal.write("\x1b[H");
            if self.first_render {
                self.terminal.clear_screen();
                self.terminal.write("\x1b[H");
            }

            // A reanchor or a previously compressed mutable window may not
            // contain the rows now becoming stable. Stage that semantic chunk
            // above one complete grid so only the staged rows scroll out.
            let paint_len = appended.len().saturating_add(rows);
            for index in 0..paint_len {
                let line = if index < appended.len() {
                    appended[index].as_str()
                } else {
                    window_line(index - appended.len())
                };
                self.terminal.clear_line();
                self.terminal.write(line);
                if index + 1 < paint_len {
                    self.terminal.write("\r\n");
                }
            }
        } else {
            let shifted_rows = if can_scroll_naturally {
                // Address the live grid's bottom row before emitting newlines;
                // cursor placement from the prior differential frame is not a
                // reliable scroll origin.
                self.terminal
                    .write(&format!("\x1b[{rows};1H{}", "\r\n".repeat(appended.len())));
                appended.len()
            } else {
                0
            };

            // Compare against the grid after any natural scroll. Pure appends
            // now repaint only newly exposed bottom rows instead of replaying
            // every physical row at a shifted logical index.
            for screen_row in 0..rows {
                let previous = previous_window
                    .get(screen_row.saturating_add(shifted_rows))
                    .map(String::as_str)
                    .unwrap_or("");
                let next = window_line(screen_row);
                if previous == next {
                    continue;
                }
                self.terminal
                    .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                self.terminal.clear_line();
                self.terminal.write(next);
            }
        }
        self.end_synchronized_output();

        self.inline_history_rows = history_rows;
        self.inline_committed_rows = commit_row;
        self.inline_commit_cursor = commit_cursor;
        self.inline_generation = Some(pinned.generation);
        self.inline_window_top = window_top;
        self.inline_surface_active = false;
        self.inline_surface_window.clear();
        self.inline_bottom_row = new_lines
            .len()
            .saturating_sub(window_top)
            .saturating_sub(1)
            .min(rows.saturating_sub(1));
    }

    pub(super) fn write_inline_viewport_surface(
        &mut self,
        new_lines: &[String],
        rows: usize,
        previous_window: &[String],
    ) {
        let visible = &new_lines[new_lines.len().saturating_sub(rows)..];
        let visible_len = visible.len();
        let mut next_window = visible.to_vec();
        next_window.resize(rows, String::new());

        let previous = if self.inline_surface_active {
            self.inline_surface_window.clone()
        } else {
            previous_window.to_vec()
        };
        let delete_images = previous
            .iter()
            .zip(&next_window)
            .any(|(old, new)| old != new && (is_image_line(old) || is_image_line(new)))
            || previous
                .get(next_window.len()..)
                .is_some_and(|tail| tail.iter().any(|line| is_image_line(line)));
        let repaint_all = self.first_render || !self.inline_surface_active || delete_images;

        self.begin_synchronized_output();
        if self.first_render {
            // Preserve content that preceded the application, then establish a
            // clean primary-screen grid without committing any surface rows.
            self.terminal.write(&"\n".repeat(rows));
            self.terminal.write("\x1b[H");
            self.terminal.clear_screen();
            self.terminal.write("\x1b[H");
        }
        if delete_images {
            self.terminal.write(&delete_all_kitty_images());
        }
        for (screen_row, next) in next_window.iter().enumerate() {
            if !repaint_all && previous.get(screen_row) == Some(next) {
                continue;
            }
            self.terminal
                .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
            self.terminal.clear_line();
            self.terminal.write(next);
        }
        self.end_synchronized_output();

        self.inline_surface_active = true;
        self.inline_surface_window = next_window;
        self.inline_bottom_row = visible_len.saturating_sub(1).min(rows.saturating_sub(1));
    }

    pub(super) fn repaint_inline_visible_rows(&mut self, new_lines: &[String], rows: usize) {
        let visible_rows = (self.inline_bottom_row + 1)
            .min(rows)
            .min(self.previous_frame.len())
            .min(new_lines.len());
        if visible_rows == 0 {
            return;
        }
        let previous_start = self.previous_frame.len() - visible_rows;
        let next_start = new_lines.len() - visible_rows;
        let previous = &self.previous_frame[previous_start..];
        let next = &new_lines[next_start..];
        let delete_images = previous
            .iter()
            .zip(next)
            .any(|(old, new)| old != new && (is_image_line(old) || is_image_line(new)));
        let changed = previous
            .iter()
            .zip(next)
            .enumerate()
            .filter(|(_, (old, new))| delete_images || old != new)
            .map(|(screen_row, (_, new))| (screen_row, new.clone()))
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return;
        }

        self.begin_synchronized_output();
        if delete_images {
            self.terminal.write(&delete_all_kitty_images());
        }
        for (screen_row, new) in changed {
            self.terminal
                .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
            self.terminal.clear_line();
            self.terminal.write(&new);
        }
        self.end_synchronized_output();
    }

    /// Destructively replace terminal-owned history after a resize or an
    /// explicit generic presentation rebuild.
    pub(super) fn reset_inline_scrollback(
        &mut self,
        new_lines: &[String],
        rows: usize,
        previous_frame_has_image: bool,
    ) {
        let window_top = new_lines.len().saturating_sub(rows);
        let delete_images =
            previous_frame_has_image || new_lines.iter().any(|line| is_image_line(line));

        self.begin_synchronized_output();
        reset_and_replay(
            self.terminal.as_mut(),
            delete_images,
            new_lines.iter().map(String::as_str),
        );
        self.end_synchronized_output();

        // The old semantic cursor referred to the discarded presentation.
        // The next frame negotiates a fresh cursor while `inline_history_rows`
        // prevents that acknowledgement from duplicating replayed rows.
        self.inline_history_rows = window_top;
        self.inline_committed_rows = 0;
        self.inline_commit_cursor = None;
        self.inline_generation = None;
        self.inline_window_top = window_top;
        self.inline_surface_active = false;
        self.inline_surface_window.clear();
        self.inline_bottom_row = new_lines
            .len()
            .saturating_sub(window_top)
            .saturating_sub(1)
            .min(rows.saturating_sub(1));
    }
}
