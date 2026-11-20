//! Scroll-driven selection uses completed frames, never input-side layout.
use super::*;

// Pi compatibility: packages/tui/src/tui-alt-screen.ts:1280-1324 keeps an
// edge drag scrolling every 50 ms. Terminal ownership requires this in Rust.
pub(super) const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);
const COPY_NOTICE_DURATION: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolCardHit {
    index: usize,
    id: u64,
}

impl InteractiveShell {
    fn owns_ansi_mouse(&self) -> bool {
        self.capture_mouse
            && !self
                .state
                .native()
                .lock()
                .expect("native mailbox poisoned")
                .accepting_input
    }

    pub(super) fn begin_tool_card_press(&mut self, row: u16, col: u16) -> bool {
        if !self.owns_ansi_mouse() {
            return false;
        }
        let Some(hit) = self.state.borrow().painted_tool_card(row, col) else {
            return false;
        };
        // Preserve the normal drag/copy anchor, even on a disclosure target.
        self.begin_transcript_selection(row, col, false);
        let mut state = self.state.borrow_mut();
        if state.pending_screen_press.is_none() {
            state.pending_screen_press = Some(ScreenPress {
                cell: (row, col),
                target: ScreenPressTarget::ToolCard(hit),
                moved: false,
                fence: renderer_geometry::GeometryFence::capture(&state),
            });
        }
        true
    }

    pub(super) fn toggle_painted_tool_card(&self, row: u16, col: u16, hit: ToolCardHit) {
        if !self.owns_ansi_mouse() {
            return;
        }
        let mut state = self.state.borrow_mut();
        if state.painted_tool_card(row, col) != Some(hit) {
            return;
        }
        let expanded = !state.show_tool_details(hit.index);
        state.set_block_disclosure(hit.index, expanded);
    }
}

impl ShellState {
    /// Hit the painted header, not the copy projection: an adapter can supply
    /// a non-copyable label, and a pad or body row can map to the same offset.
    fn painted_tool_card(&self, row: u16, col: u16) -> Option<ToolCardHit> {
        if !self.application_viewport_requested
            || self.startup_pending
            || self.panel.is_some()
            || self.overlay.is_some()
            || self.tool_input_prompt.is_some()
            || self.extension_ui.remote_fullscreen_overlay
            || self.transcript_search_active()
        {
            return None;
        }
        let cache;
        let (index, id, local, rows, surface, line) = if self.render_threaded {
            // A disclosure must not acquire authority from stale or merely
            // prepared geometry, even if the same cell is a copyable position.
            let frame = self
                .pointer_render_geometry()
                .filter(|frame| frame.is_current(self))?;
            frame.navigation_frame?;
            let absolute = frame.visible_start + usize::from(row);
            let line = frame.visible_lines.get(usize::from(row))?;
            let block = frame
                .blocks
                .iter()
                .find(|b| (b.start..b.start + b.rows).contains(&absolute))?;
            (
                block.index,
                block.id,
                absolute - block.start,
                block.rows,
                block.surface,
                line,
            )
        } else {
            let absolute = self.painted_history_row(row)?;
            cache = self.transcript_cache.borrow();
            let index = cache.blocks_in_rows(absolute, absolute + 1).next()?;
            (
                index,
                self.transcript_commit_ids[index],
                absolute - cache.block_starts[index],
                cache.block_lengths[index],
                cache.block_geometries[index],
                &cache.lines[absolute],
            )
        };
        if surface.content_row(local, rows) != Some(0)
            || self.transcript_commit_ids.get(index) != Some(&id)
            || !matches!(self.transcript[index], TranscriptBlock::Tool(_))
            || sexy_tui_rs::hyperlink_at_column(line, usize::from(col)).is_some()
        {
            return None;
        }
        let left = usize::from(surface.content_left);
        let header =
            sexy_tui_rs::slice_by_column(line, left, usize::from(surface.content_width), true);
        let plain = strip_terminal_sequences(&header);
        let start = left + visible_width(&plain[..plain.len() - plain.trim_start().len()]);
        let end = left + visible_width(plain.trim_end());
        (start..end)
            .contains(&usize::from(col))
            .then_some(ToolCardHit { index, id })
    }

