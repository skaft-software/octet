use std::cell::Ref;
use std::time::Instant;

use super::transcript_render::{
    render_assistant_update_planned, render_block_planned_with_rainbow,
};
use super::welcome_card::render_welcome_card;
use super::ShellState;

/// Final block-local geometry shared by transcript rendering and semantic
/// selection. Decorative rows and columns never enter copy offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SurfaceGeometry {
    pub(super) transition_rows: usize,
    pub(super) leading_rows: usize,
    pub(super) trailing_rows: usize,
    pub(super) content_left: u16,
    pub(super) content_width: u16,
}

impl SurfaceGeometry {
    pub(super) fn content_row(self, local_row: usize, total_rows: usize) -> Option<usize> {
        let start = self.transition_rows.checked_add(self.leading_rows)?;
        let end = total_rows.checked_sub(self.trailing_rows)?;
        (local_row >= start && local_row < end).then(|| local_row - start)
    }

    pub(super) fn content_col(self, column: u16) -> u16 {
        column
            .saturating_sub(self.content_left)
            .min(self.content_width)
    }
}

#[derive(Clone, Debug)]
pub(super) struct RenderedTranscriptBlock {
    pub(super) lines: Vec<String>,
    pub(super) geometry: SurfaceGeometry,
}

#[derive(Clone, Debug)]
pub(super) struct TranscriptCache {
    pub(super) width: Option<u16>,
    pub(super) lines: Vec<String>,
    /// Whether the cached welcome prefix was rendered while an overlay was
    /// active. Overlays suppress that prefix without changing transcript
    /// blocks, so this is part of cache staleness rather than a block revision.
    pub(super) welcome_overlay_active: bool,
    pub(super) block_starts: Vec<usize>,
    pub(super) block_lengths: Vec<usize>,
    pub(super) block_geometries: Vec<SurfaceGeometry>,
    pub(super) block_revisions: Vec<u64>,
    /// Blocks changed since the last layout pass. Keeping this explicit avoids
    /// scanning every historic block for each streamed token.
    pub(super) dirty_blocks: Vec<usize>,
    pub(super) dirty: bool,
    pub(super) generation: u64,
    /// First visual row changed by the most recent layout update.
    pub(super) last_update_start: usize,
}

impl TranscriptCache {
    /// Drop one tail block without invalidating the complete historic layout.
    /// A rendered transient status contributes cached rows to truncate; an
    /// event burst may remove the same status before its first frame, in which
    /// case the existing shorter cache is already a valid prefix. Rebuilding
    /// every prior Markdown block in either path stalls animation and input on
    /// long sessions.
    pub(super) fn truncate_tail_block(&mut self, index: usize, block_count: usize) -> bool {
        if index.checked_add(1) != Some(block_count) || self.width.is_none() {
            return false;
        }
        let cached_blocks = self.block_revisions.len();
        let metadata_aligned = self.block_starts.len() == cached_blocks
            && self.block_lengths.len() == cached_blocks
            && self.block_geometries.len() == cached_blocks;
        if !metadata_aligned || cached_blocks > block_count {
            return false;
        }

        if cached_blocks == block_count {
            let Some(start) = self.block_starts.get(index).copied() else {
                return false;
            };
            if start > self.lines.len() {
                return false;
            }
            self.lines.truncate(start);
            self.block_starts.truncate(index);
            self.block_lengths.truncate(index);
            self.block_geometries.truncate(index);
            self.block_revisions.truncate(index);
        }
        self.dirty_blocks.retain(|dirty| *dirty < index);
        self.dirty = true;
        true
    }
}

impl Default for TranscriptCache {
    fn default() -> Self {
        Self {
            width: None,
            lines: Vec::new(),
            welcome_overlay_active: false,
            block_starts: Vec::new(),
            block_lengths: Vec::new(),
            block_geometries: Vec::new(),
            block_revisions: Vec::new(),
            dirty_blocks: Vec::new(),
            dirty: true,
            generation: 0,
            last_update_start: 0,
        }
    }
}

fn replace_welcome_prefix(
    cache: &mut TranscriptCache,
    welcome: Vec<String>,
    overlay_active: bool,
    first_changed: &mut usize,
) {
    let old_length = cache
        .block_starts
        .first()
        .copied()
        .unwrap_or(cache.lines.len())
        .min(cache.lines.len());
    let changed = cache.welcome_overlay_active != overlay_active
        || cache.lines.get(..old_length) != Some(welcome.as_slice());
    if changed {
        *first_changed = 0;
        let new_length = welcome.len();
        cache.lines.splice(0..old_length, welcome);
        let delta = new_length as isize - old_length as isize;
        if delta > 0 {
            for start in &mut cache.block_starts {
                *start += delta as usize;
            }
        } else if delta < 0 {
            for start in &mut cache.block_starts {
                *start = start.saturating_sub((-delta) as usize);
            }
        }
    }
    cache.welcome_overlay_active = overlay_active;
}

