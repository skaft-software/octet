//! Scroll-driven selection uses completed frames, never input-side layout.
use super::*;

// Pi compatibility: packages/tui/src/tui-alt-screen.ts:1280-1324 keeps an
// edge drag scrolling every 50 ms. Terminal ownership requires this in Rust.
pub(super) const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);
const COPY_NOTICE_DURATION: Duration = Duration::from_millis(1500);

impl ShellState {
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