    /// Displace both the retained range and its pending gesture before another
    /// surface selects. Pi compatibility: packages/tui/src/tui-alt-screen.ts:
    /// 1335-1405 has one screen-level range; component clicks clear it at 1372.
    /// Native terminal ownership keeps this transition in Rust, not the adapter.
    pub(super) fn clear_text_selection(&mut self) {
        self.cancel_transcript_pointer();
        self.transcript_selection = None;
        self.composer_selection = None;
        self.welcome_text_selection = None;
        self.welcome_last_click = None;
    }

    pub(super) fn cancel_transcript_pointer(&mut self) {
        self.pending_selection_anchor = None;
        self.selection_pointer_cell = None;
        self.deferred_pointer_press = None;
        self.pending_screen_press = None;
        self.selection_dragging = false;
        self.selection_scroll_pending = false;
        self.selection_autoscroll_at = None;
    }

    pub(super) fn transcript_pointer_deadline(&self) -> Option<Instant> {
        self.selection_autoscroll_at
            .into_iter()
            .chain(self.copy_notice.as_ref().map(|(_, until)| *until))
            .min()
    }

    pub(super) fn advance_transcript_pointer(&mut self, now: Instant) {
        if self
            .copy_notice
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.copy_notice = None;
        }
        if self.selection_autoscroll_at.is_some_and(|at| now >= at) {
            if let Some((row, col)) = self
                .selection_pointer_cell
                .filter(|_| self.selection_dragging)
            {
                InteractiveShell::extend_transcript_selection_state(self, row, col);
                if self.selection_autoscroll_at.is_some() {
                    self.selection_autoscroll_at = Some(now + AUTOSCROLL_INTERVAL);
                }
            } else {
                self.selection_autoscroll_at = None;
            }
        }
    }

    /// Wheel input changes navigation intent immediately; focus waits for the
    /// corresponding painted viewport. Ordinary stream paints never remap it.
    pub(super) fn resolve_scrolled_transcript_selection(&mut self) -> Option<String> {
        if !self.selection_scroll_pending {
            return None;
        }
        let focus = if self.render_threaded {
            let geometry = self.pointer_render_geometry()?;
            let (painted, pointer) = geometry.pointer_selection.as_ref()?;
            let selection = self.transcript_selection.as_ref()?;
            // A newer drag may have arrived while this frame was writing.
            // Only accept the range actually highlighted for this gesture.
            if Some(*pointer) != self.selection_pointer_cell || painted.anchor != selection.anchor {
                return None;
            }
            let block = geometry
                .blocks
                .iter()
                .find(|block| block.index == painted.focus.block)?;
            if self.transcript_commit_ids.get(block.index) != Some(&block.id) {
                return None;
            }
            painted.focus
        } else {
            let (row, col) = self.selection_pointer_cell?;
            let capacity = transcript_gesture_viewport_capacity(self);
            let row = row.min(capacity.checked_sub(1)? as u16);
            InteractiveShell::transcript_position_at_gesture_cell(self, row, col)?
        };
        self.selection_scroll_pending = false;
        self.transcript_selection.as_mut()?.focus = focus;
        if self.selection_dragging {
            return None;
        }
        // A wheel extension after release also refreshes the clipboard.
        let copy = semantic_selected_text(self)?;
        self.retain_transcript_copy(&copy, Instant::now());
        Some(copy)
    }

    pub(super) fn retain_transcript_copy(&mut self, text: &str, now: Instant) {
        self.copy_buffer = Some(text.to_owned());
        // Pi compatibility: packages/tui/src/tui-alt-screen.ts:1483-1503
        // flashes copy feedback. Keep the existing bounded OSC 52 transport;
        // a local helper still receives the complete selection.
        let message = if clipboard_transport::is_truncated(text) {
            "Copied; terminal copy truncated (49,152-byte limit)"
        } else {
            "Copied selection"
        };
        self.copy_notice = Some((message.to_owned(), now + COPY_NOTICE_DURATION));
    }

    pub(super) fn decorate_copy_notice(
        &self,
        lines: &mut [String],
        transcript_rows: usize,
        width: u16,
        now: Instant,
    ) {
        let Some((text, _)) = self.copy_notice.as_ref().filter(|(_, until)| now < *until) else {
            return;
        };
        // Reuse chrome without moving the transcript or hiding its highlight.
        if let Some(row) = lines
            .iter_mut()
            .skip(transcript_rows)
            .rev()
            .find(|row| !row.contains(sexy_tui_rs::CURSOR_MARKER))
        {
            *row = fit_line(&self.theme.fg("success", text), width);
        }
    }
}