impl ShellState {
    pub(super) fn rendered_transcript(&self, width: u16) -> Ref<'_, Vec<String>> {
        let stale = {
            let cache = self.transcript_cache.borrow();
            cache.dirty
                || cache.width != Some(width)
                || cache.welcome_overlay_active != self.overlay.is_some()
        };
        if stale {
            let mut rich_renderer_slot = self.rich_renderer.borrow_mut();
            if rich_renderer_slot.is_none() {
                *rich_renderer_slot = Some(self.theme.rich_renderer());
            }
            let rich_renderer = rich_renderer_slot
                .as_ref()
                .expect("rich renderer initialized above");
            let mut reasoning_renderer_slot = self.reasoning_renderer.borrow_mut();
            if reasoning_renderer_slot.is_none() {
                *reasoning_renderer_slot = Some(self.theme.reasoning_renderer());
            }
            let reasoning_renderer = reasoning_renderer_slot
                .as_ref()
                .expect("reasoning renderer initialized above");
            let mut cache = self.transcript_cache.borrow_mut();
            let previous_line_count = cache.lines.len();
            let rainbow_strength = self.status_rainbow_strength();
            let overlay_active = self.overlay.is_some();
            let mut first_changed = cache.lines.len();
            let rebuild =
                cache.width != Some(width) || cache.block_revisions.len() > self.transcript.len();

            if rebuild {
                first_changed = 0;
                cache.lines.clear();
                cache.block_starts.clear();
                cache.block_lengths.clear();
                cache.block_geometries.clear();
                cache.block_revisions.clear();
                cache.dirty_blocks.clear();
                cache.width = Some(width);
                cache.welcome_overlay_active = overlay_active;
                cache
                    .lines
                    .extend(render_welcome_card(self, width, 10, Instant::now()));

                for (index, block) in self.transcript.iter().enumerate() {
                    let rendered = render_block_planned_with_rainbow(
                        index
                            .checked_sub(1)
                            .and_then(|previous| self.transcript.get(previous)),
                        block,
                        &self.theme,
                        rich_renderer,
                        reasoning_renderer,
                        width,
                        self.show_tool_details(block),
                        self.event_spinner_frame,
                        self.status_shimmer_frame,
                        rainbow_strength,
                    );
                    let start = cache.lines.len();
                    let length = rendered.lines.len();
                    cache.lines.extend(rendered.lines);
                    cache.block_starts.push(start);
                    cache.block_lengths.push(length);
                    cache.block_geometries.push(rendered.geometry);
                    cache.block_revisions.push(self.block_revisions[index]);
                }
            } else {
                // The startup card is a bounded prefix, not a transcript
                // block. Refresh it independently so a 2.2 s animation or an
                // overlay transition never reparses historical Markdown.
                replace_welcome_prefix(
                    &mut cache,
                    render_welcome_card(self, width, 10, Instant::now()),
                    overlay_active,
                    &mut first_changed,
                );

                // New blocks are appended in normal operation. Render them
                // once and leave every existing block's layout untouched.
                while cache.block_revisions.len() < self.transcript.len() {
                    let index = cache.block_revisions.len();
                    let rendered = render_block_planned_with_rainbow(
                        index
                            .checked_sub(1)
                            .and_then(|previous| self.transcript.get(previous)),
                        &self.transcript[index],
                        &self.theme,
                        rich_renderer,
                        reasoning_renderer,
                        width,
                        self.show_tool_details(&self.transcript[index]),
                        self.event_spinner_frame,
                        self.status_shimmer_frame,
                        rainbow_strength,
                    );
                    let start = cache.lines.len();
                    first_changed = first_changed.min(start);
                    let length = rendered.lines.len();
                    cache.lines.extend(rendered.lines);
                    cache.block_starts.push(start);
                    cache.block_lengths.push(length);
                    cache.block_geometries.push(rendered.geometry);
                    cache.block_revisions.push(self.block_revisions[index]);
                }

                // `touch_block` records mutations as they happen. In
                // particular, a token delta normally changes only the active
                // tail block; iterating `0..transcript.len()` here used to make
                // every streaming frame progressively slower as history grew.
                let mut dirty_blocks = std::mem::take(&mut cache.dirty_blocks);
                dirty_blocks.sort_unstable();
                dirty_blocks.dedup();
                for index in dirty_blocks {
                    // A newly appended block is rendered above with its latest
                    // revision. A stale queued index can therefore be skipped.
                    if index >= cache.block_revisions.len()
                        || cache.block_revisions[index] == self.block_revisions[index]
                    {
                        continue;
                    }
                    let start = cache.block_starts[index];
                    let old_length = cache.block_lengths[index];
                    let previous = index
                        .checked_sub(1)
                        .and_then(|previous| self.transcript.get(previous));
                    if let Some(update) = render_assistant_update_planned(
                        previous,
                        &self.transcript[index],
                        &self.theme,
                        rich_renderer,
                        width,
                    )
                    .filter(|update| update.stable_rows <= old_length)
                    {
                        let replacement_start = start.saturating_add(update.stable_rows);
                        first_changed = first_changed.min(replacement_start);
                        let new_length =
                            update.stable_rows.saturating_add(update.replacement.len());
                        cache
                            .lines
                            .splice(replacement_start..start + old_length, update.replacement);
                        cache.block_lengths[index] = new_length;
                        cache.block_geometries[index] = update.geometry;
                        cache.block_revisions[index] = self.block_revisions[index];

                        let delta = new_length as isize - old_length as isize;
                        if delta != 0 {
                            for following in cache.block_starts.iter_mut().skip(index + 1) {
                                if delta > 0 {
                                    *following += delta as usize;
                                } else {
                                    *following = following.saturating_sub((-delta) as usize);
                                }
                            }
                        }
                        continue;
                    }

                    first_changed = first_changed.min(start);
                    let rendered = render_block_planned_with_rainbow(
                        previous,
                        &self.transcript[index],
                        &self.theme,
                        rich_renderer,
                        reasoning_renderer,
                        width,
                        self.show_tool_details(&self.transcript[index]),
                        self.event_spinner_frame,
                        self.status_shimmer_frame,
                        rainbow_strength,
                    );
                    let new_length = rendered.lines.len();
                    cache
                        .lines
                        .splice(start..start + old_length, rendered.lines);
                    cache.block_lengths[index] = new_length;
                    cache.block_geometries[index] = rendered.geometry;
                    cache.block_revisions[index] = self.block_revisions[index];

                    let delta = new_length as isize - old_length as isize;
                    if delta != 0 {
                        for following in cache.block_starts.iter_mut().skip(index + 1) {
                            if delta > 0 {
                                *following += delta as usize;
                            } else {
                                *following = following.saturating_sub((-delta) as usize);
                            }
                        }
                    }
                }
            }

            cache.last_update_start = first_changed.min(cache.lines.len());
            cache.dirty = false;
            cache.generation = cache.generation.saturating_add(1);
            let history_prepended = self.history_prepended.replace(false);
            if !self.follow_tail && self.viewport_anchor.get().is_none() && !history_prepended {
                let current = self.scroll_from_bottom.get();
                if cache.lines.len() >= previous_line_count {
                    self.scroll_from_bottom
                        .set(current.saturating_add(cache.lines.len() - previous_line_count));
                } else {
                    self.scroll_from_bottom
                        .set(current.saturating_sub(previous_line_count - cache.lines.len()));
                }
            }
        }
        Ref::map(self.transcript_cache.borrow(), |cache| &cache.lines)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::tui::view::InteractiveShell;
    use sexy_tui_rs::strip_terminal_sequences;

    #[test]
    fn welcome_prefix_replacement_reanchors_following_blocks() {
        let mut cache = TranscriptCache {
            width: Some(80),
            lines: vec!["old 1".into(), "old 2".into(), "history".into()],
            welcome_overlay_active: false,
            block_starts: vec![2],
            block_lengths: vec![1],
            block_geometries: vec![SurfaceGeometry::default()],
            block_revisions: vec![0],
            dirty_blocks: Vec::new(),
            dirty: true,
            generation: 0,
            last_update_start: 0,
        };
        let mut first_changed = cache.lines.len();
        replace_welcome_prefix(&mut cache, vec!["new".into()], false, &mut first_changed);
        assert_eq!(cache.lines, ["new", "history"]);
        assert_eq!(cache.block_starts, [1]);
        assert_eq!(first_changed, 0);
    }

    #[test]
    fn welcome_animation_and_overlay_changes_replace_one_prefix() {
        let mut shell = InteractiveShell::test_shell();
        let started = Instant::now() - Duration::from_millis(350);
        shell.state.borrow_mut().startup_card_started_at = Some(started);
        let first = shell.state.borrow().rendered_transcript(80).clone();

        let next_started = Instant::now() - Duration::from_millis(1400);
        {
            let mut state = shell.state.borrow_mut();
            state.startup_card_started_at = Some(next_started);
            state.invalidate_transcript();
        }
        let second = shell.state.borrow().rendered_transcript(80).clone();
        assert_ne!(
            first, second,
            "animation must replace the cached welcome prefix"
        );
        assert_eq!(
            shell
                .state
                .borrow()
                .transcript_cache
                .borrow()
                .last_update_start,
            0,
            "prefix changes must redraw from the first logical row"
        );

        shell.show_overlay_text("overlay".into());
        let overlay = shell.state.borrow().rendered_transcript(80).clone();
        assert!(!overlay
            .iter()
            .any(|line| { strip_terminal_sequences(line).contains("octet") }));
        shell.close_overlay();
        let restored = shell.state.borrow().rendered_transcript(80).clone();
        assert!(restored
            .iter()
            .any(|line| strip_terminal_sequences(line).contains("octet")));
    }
}
