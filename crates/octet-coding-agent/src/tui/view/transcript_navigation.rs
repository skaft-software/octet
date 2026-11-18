//! Application-owned transcript find/scroll controls. Search uses rendered,
//! sanitized rows, never raw protocol messages; decorations never enter copy.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use sexy_tui_rs::{slice_by_column, utils::background_sgr_at_column, CURSOR_MARKER};
use std::hash::{Hash, Hasher};
use unicode_segmentation::UnicodeSegmentation;

use super::*;
use crate::tui::keymap;

const MAX_QUERY_BYTES: usize = 1024;
const MAX_CORPUS_BYTES: usize = 4 * 1024 * 1024;
const MAX_CORPUS_SPANS: usize = 100_000;
const MAX_MATCHES: usize = 10_000;
const SCROLLBAR_HIDE_DELAY: Duration = Duration::from_millis(1000);

/// Presentation of the application-owned transcript scrollbar. Native mouse
/// history remains terminal-owned until semantic navigation is requested.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TranscriptScrollbar {
    Hidden,
    /// Pi's fullscreen default: visible while scrolling, hidden after a second.
    #[default]
    Auto,
    Always,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchSegment {
    row: usize,
    start: usize,
    end: usize,
}

#[derive(Clone)]
struct CorpusSpan {
    start: usize,
    end: usize,
    segment: MatchSegment,
    linear: bool,
}

#[derive(Clone, Default)]
struct SearchCache {
    source: Option<(u64, u16)>,
    corpus: String,
    spans: Vec<CorpusSpan>,
    query: String,
    matches: Vec<Vec<MatchSegment>>,
    corpus_limited: bool,
    matches_limited: bool,
    row_fingerprints: Vec<(u64, usize)>,
    source_lines: usize,
    builds: usize,
    searches: usize,
}

impl SearchCache {
    fn update(
        &mut self,
        lines: &[String],
        generation: u64,
        width: u16,
        changed_row: usize,
        query: &str,
    ) {
        let mut source_changed = self.source != Some((generation, width));
        if source_changed {
            // Styling/timer animation must not rebuild a historic search corpus.
            // Check only the changed suffix when observing consecutive layouts.
            let from = if self.source.is_some_and(|(old, columns)| {
                columns == width && old.saturating_add(1) == generation
            }) {
                changed_row.min(self.row_fingerprints.len())
            } else {
                0
            };
            let mut bytes = from
                .checked_sub(1)
                .map_or(0, |row| self.row_fingerprints[row].1);
            let mut content_changed = self.source.is_none_or(|(_, columns)| columns != width)
                || self.source_lines != lines.len();
            let mut count = from;
            for (row, line) in lines.iter().enumerate().skip(from) {
                if row >= MAX_CORPUS_SPANS || bytes.saturating_add(line.len()) > MAX_CORPUS_BYTES {
                    break;
                }
                let plain = strip_terminal_sequences(line);
                bytes += line.len();
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                plain.hash(&mut hash);
                let fingerprint = hash.finish();
                if let Some(previous) = self.row_fingerprints.get_mut(row) {
                    content_changed |= previous.0 != fingerprint;
                    *previous = (fingerprint, bytes);
                } else {
                    self.row_fingerprints.push((fingerprint, bytes));
                    content_changed = true;
                }
                count += 1;
            }
            content_changed |= count != self.row_fingerprints.len();
            self.row_fingerprints.truncate(count);
            self.source_lines = lines.len();
            self.source = Some((generation, width));
            source_changed = content_changed;
        }
        if source_changed {
            self.corpus.clear();
            self.spans.clear();
            self.corpus_limited = self.row_fingerprints.len() < lines.len();
            let mut separator = false;
            'rows: for (row, styled) in lines.iter().take(self.row_fingerprints.len()).enumerate() {
                let plain = strip_terminal_sequences(styled);
                let mut column = 0;
                // Store ASCII words as runs, not one mapping/allocation per cell.
                let ascii = plain.is_ascii();
                let pieces: Vec<&str> = if ascii {
                    plain.split_inclusive(' ').collect()
                } else {
                    plain.graphemes(true).collect()
                };
                for piece in pieces {
                    let text = piece.trim_end_matches(' ');
                    let width = visible_width(piece);
                    if text.is_empty() || text.chars().all(char::is_whitespace) {
                        separator |= !self.corpus.is_empty();
                        column += width;
                        continue;
                    }
                    let folded = text.to_lowercase();
                    if self.corpus.len().saturating_add(folded.len() + 1) > MAX_CORPUS_BYTES
                        || self.spans.len() >= MAX_CORPUS_SPANS
                    {
                        self.corpus_limited = true;
                        break 'rows;
                    }
                    if separator {
                        self.corpus.push(' ');
                    }
                    let start = self.corpus.len();
                    self.corpus.push_str(&folded);
                    self.spans.push(CorpusSpan {
                        start,
                        end: self.corpus.len(),
                        segment: MatchSegment {
                            row,
                            start: column,
                            end: column + visible_width(text),
                        },
                        linear: ascii,
                    });
                    column += width;
                    separator = text.len() < piece.len();
                }
                separator |= !self.corpus.is_empty();
            }
            self.source = Some((generation, width));
            self.builds += 1;
        }
        let query = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !source_changed && self.query == query {
            return;
        }
        self.query = query;
        self.matches.clear();
        self.matches_limited = false;
        self.searches += 1;
        if self.query.is_empty() {
            return;
        }
        let mut span_index = 0;
        for (start, text) in self.corpus.match_indices(&self.query) {
            if self.matches.len() == MAX_MATCHES {
                self.matches_limited = true;
                break;
            }
            let end = start + text.len();
            while span_index < self.spans.len() && self.spans[span_index].end <= start {
                span_index += 1;
            }
            let mut segments: Vec<MatchSegment> = Vec::new();
            for span in &self.spans[span_index..] {
                if span.start >= end {
                    break;
                }
                let mut segment = span.segment.clone();
                if span.linear {
                    segment.start += start.saturating_sub(span.start);
                    segment.end = span.segment.start + end.min(span.end) - span.start;
                }
                if let Some(previous) = segments
                    .last_mut()
                    .filter(|previous| previous.row == segment.row && segment.start <= previous.end)
                {
                    previous.end = previous.end.max(segment.end);
                } else {
                    segments.push(segment);
                }
            }
            if !segments.is_empty() {
                self.matches.push(segments);
            }
        }
    }
}

#[derive(Default)]
struct TranscriptSearch {
    editor: TextEditor,
    cache: std::sync::Arc<SearchCache>,
    current: usize,
    pending_steps: isize,
    anchor_row: usize,
    select_from_anchor: bool,
    reveal: bool,
    hover: Option<SearchButton>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchButton {
    Previous,
    Next,
    Close,
}

#[derive(Clone, Copy)]
struct ScrollbarGeometry {
    column: u16,
    rows: usize,
    thumb_top: usize,
    thumb_rows: usize,
    maximum: usize,
    visible: bool,
}

#[derive(Clone, Copy)]
struct JumpToBottomButton {
    row: usize,
    column: u16,
    width: u16,
}

#[derive(Clone, Copy)]
pub(super) struct NavigationFrame {
    size: (u16, u16),
    scrollbar: Option<ScrollbarGeometry>,
    search_buttons_row: Option<usize>,
    jump_to_bottom: Option<JumpToBottomButton>,
}

#[cfg(test)]
impl TranscriptNavigation {
    /// Test shells keep the transient scrollbar column out of row assertions;
    /// tests about the scrollbar itself opt back in.
    pub(super) fn without_scrollbar() -> Self {
        Self {
            mode: TranscriptScrollbar::Hidden,
            ..Self::default()
        }
    }
}

#[derive(Default)]
pub(super) struct TranscriptNavigation {
    search: Option<TranscriptSearch>,
    mode: TranscriptScrollbar,
    hover: bool,
    jump_to_bottom_hover: bool,
    drag_offset: Option<usize>,
    hide_at: Option<Instant>,
    frame: Option<NavigationFrame>,
}

impl TranscriptNavigation {
    /// Bounded presentation snapshot: the at-most-1-KiB query is copied, while
    /// immutable corpus/match storage is shared with the renderer. No terminal
    /// layout or history-sized copy happens under the input-owner lock.
    pub(super) fn render_snapshot(&self) -> Self {
        Self {
            search: self.search.as_ref().map(|search| {
                let mut editor = TextEditor::new();
                editor.set_text(search.editor.text());
                editor.set_cursor(search.editor.cursor());
                TranscriptSearch {
                    editor,
                    cache: search.cache.clone(),
                    current: search.current,
                    pending_steps: search.pending_steps,
                    anchor_row: search.anchor_row,
                    select_from_anchor: search.select_from_anchor,
                    reveal: search.reveal,
                    hover: search.hover,
                }
            }),
            mode: self.mode,
            hover: self.hover,
            jump_to_bottom_hover: self.jump_to_bottom_hover,
            drag_offset: self.drag_offset,
            hide_at: self.hide_at,
            frame: self.frame,
        }
    }

    pub(super) fn rendered_frame(&self) -> Option<NavigationFrame> {
        self.frame
    }

    /// Publish hit targets only after the terminal write and geometry fence.
    /// Unrelated semantic borrows must not discard a painted scrollbar.
    pub(super) fn accept_rendered_frame(&mut self, frame: Option<NavigationFrame>) {
        self.frame = frame;
    }

    /// The caller must fence this by the complete shell input/render revision.
    /// Draft/query edits never flow backwards from a completed renderer frame.
    pub(super) fn apply_render_feedback(&mut self, rendered: &Self) {
        if let (Some(search), Some(result)) = (&mut self.search, &rendered.search) {
            if search.editor.text() == result.editor.text() {
                search.cache = result.cache.clone();
                search.current = result.current;
                search.pending_steps = result.pending_steps;
                search.select_from_anchor = result.select_from_anchor;
                search.reveal = result.reveal;
            }
        }
    }

    pub(super) fn reset_session(&mut self) {
        *self = Self {
            mode: self.mode,
            ..Self::default()
        };
    }

    fn activity(&mut self, now: Instant) {
        if self.mode == TranscriptScrollbar::Auto {
            self.hide_at = Some(now + SCROLLBAR_HIDE_DELAY);
        }
    }

    fn reset_pointer(&mut self) {
        self.hover = false;
        self.jump_to_bottom_hover = false;
        self.drag_offset = None;
        if let Some(search) = self.search.as_mut() {
            search.hover = None;
        }
        self.frame = None;
    }
}

impl ShellState {
    pub(super) fn reset_transcript_navigation_pointer(&self) {
        self.transcript_navigation.borrow_mut().reset_pointer();
    }

    pub(crate) fn transcript_search_active(&self) -> bool {
        self.panel.is_none()
            && self.overlay.is_none()
            && self.tool_input_prompt.is_none()
            && self.transcript_navigation.borrow().search.is_some()
    }

    pub(super) fn transcript_content_width(&self, width: u16) -> u16 {
        if self.application_viewport_requested
            && self.transcript_navigation.borrow().mode == TranscriptScrollbar::Always
            && width > 1
        {
            width - 1
        } else {
            width
        }
    }

    pub(super) fn transcript_scroll_activity(&self) {
        self.transcript_navigation
            .borrow_mut()
            .activity(Instant::now());
    }

    pub(super) fn transcript_scrollbar_deadline(&self) -> Option<Instant> {
        let nav = self.transcript_navigation.borrow();
        (self.panel.is_none()
            && self.overlay.is_none()
            && self.tool_input_prompt.is_none()
            && nav.mode == TranscriptScrollbar::Auto
            && !nav.hover
            && nav.drag_offset.is_none())
        .then_some(nav.hide_at)
        .flatten()
    }

    pub(super) fn expire_transcript_scrollbar(&self, now: Instant) {
        if self
            .transcript_scrollbar_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            self.transcript_navigation.borrow_mut().hide_at = None;
        }
    }

    pub(super) fn refresh_transcript_search(&self, width: u16) {
        if !self.transcript_search_active() || self.render_threaded {
            return;
        }
        let lines = self.rendered_transcript(width);
        let cache = self.transcript_cache.borrow();
        let mut nav = self.transcript_navigation.borrow_mut();
        let search = nav.search.as_mut().expect("active search");
        let columns = cache.width.unwrap_or(width);
        let query = search
            .editor
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if search.cache.source != Some((cache.generation, columns)) || search.cache.query != query {
            // Snapshot and feedback share immutable search results. Only an
            // actual content/query revision takes ownership of their cache.
            std::sync::Arc::make_mut(&mut search.cache).update(
                &lines,
                cache.generation,
                columns,
                cache.last_update_start,
                search.editor.text(),
            );
        }
        if std::mem::take(&mut search.select_from_anchor) {
            let index = search
                .cache
                .matches
                .partition_point(|segments| segments[0].row < search.anchor_row);
            search.current = if index < search.cache.matches.len() {
                index
            } else {
                0
            };
        }
        search.current = search
            .current
            .min(search.cache.matches.len().saturating_sub(1));
        let count = search.cache.matches.len();
        if count > 0 && search.pending_steps != 0 {
            search.current = (search.current as isize)
                .saturating_add(std::mem::take(&mut search.pending_steps))
                .rem_euclid(count as isize) as usize;
        }
    }

    /// Render only the active query owner's cursor. Buttons precede the result
    /// count so narrow terminals retain real hit targets rather than ghost ones.
    pub(super) fn transcript_search_panel(&self, width: u16, available: usize) -> Vec<String> {
        if !self.transcript_search_active() || available == 0 {
            return Vec::new();
        }
        self.refresh_transcript_search(width);
        let nav = self.transcript_navigation.borrow();
        let search = nav.search.as_ref().expect("active search");
        let cursor = search.editor.cursor();
        let query = search.editor.text();
        let prefix = if width >= 8 { "Find: " } else { "" };
        let budget = usize::from(width).saturating_sub(prefix.len() + 1);
        let before = &query[..cursor];
        let before = slice_by_column(
            before,
            visible_width(before).saturating_sub(budget),
            budget,
            true,
        );
        let after = slice_by_column(
            &query[cursor..],
            0,
            budget.saturating_sub(visible_width(&before)),
            true,
        );
        let mut rows = vec![format!("{prefix}{before}{CURSOR_MARKER}{after}")];
        if available > 1 {
            let button = |kind, label| {
                if search.hover == Some(kind) {
                    self.theme.bold(&self.theme.fg("accent", label))
                } else {
                    self.theme.fg("muted", label)
                }
            };
            let count = search.cache.matches.len();
            let limited = if search.cache.corpus_limited || search.cache.matches_limited {
                " (limited)"
            } else {
                ""
            };
            rows.push(fit_line(
                &format!(
                    "{} {} {} {}/{count}{limited} · Enter next · Shift+Enter previous · Esc close",
                    button(SearchButton::Previous, "[<]"),
                    button(SearchButton::Next, "[>]"),
                    button(SearchButton::Close, "[x]"),
                    if count == 0 { 0 } else { search.current + 1 }
                ),
                width,
            ));
        }
        rows
    }

    pub(super) fn reveal_transcript_search(&self, length: usize, available: usize) {
        let row = {
            let mut nav = self.transcript_navigation.borrow_mut();
            nav.search.as_mut().and_then(|search| {
                std::mem::take(&mut search.reveal)
                    .then(|| {
                        search
                            .cache
                            .matches
                            .get(search.current)
                            .and_then(|segments| segments.first())
                            .map(|segment| segment.row)
                    })
                    .flatten()
            })
        };
        if let Some(row) = row {
            let capacity = transcript_viewport_capacity(available, true).max(1);
            let next = length.saturating_sub(row.saturating_add(capacity));
            self.viewport_anchor.set(None);
            self.scroll_from_bottom
                .set(next.min(super::viewport::max_scroll_for_available(length, available)));
            self.transcript_scroll_activity();
        }
    }

    pub(super) fn decorate_transcript_search(&self, lines: &mut [String], start: usize) {
        if !self.transcript_search_active() {
            return;
        }
        let nav = self.transcript_navigation.borrow();
        let search = nav.search.as_ref().expect("active search");
        // Matches are row ordered; only visible matches participate in painting.
        let first = search
            .cache
            .matches
            .partition_point(|segments| segments.last().is_some_and(|segment| segment.row < start));
        let mut by_row: Vec<Vec<(usize, usize, bool)>> = vec![Vec::new(); lines.len()];
        for (index, segments) in search.cache.matches.iter().enumerate().skip(first) {
            if segments[0].row >= start + lines.len() {
                break;
            }
            for segment in segments {
                if let Some(ranges) = segment
                    .row
                    .checked_sub(start)
                    .and_then(|row| by_row.get_mut(row))
                {
                    ranges.push((segment.start, segment.end, index == search.current));
                }
            }
        }
        let color = self.theme.capabilities().color != crate::tui::terminal::ColorDepth::None;
        for (line, ranges) in lines.iter_mut().zip(by_row) {
            if ranges.is_empty() || !color {
                continue;
            }
            let mut styled = String::new();
            let mut column = 0;
            for (start, end, current) in ranges {
                styled.push_str(&slice_by_column(
                    line,
                    column,
                    start.saturating_sub(column),
                    true,
                ));
                styled.push_str("\x1b[0m");
                let matched = strip_terminal_sequences(&slice_by_column(
                    line,
                    start,
                    end.saturating_sub(start),
                    true,
                ));
                styled.push_str(&self.theme.transcript_search_match(&matched, current));
                column = end;
            }
            styled.push_str(&slice_by_column(
                line,
                column,
                visible_width(line).saturating_sub(column),
                true,
            ));
            styled.push_str("\x1b[0m");
            if line.contains("\x1b]8;") {
                styled.push_str("\x1b]8;;\x1b\\");
            }
            *line = styled;
        }
    }

    pub(super) fn decorate_transcript_navigation(
        &self,
        lines: &mut Vec<String>,
        width: u16,
        chrome: &super::shell_chrome::ShellChrome,
        now: Instant,
    ) {
        if self.overlay.is_some() || self.panel.is_some() || self.tool_input_prompt.is_some() {
            let mut nav = self.transcript_navigation.borrow_mut();
            nav.reset_pointer();
            return;
        }
        let length = self.rendered_transcript(width).len();
        let scroll = resolved_scroll_from_bottom(self, length, chrome.transcript_rows);
        let rows = transcript_viewport_capacity(chrome.transcript_rows, scroll > 0);
        let maximum = super::viewport::max_scroll_for_available(length, chrome.transcript_rows);
        let mut nav = self.transcript_navigation.borrow_mut();
        let scrollbar = if nav.mode != TranscriptScrollbar::Hidden && rows > 0 && width > 1 {
            // A scrollable track with at least two rows needs room to move.
            let cap = rows - usize::from(maximum > 0 && rows > 1);
            let length = length.max(rows);
            let thumb_rows =
                ((rows.saturating_mul(rows) + length / 2) / length).clamp(cap.min(2), cap);
            let thumb_top = maximum
                .saturating_sub(scroll)
                .saturating_mul(rows - thumb_rows)
                .saturating_add(maximum / 2)
                .checked_div(maximum)
                .unwrap_or(0);
            let visible = nav.mode == TranscriptScrollbar::Always
                || (maximum > 0
                    && (nav.hover
                        || nav.drag_offset.is_some()
                        || nav.hide_at.is_some_and(|deadline| deadline > now)));
            Some(ScrollbarGeometry {
                column: width - 1,
                rows,
                thumb_rows,
                thumb_top,
                maximum,
                visible,
            })
        } else {
            None
        };
        let active = nav.hover || nav.drag_offset.is_some();
        if let Some(bar) = scrollbar.filter(|bar| bar.visible) {
            let colored = self.theme.capabilities().color != crate::tui::terminal::ColorDepth::None;
            lines.resize_with(lines.len().max(bar.rows), String::new);
            for (row, line) in lines.iter_mut().take(bar.rows).enumerate() {
                let column = usize::from(bar.column);
                let prefix = slice_by_column(line, 0, column, true);
                let padding = column.saturating_sub(visible_width(&prefix));
                let thumb = row >= bar.thumb_top && row < bar.thumb_top + bar.thumb_rows;
                let glyph = match (thumb, self.theme.unicode()) {
                    (true, true) => "┃",
                    (true, false) => "#",
                    (false, true) => self.theme.glyph("vertical"),
                    (false, false) if self.theme.glyph("vertical").trim().is_empty() => " ",
                    (false, false) => ".",
                };
                let painted = self.theme.transcript_scrollbar_glyph(glyph, thumb, active);
                let background = if colored && nav.mode == TranscriptScrollbar::Auto {
                    background_sgr_at_column(line, column).unwrap_or_default()
                } else {
                    String::new()
                };
                let reset = if colored { "\x1b[0m" } else { "" };
                let close_link = if prefix.contains("\x1b]8;") {
                    "\x1b]8;;\x1b\\"
                } else {
                    ""
                };
                *line = format!(
                    "{prefix}{reset}{close_link}{background}{}{painted}{reset}",
                    " ".repeat(padding)
                );
            }
        }
        // Native terminal ownership keeps this control and its exact painted
        // hit rectangle in the host. Pi compatibility source: packages/tui/src/
        // tui-alt-screen.ts:1655-1676 centers its scroll-to-end control on the
        // last viewport row and retains only the control's occupied cells.
        let jump_to_bottom = if scroll > 0 && chrome.transcript_rows > 0 && width > 0 {
            let padding = self
                .theme
                .layout()
                .block_padding_x
                .unwrap_or(0)
                .min(width.saturating_sub(1) / 2);
            let available = usize::from(width - 2 * padding);
            let arrow = if self.theme.unicode() { "↓" } else { "v" };
            let mut label = format!("[ {arrow} Jump to bottom ]");
            if self.new_output_count > 0 {
                let count_label = format!(
                    "[ {arrow} Jump to bottom{}{} new ]",
                    semantic_separator(&self.theme),
                    self.new_output_count
                );
                if visible_width(&count_label) <= available {
                    label = count_label;
                }
            }
            let label = sexy_tui_rs::truncate_to_width(&label, available, Some(""));
            let button_width = visible_width(&label) as u16;
            let column = (width - button_width) / 2;
            // This is the reserved breathing row, including at the oldest row
            // where fewer transcript lines might fill the viewport.
            lines.resize_with(chrome.transcript_rows, String::new);
            lines[rows] = format!(
                "{}{}",
                " ".repeat(usize::from(column)),
                self.theme
                    .transcript_scroll_button(&label, nav.jump_to_bottom_hover)
            );
            Some(JumpToBottomButton {
                row: rows,
                column,
                width: button_width,
            })
        } else {
            nav.jump_to_bottom_hover = false;
            None
        };
        nav.frame = Some(NavigationFrame {
            size: self.size,
            scrollbar,
            jump_to_bottom,
            search_buttons_row: (nav.search.is_some() && chrome.panel.len() > 1).then_some(
                chrome.transcript_rows
                    + chrome.header.len()
                    + chrome.error.len()
                    + chrome.pending.len()
                    + 1,
            ),
        });
    }
}

impl InteractiveShell {
    /// True only while the transcript search, not a picker/tool prompt, owns
    /// text input. Clipboard/extension predispatch must respect this owner.
    pub fn transcript_search_active(&self) -> bool {
        self.state.borrow().transcript_search_active()
    }

    pub fn set_transcript_scrollbar(&mut self, mode: TranscriptScrollbar) {
        let mut state = self.state.borrow_mut();
        let mut nav = state.transcript_navigation.borrow_mut();
        nav.mode = mode;
        nav.reset_pointer();
        nav.activity(Instant::now());
        drop(nav);
        if mode != TranscriptScrollbar::Hidden {
            state.application_viewport_requested = true;
        }
        state.invalidate_transcript_layout();
        drop(state);
        self.render();
    }

    pub(super) fn close_transcript_navigation(&self) {
        let state = self.state.borrow();
        let mut nav = state.transcript_navigation.borrow_mut();
        nav.search = None;
        nav.reset_pointer();
    }

    fn open_transcript_search(&mut self) {
        if !self.state.borrow().run.is_active() {
            if let Err(error) = self.materialize_deferred_history() {
                self.state.borrow_mut().error =
                    Some(format!("could not load older session history: {error}"));
            }
        }
        self.reset_input_interaction();
        let mut state = self.state.borrow_mut();
        let anchor_row = if state.render_threaded {
            state
                .retained_render_geometry()
                .map_or(0, |geometry| geometry.visible_start)
        } else {
            let chrome = shell_chrome(&state, state.size.0, Instant::now());
            let length = state.rendered_transcript(state.size.0).len();
            let scroll = resolved_scroll_from_bottom(&state, length, chrome.transcript_rows);
            length
                .saturating_sub(scroll)
                .saturating_sub(transcript_viewport_capacity(
                    chrome.transcript_rows,
                    scroll > 0,
                ))
        };
        state.application_viewport_requested = true;
        state.follow_tail = false;
        state.pending_selection_anchor = None;
        state.selection_dragging = false;
        state.extension_autocomplete = None;
        state.transcript_navigation.borrow_mut().search = Some(TranscriptSearch {
            anchor_row,
            ..TranscriptSearch::default()
        });
    }

    pub(super) fn edit_transcript_search(&mut self, action: EditAction) {
        let mut state = self.state.borrow_mut();
        let mut nav = state.transcript_navigation.borrow_mut();
        let Some(search) = nav.search.as_mut() else {
            return;
        };
        let action = match action {
            EditAction::Newline | EditAction::Up | EditAction::Down => return,
            EditAction::Paste(text) => {
                let remaining = MAX_QUERY_BYTES.saturating_sub(search.editor.text().len());
                let text = sanitize_for_terminal(&text).replace(['\r', '\n', '\t'], " ");
                let mut end = text.len().min(remaining);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                EditAction::Paste(text[..end].to_owned())
            }
            EditAction::Char(ch)
                if ch.is_control()
                    || search.editor.text().len() + ch.len_utf8() > MAX_QUERY_BYTES =>
            {
                return
            }
            action => action,
        };
        let revision = search.editor.text_revision();
        search.editor.apply(action, MAX_QUERY_BYTES);
        if search.editor.text().len() > MAX_QUERY_BYTES {
            let mut end = MAX_QUERY_BYTES;
            while !search.editor.text().is_char_boundary(end) {
                end -= 1;
            }
            let bounded = search.editor.text()[..end].to_owned();
            search.editor.set_text(bounded);
        }
        let changed = revision != search.editor.text_revision();
        if changed {
            search.pending_steps = 0;
            search.anchor_row = search
                .cache
                .matches
                .get(search.current)
                .and_then(|segments| segments.first())
                .map_or(search.anchor_row, |segment| segment.row);
            search.select_from_anchor = true;
            search.reveal = true;
        }
        drop(nav);
        if changed {
            state.follow_tail = false;
        }
    }

    fn navigate_transcript_search(&mut self, forward: bool) {
        let mut state = self.state.borrow_mut();
        state.refresh_transcript_search(state.size.0);
        let mut nav = state.transcript_navigation.borrow_mut();
        if let Some(search) = nav.search.as_mut() {
            if state.render_threaded {
                search.pending_steps =
                    search
                        .pending_steps
                        .saturating_add(if forward { 1 } else { -1 });
                search.reveal = true;
                drop(nav);
                state.follow_tail = false;
                return;
            }
            let length = search.cache.matches.len();
            if length > 0 {
                search.current = if forward {
                    (search.current + 1) % length
                } else {
                    (search.current + length - 1) % length
                };
                search.reveal = true;
            }
        }
        drop(nav);
        state.follow_tail = false;
    }

    /// Call before extension shortcuts and clipboard/paste admission. Returns
    /// true when the query or a rendered navigation control consumed the event.
    /// translate_input also invokes it as the common dispatch fallback.
    pub fn intercept_transcript_input(&mut self, event: &Event) -> bool {
        if let Event::Mouse(mouse) = event {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && mouse.modifiers.is_empty()
                && super::transcript_selection::click_splash(
                    &mut self.state.borrow_mut(),
                    mouse.column,
                    mouse.row,
                    Instant::now(),
                )
            {
                self.render();
                return true;
            }
        }
        if !matches!(
            event,
            Event::Mouse(mouse) if matches!(
                mouse.kind,
                MouseEventKind::Up(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Moved
            )
        ) {
            // Revoke stale click targets before other input owners consume the
            // event, but keep an active composer selection through wheel input.
            // Native terminal gesture ownership cannot live in the adapter.
            // Pi compatibility: packages/tui/src/tui-alt-screen.ts:719-733,
            // 1335-1378 routes wheels without retiring selection ownership.
            let mut state = self.state.borrow_mut();
            let composer_drag = matches!(
                event,
                Event::Mouse(mouse) if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                        | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                )
            ) && state.pending_screen_press.as_ref().is_some_and(|press| {
                press.moved
                    && matches!(press.target, super::ScreenPressTarget::Composer(_))
                    && press.fence.composer_current(&state)
                    && super::tern::editor_focused(&state)
            });
            if !composer_drag {
                state.pending_screen_press = None;
            }
        }
        if matches!(
            event,
            Event::Resize(..) | Event::FocusLost | Event::FocusGained
        ) {
            let mut state = self.state.borrow_mut();
            state.transcript_navigation.borrow_mut().reset_pointer();
            // Pi compatibility: packages/tui/src/tui-alt-screen.ts:695-715
            // cancels an active selection on focus loss, never on release.
            state.cancel_transcript_pointer();
            return false;
        }
        let exclusive = {
            let state = self.state.borrow();
            state.startup_pending
                || state.panel.is_some()
                || state.overlay.is_some()
                || state.tool_input_prompt.is_some()
        };
        if exclusive {
            self.state.borrow_mut().cancel_transcript_pointer();
            self.close_transcript_navigation();
            return false;
        }
        if let Event::Key(key) = event {
            if keymap::is_close_key(key) {
                return false;
            }
            if key.kind == KeyEventKind::Release {
                return self.transcript_search_active();
            }
            let bindings = &self.input_dispatch.bindings;
            if !self.transcript_search_active()
                && keymap::editor_binding(key, bindings, true).is_some()
                && !["tui.altScreen.search", "tui.altScreen.toggleScrollbar"]
                    .iter()
                    .any(|id| {
                        bindings.user_bindings().contains_key(*id) && bindings.matches(key, id)
                    })
            {
                return false;
            }
            if key.kind == KeyEventKind::Press && bindings.matches(key, "tui.altScreen.search") {
                if self.transcript_search_active() {
                    self.close_transcript_navigation();
                } else {
                    self.open_transcript_search();
                }
                self.render();
                return true;
            }
            if self.transcript_search_active() {
                if bindings.matches(key, "tui.altScreen.searchClose")
                    || (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL)
                {
                    self.close_transcript_navigation();
                } else if bindings.matches(key, "tui.altScreen.searchPrevious") {
                    self.navigate_transcript_search(false);
                } else if bindings.matches(key, "tui.altScreen.searchNext") {
                    self.navigate_transcript_search(true);
                } else if bindings.matches(key, "tui.altScreen.pageUp")
                    || bindings.matches(key, "tui.editor.pageUp")
                {
                    self.scroll(-1);
                } else if bindings.matches(key, "tui.altScreen.pageDown")
                    || bindings.matches(key, "tui.editor.pageDown")
                {
                    self.scroll(1);
                } else if let Some(action) = keymap::editor_binding(key, bindings, false) {
                    self.edit_transcript_search(action);
                } else if let KeyCode::Char(ch) = key.code {
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                        self.edit_transcript_search(EditAction::Char(ch));
                    }
                }
                self.render();
                return true;
            }
            if key.kind == KeyEventKind::Press
                && bindings.matches(key, "tui.altScreen.toggleScrollbar")
            {
                let mode = match self.state.borrow().transcript_navigation.borrow().mode {
                    TranscriptScrollbar::Hidden => TranscriptScrollbar::Auto,
                    TranscriptScrollbar::Auto => TranscriptScrollbar::Always,
                    TranscriptScrollbar::Always => TranscriptScrollbar::Hidden,
                };
                self.set_transcript_scrollbar(mode);
                return true;
            }
        }
        if let Event::Paste(text) = event {
            if self.transcript_search_active() {
                self.edit_transcript_search(EditAction::Paste(text.clone()));
                self.render();
                return true;
            }
        }
        if let Event::Mouse(mouse) = event {
            let consumed = self.transcript_navigation_mouse(*mouse);
            if consumed {
                self.render();
                return true;
            }
            if self.transcript_search_active() {
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let step = crate::tui::keymap::wheel_scroll_rows(mouse.modifiers);
                        self.scroll_lines(if mouse.kind == MouseEventKind::ScrollUp {
                            -step
                        } else {
                            step
                        });
                    }
                    MouseEventKind::Down(MouseButton::Left) => self.begin_transcript_selection(
                        mouse.row,
                        mouse.column,
                        mouse.modifiers.contains(KeyModifiers::SHIFT),
                    ),
                    MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                        self.extend_transcript_selection(mouse.row, mouse.column)
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        self.end_transcript_selection(mouse.row, mouse.column)
                    }
                    _ => {}
                }
                self.render();
                return true;
            }
        }
        false
    }

    fn transcript_navigation_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        // Wheel input is independent of a captured scrollbar press, including
        // over the hint and outside the track. Pi compatibility:
        // packages/tui/src/tui-alt-screen.ts:719-733 routes wheels before its
        // scrollbar gesture handler. Terminal dispatch belongs to this host.
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return false;
        }
        let now = Instant::now();
        let mut state = self.state.borrow_mut();
        if !state.application_viewport_requested {
            return false;
        }
        // Validate the retained frame before taking the mutable navigation
        // borrow. Threaded geometry validation reads the scrollbar mode via
        // `transcript_content_width`, which borrows this same RefCell.
        // Native terminal ownership requires host-side hit testing. Pi uses
        // painted layout for hover, presses and captured drags, not a matching
        // transcript generation (packages/tui/src/tui-alt-screen.ts:1050-1144).
        // Keep layout fences, and let a captured gesture own its scroll changes.
        let captured = state.transcript_navigation.borrow().drag_offset.is_some();
        let frame = if state.render_threaded {
            let geometry = if captured {
                state.gesture_render_geometry()
            } else {
                state.pointer_render_geometry()
            };
            geometry.and_then(|geometry| geometry.navigation_frame)
        } else {
            state.transcript_navigation.borrow().frame
        };
        let frame = frame.filter(|frame| frame.size == state.size);
        let mut nav = state.transcript_navigation.borrow_mut();
        let Some(frame) = frame else {
            nav.reset_pointer();
            return false;
        };
        let bottom_hovered = !state.follow_tail
            && frame.jump_to_bottom.is_some_and(|button| {
                usize::from(mouse.row) == button.row
                    && mouse.column >= button.column
                    && mouse.column < button.column + button.width
            });
        if !state.selection_dragging
            && state.pending_selection_anchor.is_none()
            && nav.drag_offset.is_none()
        {
            if nav.jump_to_bottom_hover != bottom_hovered {
                nav.jump_to_bottom_hover = bottom_hovered;
                self.request_navigation_render();
            }
            // Terminal ownership keeps this in the host. Pi compatibility:
            // packages/tui/src/tui-alt-screen.ts:1050-1056 uses the last painted
            // rectangle, even when new transcript output is awaiting paint.
            if bottom_hovered
                && mouse.modifiers.is_empty()
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            {
                drop(nav);
                drop(state);
                self.jump_to_tail();
                return true;
            }
        }
        let button = if frame.search_buttons_row == Some(usize::from(mouse.row)) {
            match mouse.column {
                0..=2 if state.size.0 >= 3 => Some(SearchButton::Previous),
                4..=6 if state.size.0 >= 7 => Some(SearchButton::Next),
                8..=10 if state.size.0 >= 11 => Some(SearchButton::Close),
                _ => None,
            }
        } else {
            None
        };
        if let Some(search) = nav.search.as_mut() {
            if search.hover != button {
                search.hover = button;
                self.request_navigation_render();
            }
        }
        if nav.drag_offset.is_none() && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(button) = button {
                drop(nav);
                drop(state);
                match button {
                    SearchButton::Previous => self.navigate_transcript_search(false),
                    SearchButton::Next => self.navigate_transcript_search(true),
                    SearchButton::Close => self.close_transcript_navigation(),
                }
                return true;
            }
        }
        let hovered = frame.scrollbar.is_some_and(|bar| {
            mouse.column == bar.column
                && usize::from(mouse.row) < bar.rows
                && (bar.maximum > 0 || nav.mode == TranscriptScrollbar::Always)
        });
        if !state.selection_dragging
            && state.pending_selection_anchor.is_none()
            && nav.hover != hovered
        {
            nav.hover = hovered;
            nav.activity(now);
            self.request_navigation_render();
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) && nav.drag_offset.take().is_some() {
            nav.activity(now);
            return true;
        }
        let Some(bar) = frame.scrollbar else {
            return false;
        };
        let starting = !state.selection_dragging
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && hovered
            && (bar.visible || nav.mode == TranscriptScrollbar::Auto);
        let mut jump_on_press = false;
        if starting {
            let row = usize::from(mouse.row);
            let on_thumb = row >= bar.thumb_top && row < bar.thumb_top + bar.thumb_rows;
            nav.drag_offset = Some(if on_thumb {
                row - bar.thumb_top
            } else {
                bar.thumb_rows / 2
            });
            jump_on_press = !on_thumb;
        }
        if let Some(grab) = nav.drag_offset {
            let travel = bar.rows - bar.thumb_rows;
            let scroll_to = (travel > 0
                && (jump_on_press
                    || matches!(
                        mouse.kind,
                        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                    )))
            .then(|| {
                let offset = usize::from(mouse.row).saturating_sub(grab).min(travel);
                (offset.saturating_mul(bar.maximum) + travel / 2) / travel
            });
            if scroll_to.is_some() {
                nav.activity(now);
            }
            drop(nav);
            if starting {
                state.pending_selection_anchor = None;
                state.selection_dragging = false;
                state.transcript_selection = None;
            }
            if let Some(top) = scroll_to {
                state.viewport_anchor.set(None);
                state
                    .scroll_from_bottom
                    .set(bar.maximum.saturating_sub(top));
                state.follow_tail = top == bar.maximum;
                if state.follow_tail {
                    state.jump_to_tail();
                }
                retain_viewport_anchor(&state);
            }
            return true;
        }
        false
    }

    // Mouse hover uses the normal renderer notification without retaining a
    // mutable borrow of the shared state across render(). The inline test TUI
    // is refreshed by its explicit frame assertions.
    fn request_navigation_render(&self) {
        if let Some(tx) = self
            .render_tx
            .lock()
            .expect("render sender mutex poisoned")
            .as_ref()
        {
            let _ = tx.try_send(RenderCommand::Render);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::{keybindings::KeybindingsManager, InputAction, PointerGesture};
    use crossterm::event::{KeyEvent, MouseEvent};

    fn bottom_button_shell(threaded: bool, new: usize) -> InteractiveShell {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(60, 16);
        shell.capture_mouse = true;
        for index in 0..80 {
            shell.notice(format!("button history {index:02}"));
        }
        shell.scroll_lines(-1);
        shell.state.borrow_mut().new_output_count = new;
        if threaded {
            shell.isolate_native_test_renderer();
            shell.render_written_test_frame();
        } else {
            frame(&shell, Instant::now());
        }
        shell
    }

    fn bottom_button_row(shell: &InteractiveShell) -> u16 {
        let state = shell.state.borrow();
        transcript_viewport_capacity(
            shell_chrome(&state, state.size.0, Instant::now()).transcript_rows,
            true,
        ) as u16
    }

    fn paint_bottom_button(shell: &mut InteractiveShell) -> (u16, u16) {
        shell.render_written_test_frame();
        written_bottom_button(shell)
    }

    fn written_bottom_button(shell: &InteractiveShell) -> (u16, u16) {
        shell
            .tui
            .as_ref()
            .unwrap()
            .rendered_frame()
            .iter()
            .enumerate()
            .find_map(|(row, line)| {
                let plain = strip_terminal_sequences(line);
                let start = plain.find("[ ↓ Jump to bottom")?;
                Some((visible_width(&plain[..start]) as u16, row as u16))
            })
            .expect("button in the written terminal frame")
    }

    fn assert_bottom_button_click(shell: &mut InteractiveShell, column: u16, row: u16) {
        assert!(!shell.state.borrow().follow_tail);
        // Exercise the predispatch entry point used by both idle and active
        // event loops, including motion before the press and release.
        shell.intercept_transcript_input(&mouse(MouseEventKind::Moved, 0, row));
        shell.intercept_transcript_input(&mouse(MouseEventKind::Moved, column, row));
        let hovered = shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .jump_to_bottom_hover;
        assert!(
            shell.intercept_transcript_input(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                column,
                row,
            )),
            "button click must use the written rectangle"
        );
        assert!(hovered, "hover must use the written button rectangle");
        let active = shell.state.borrow().run.is_active();
        shell.translate_input(
            Some(mouse(MouseEventKind::Up(MouseButton::Left), column, row)),
            active,
        );
        let state = shell.state.borrow();
        assert!(state.follow_tail);
        assert_eq!(state.scroll_from_bottom.get(), 0);
        assert_eq!(state.new_output_count, 0);
        assert!(state.pending_selection_anchor.is_none());
        assert!(!state.selection_dragging);
    }

    #[test]
    fn jump_to_bottom_click_through_idle_event_entry_point() {
        for threaded in [false, true] {
            for pending_notice in [false, true] {
                let mut shell = bottom_button_shell(threaded, 2);
                let (column, row) = paint_bottom_button(&mut shell);
                if pending_notice {
                    shell.notice("idle session update");
                }
                assert_bottom_button_click(&mut shell, column, row);
            }
        }
    }

    #[test]
    fn jump_to_bottom_click_during_streaming_uses_written_rectangle() {
        for threaded in [false, true] {
            let mut shell = bottom_button_shell(threaded, 0);
            shell.on_prompt_submitted("stream a response");
            shell.scroll_lines(-1);
            for _ in 0..3 {
                shell.on_agent_event(&AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "streamed text\n".into(),
                });
                paint_bottom_button(&mut shell);
            }
            let (column, row) = paint_bottom_button(&mut shell);
            // A delta arrives after paint, before pointer input.
            shell.on_agent_event(&AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "unpainted delta\n".into(),
            });
            assert_bottom_button_click(&mut shell, column, row);
        }
    }

    #[test]
    fn jump_to_bottom_click_with_visible_subagent_roster_uses_written_row() {
        for threaded in [false, true] {
            let mut shell = bottom_button_shell(threaded, 0);
            shell.set_subagent_telemetry(
                Some(&octet_agent::DelegationTelemetrySnapshot {
                    revision: 1,
                    captured_at_ms: 1,
                    children: vec![octet_agent::DelegationTelemetryChild {
                        child_id: "worker-1".into(),
                        task_name: "Inspect tests".into(),
                        profile: None,
                        model: "fixture".into(),
                        state: "running".into(),
                        phase: "using_tool".into(),
                        current_tool: Some("bash".into()),
                        tool_use_count: 1,
                        input_tokens: 100,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        estimated_output_tokens: None,
                        output_tokens: 10,
                        reasoning_tokens: 0,
                        total_tokens: 110,
                        cost: None,
                        cost_microdollars: None,
                        elapsed_ms: 500,
                        failure_class: None,
                        failure_reason: None,
                        effective_tool_policy: octet_agent::SandboxConfig::new(".")
                            .effective_tool_policy(octet_agent::EffectPolicy::Controlled),
                        orchestration_provenance:
                            octet_agent::DelegationOrchestrationProvenance::all(
                                octet_agent::DelegationPolicySource::ParentInherited,
                            ),
                        session: None,
                    }],
                    total_cost_microdollars: None,
                    failure_reason: None,
                    failure_class: None,
                }),
                true,
            );
            shell.jump_to_tail();
            shell.render_written_test_frame();
            shell.scroll_lines(-1);
            let (column, row) = paint_bottom_button(&mut shell);
            let painted = shell
                .tui
                .as_ref()
                .unwrap()
                .rendered_frame()
                .iter()
                .map(|line| strip_terminal_sequences(line))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(painted.contains("Subagents"), "{painted}");
            assert_bottom_button_click(&mut shell, column, row);
        }
    }

    #[test]
    fn jump_to_bottom_click_with_threaded_write_overtaken_by_streaming() {
        let mut shell = bottom_button_shell(true, 1);
        shell.on_prompt_submitted("stream a response");
        shell.scroll_lines(-1);
        // Prepare a frame, then advance semantic state before its write receipt.
        shell.state.borrow_mut().new_output_count = 1000;
        shell.tui.as_mut().unwrap().request_render();
        shell.on_agent_event(&AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "delta during terminal write\n".into(),
        });
        shell.state.frame_written_at(None);
        let (column, row) = written_bottom_button(&shell);
        assert!(shell.state.borrow().painted_geometry.is_some());
        assert_bottom_button_click(&mut shell, column, row);
    }

    fn navigation_controls_shell(threaded: bool, streaming: bool) -> InteractiveShell {
        let mut shell = bottom_button_shell(threaded, 1);
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        if streaming {
            shell.on_agent_event(&AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "streaming response\n".into(),
            });
        }
        // Exercise successive completed renderer generations, not just startup.
        for _ in 0..3 {
            shell.render_written_test_frame();
        }
        shell
    }

    fn pending_navigation_output(shell: &mut InteractiveShell, streaming: bool) {
        if streaming {
            shell.on_agent_event(&AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "pending streamed output\n".into(),
            });
        } else {
            shell.notice("pending idle update");
        }
    }

    fn painted_bar(shell: &InteractiveShell) -> ScrollbarGeometry {
        let state = shell.state.borrow();
        let bar = state
            .transcript_navigation
            .borrow()
            .frame
            .unwrap()
            .scrollbar
            .unwrap();
        let row = &shell.tui.as_ref().unwrap().rendered_frame()[bar.thumb_top];
        assert_eq!(
            strip_terminal_sequences(&slice_by_column(row, usize::from(bar.column), 1, true)),
            "┃",
            "the hit target must be the written thumb"
        );
        bar
    }

    #[test]
    fn navigation_controls_scrollbar_hover_uses_painted_track() {
        for threaded in [false, true] {
            for streaming in [false, true] {
                let mut shell = navigation_controls_shell(threaded, streaming);
                let bar = painted_bar(&shell);
                pending_navigation_output(&mut shell, streaming);
                shell.intercept_transcript_input(&mouse(
                    MouseEventKind::Moved,
                    bar.column,
                    bar.thumb_top as u16,
                ));
                assert!(
                    shell.state.borrow().transcript_navigation.borrow().hover,
                    "hover ignored: threaded={threaded}, streaming={streaming}"
                );
                shell.render_written_test_frame();
                painted_bar(&shell); // Hover keeps the same thin glyph.
            }
        }
    }

    #[test]
    fn scrollbar_auto_preserves_card_fill_in_the_overlay_cell() {
        let mut shell = search_shell();
        shell.set_transcript_scrollbar(TranscriptScrollbar::Auto);
        let now = Instant::now();
        let state = shell.state.borrow();
        let width = state.size.0;
        let chrome = super::super::shell_chrome::shell_chrome(&state, width, now);
        let mut lines = vec![format!(
            "\x1b[48;2;12;34;56m{} \x1b[0m",
            "x".repeat(usize::from(width - 1))
        )];
        state.decorate_transcript_navigation(&mut lines, width, &chrome, now);
        let mut terminal = vt100::Parser::new(1, width, 0);
        terminal.process(lines[0].as_bytes());
        let edge = terminal.screen().cell(0, width - 1).unwrap();
        assert_eq!(edge.contents(), "│");
        assert_eq!(edge.bgcolor(), vt100::Color::Rgb(12, 34, 56));
    }

    #[test]
    fn scrollbar_thumb_press_does_not_quantize_the_viewport() {
        let mut shell = navigation_controls_shell(true, false);
        shell.scroll_lines(-1);
        shell.render_written_test_frame();
        let bar = painted_bar(&shell);
        let before = shell.state.borrow().scroll_from_bottom.get();
        let row = bar.thumb_top as u16;
        assert!(burst(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            bar.column,
            row
        ));
        assert!(burst(
            &mut shell,
            MouseEventKind::Up(MouseButton::Left),
            bar.column,
            row
        ));
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), before);
    }

    #[test]
    fn navigation_controls_scrollbar_click_uses_painted_track() {
        for threaded in [false, true] {
            for streaming in [false, true] {
                let mut shell = navigation_controls_shell(threaded, streaming);
                let bar = painted_bar(&shell);
                assert!(bar.thumb_top > 0);
                pending_navigation_output(&mut shell, streaming);
                assert!(
                    shell.intercept_transcript_input(&mouse(
                        MouseEventKind::Down(MouseButton::Left),
                        bar.column,
                        0,
                    )),
                    "click ignored: threaded={threaded}, streaming={streaming}"
                );
                let state = shell.state.borrow();
                let painted_maximum = state
                    .transcript_navigation
                    .borrow()
                    .frame
                    .unwrap()
                    .scrollbar
                    .unwrap()
                    .maximum;
                assert_eq!(
                    state.scroll_from_bottom.get(),
                    painted_maximum,
                    "top-of-track click: threaded={threaded}, streaming={streaming}, old maximum={}",
                    bar.maximum,
                );
                assert!(shell
                    .tui
                    .as_ref()
                    .unwrap()
                    .rendered_frame()
                    .iter()
                    .any(|row| { strip_terminal_sequences(row).contains("button history 00") }));
                assert!(state.pending_selection_anchor.is_none());
                assert!(!state.selection_dragging);
            }
        }
    }

    #[test]
    fn navigation_controls_scrollbar_drag_survives_pending_output() {
        for threaded in [false, true] {
            for streaming in [false, true] {
                for pending_press in [false, true] {
                    let mut shell = navigation_controls_shell(threaded, streaming);
                    let bar = painted_bar(&shell);
                    if pending_press {
                        pending_navigation_output(&mut shell, streaming);
                    }
                    assert!(shell.intercept_transcript_input(&mouse(
                        MouseEventKind::Down(MouseButton::Left),
                        bar.column,
                        bar.thumb_top as u16,
                    )));
                    pending_navigation_output(&mut shell, streaming);
                    assert!(
                        shell.intercept_transcript_input(&mouse(
                            MouseEventKind::Drag(MouseButton::Left),
                            bar.column,
                            0,
                        )),
                        "drag ignored: threaded={threaded}, streaming={streaming}"
                    );
                    assert!(shell.state.borrow().scroll_from_bottom.get() > 0);
                    pending_navigation_output(&mut shell, streaming);
                    assert!(shell.intercept_transcript_input(&mouse(
                        MouseEventKind::Drag(MouseButton::Left),
                        bar.column,
                        bar.rows as u16 + 2,
                    )));
                    assert!(shell.state.borrow().follow_tail);
                    assert!(shell.intercept_transcript_input(&mouse(
                        MouseEventKind::Up(MouseButton::Left),
                        bar.column,
                        bar.rows as u16 + 2,
                    )));
                    assert!(shell
                        .state
                        .borrow()
                        .transcript_navigation
                        .borrow()
                        .drag_offset
                        .is_none());
                }
            }
        }
    }

    #[test]
    fn navigation_controls_bottom_button_uses_painted_rectangle() {
        for threaded in [false, true] {
            for streaming in [false, true] {
                let mut shell = navigation_controls_shell(threaded, streaming);
                let (column, row) = written_bottom_button(&shell);
                pending_navigation_output(&mut shell, streaming);
                assert_bottom_button_click(&mut shell, column, row);
            }
        }
    }

    #[test]
    fn jump_to_bottom_button_limits_clicks_to_its_cells() {
        for threaded in [false, true] {
            for new in [0, 1, 2] {
                let label_width = if new == 0 { 20 } else { 28 };
                let left = (60 - label_width) / 2;
                let right = left + label_width;
                for column in [0, left - 1, right, 59] {
                    let mut shell = bottom_button_shell(threaded, new);
                    let row = bottom_button_row(&shell);
                    dispatch(
                        &mut shell,
                        mouse(MouseEventKind::Down(MouseButton::Left), column, row),
                    );
                    assert!(
                        !shell.state.borrow().follow_tail,
                        "outside column {column}, threaded={threaded}, new={new}"
                    );
                }
                // Include brackets, padding, arrow and every new-count cell.
                for column in left..right {
                    let mut shell = bottom_button_shell(threaded, new);
                    let row = bottom_button_row(&shell);
                    dispatch(
                        &mut shell,
                        mouse(MouseEventKind::Down(MouseButton::Left), column, row),
                    );
                    assert!(
                        shell.state.borrow().follow_tail,
                        "inside column {column}, threaded={threaded}, new={new}"
                    );
                    assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 0);
                    assert_eq!(shell.state.borrow().new_output_count, 0);
                }
            }
        }
    }

    #[test]
    fn jump_to_bottom_button_hover_and_accessible_profiles_keep_geometry() {
        use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
        for (interactive, unicode, color) in [
            (true, true, ColorDepth::TrueColor),
            (true, true, ColorDepth::Ansi16),
            (true, false, ColorDepth::None),
            (false, false, ColorDepth::None),
        ] {
            let mut shell = bottom_button_shell(false, 1);
            shell.set_theme(crate::tui::theme::test_theme_with(
                TerminalCapabilities::test(interactive, unicode, color),
            ));
            let before = frame(&shell, Instant::now());
            let row = bottom_button_row(&shell);
            let label = if unicode {
                "[ ↓ Jump to bottom · 1 new ]"
            } else {
                "[ v Jump to bottom - 1 new ]"
            };
            assert_eq!(
                strip_terminal_sequences(&before[usize::from(row)]),
                format!("{}{label}", " ".repeat(16))
            );
            dispatch(&mut shell, mouse(MouseEventKind::Moved, 17, row));
            assert!(
                shell
                    .state
                    .borrow()
                    .transcript_navigation
                    .borrow()
                    .jump_to_bottom_hover
            );
            let after = frame(&shell, Instant::now());
            assert_eq!(before.len(), after.len());
            assert_eq!(
                strip_terminal_sequences(&before[usize::from(row)]),
                strip_terminal_sequences(&after[usize::from(row)])
            );
            if color == ColorDepth::None {
                assert!(!after[usize::from(row)].contains('\x1b'));
            } else {
                let terminal =
                    super::super::surface_frame::emulate_row_for_test(&after[usize::from(row)], 60);
                assert!(terminal.screen().cell(0, 18).unwrap().underline());
                assert!(terminal.screen().cell(0, 18).unwrap().bold());
            }
            dispatch(&mut shell, mouse(MouseEventKind::Moved, 0, row));
            assert!(
                !shell
                    .state
                    .borrow()
                    .transcript_navigation
                    .borrow()
                    .jump_to_bottom_hover
            );
            shell.jump_to_tail();
            let live = frame(&shell, Instant::now());
            assert!(!live.iter().any(|line| line.contains("Jump to bottom")));
            assert_eq!(after.len(), live.len());
        }
        let mut shell = bottom_button_shell(false, 1);
        shell.set_size(22, 8);
        let narrow = frame(&shell, Instant::now());
        let row = bottom_button_row(&shell);
        assert_eq!(
            strip_terminal_sequences(&narrow[usize::from(row)]),
            " [ ↓ Jump to bottom ]"
        );
    }

    #[test]
    fn jump_to_bottom_button_preserves_search_keyboard_and_transcript_selection() {
        let mut shell = bottom_button_shell(false, 0);
        let rows = frame(&shell, Instant::now());
        let (row, column) = rows
            .iter()
            .enumerate()
            .skip(2)
            .find_map(|(row, line)| {
                let plain = strip_terminal_sequences(line);
                plain
                    .find("button history")
                    .map(|index| (row as u16, visible_width(&plain[..index]) as u16))
            })
            .unwrap();
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        );
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Up(MouseButton::Left), column + 10, row),
        );
        assert_eq!(
            shell.state.borrow().copy_buffer.as_deref(),
            Some("button his")
        );
        assert!(!shell.state.borrow().follow_tail);
        dispatch(&mut shell, key(KeyCode::PageDown, KeyModifiers::NONE));
        assert!(shell.state.borrow().follow_tail);
        shell.scroll_lines(-100);
        dispatch(&mut shell, key(KeyCode::End, KeyModifiers::CONTROL));
        assert!(shell.state.borrow().follow_tail);
        shell.scroll_lines(-100);
        shell.input_dispatch.bindings = KeybindingsManager::with_platform(
            "linux",
            false,
            std::collections::BTreeMap::from([("tui.altScreen.bottom".into(), vec!["end".into()])]),
        );
        dispatch(&mut shell, key(KeyCode::End, KeyModifiers::NONE));
        assert!(shell.state.borrow().follow_tail);

        for threaded in [false, true] {
            let mut shell = bottom_button_shell(threaded, 0);
            shell.open_transcript_search();
            if threaded {
                shell.render_written_test_frame();
            } else {
                frame(&shell, Instant::now());
            }
            let row = bottom_button_row(&shell);
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Down(MouseButton::Left), 0, row),
            );
            assert!(!shell.state.borrow().follow_tail);
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Down(MouseButton::Left), 30, row),
            );
            assert!(shell.state.borrow().follow_tail);
        }
    }

    #[test]
    fn jump_to_bottom_button_rejects_stale_painted_geometry() {
        let mut shell = bottom_button_shell(true, 0);
        let row = bottom_button_row(&shell);
        shell.set_size(62, 16);
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 30, row),
        );
        assert!(!shell.state.borrow().follow_tail);
        shell.render_written_test_frame();
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 30, row),
        );
        assert!(shell.state.borrow().follow_tail);
    }

    #[test]
    fn search_visibility_invalidates_painted_geometry() {
        let shell = InteractiveShell::test_shell();
        let state = shell.state.borrow();
        let capture = || {
            super::renderer_geometry::RenderedGeometry::capture(
                &state,
                super::renderer_geometry::GeometryFence::capture(&state),
            )
        };
        let closed = capture();
        state.transcript_navigation.borrow_mut().search = Some(TranscriptSearch::default());
        assert!(!closed.is_current(&state));
        let open = capture();
        state.transcript_navigation.borrow_mut().search = None;
        assert!(!open.is_current(&state));
    }

    #[test]
    fn unmodified_mouse_release_displacement_copies_without_a_motion_event() {
        for with_motion in [true, false] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(80, 20);
            shell.on_prompt_submitted("hello world");
            shell.apply_edit(EditAction::Paste("kept draft".into()));
            shell.state.borrow_mut().application_viewport_requested = true;
            let rows = frame(&shell, Instant::now());
            let (row, start) = rows
                .iter()
                .enumerate()
                .find_map(|(row, line)| {
                    let plain = strip_terminal_sequences(line);
                    plain
                        .find("hello world")
                        .map(|offset| (row as u16, plain[..offset].chars().count() as u16 + 1))
                })
                .expect("visible semantic transcript");
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Down(MouseButton::Left), start, row),
            );
            if with_motion {
                dispatch(
                    &mut shell,
                    mouse(MouseEventKind::Drag(MouseButton::Left), start + 2, row),
                );
            }
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Up(MouseButton::Left), start + 4, row),
            );
            assert_eq!(
                shell.state.borrow().copy_buffer.as_deref(),
                Some("ello"),
                "motion={with_motion}"
            );
            assert_eq!(shell.pending(), "kept draft");
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Down(MouseButton::Left), start, row),
            );
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Up(MouseButton::Left), start, row),
            );
            assert!(shell.state.borrow().transcript_selection.is_none());
        }
    }

    // Pi's fullscreen chat viewport defaults to "auto": the scrollbar appears
    // while scrolling and hides after a second.
    #[test]
    fn scrollbar_defaults_to_pis_transient_auto_mode() {
        assert_eq!(TranscriptScrollbar::default(), TranscriptScrollbar::Auto);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn dispatch(shell: &mut InteractiveShell, event: Event) -> InputAction {
        let action = shell.translate_input(Some(event), false);
        match &action {
            InputAction::Edit(edit) => shell.apply_edit(edit.clone()),
            InputAction::Scroll(direction) => shell.scroll(*direction),
            InputAction::ScrollLines(direction) => shell.scroll_lines(*direction),
            InputAction::JumpToTail => shell.jump_to_tail(),
            InputAction::Resize(width, height) => shell.set_size(*width, *height),
            InputAction::TranscriptPointer(PointerGesture::Begin { row, col, extend }) => {
                shell.begin_transcript_selection(*row, *col, *extend)
            }
            InputAction::TranscriptPointer(PointerGesture::Extend { row, col }) => {
                shell.extend_transcript_selection(*row, *col)
            }
            InputAction::TranscriptPointer(PointerGesture::End { row, col }) => {
                shell.end_transcript_selection(*row, *col)
            }
            _ => {}
        }
        action
    }

    fn frame(shell: &InteractiveShell, now: Instant) -> Vec<String> {
        let state = shell.state.borrow();
        super::super::viewport::render_shell_viewport_at(&state, state.size.0, now)
    }

    /// A pointer gesture must copy the row it is drawn on. Row geometry comes
    /// A pointer gesture must copy the row it is drawn on, in the viewport the
    /// reader is looking at. Markdown layout inserts rows the semantic copy text
    /// does not contain (a blank separator between a paragraph and the list that
    /// follows it) and wraps prose itself, so row hit testing has to read the
    /// painted rows rather than re-wrap the copy text.
    #[test]
    fn pointer_selection_copies_the_painted_row_in_wrapped_markdown() {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.capture_mouse = true;
        shell.state.borrow_mut().application_viewport_requested = true;
        shell.isolate_native_test_renderer();
        shell.on_prompt_submitted("numbered request");
        let mut text = String::from("U8-BEGIN\n");
        for index in 1..=40 {
            text.push_str(&format!(
                "{index}. U8-LONG-{index:03} numbered line with text\n"
            ));
        }
        text.push_str(&format!(
            "\n{}",
            "a long wrapped paragraph with unique trailing words "
        ));
        text.push_str(&"wrapped words ".repeat(30));
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(text),
            )));
        shell.apply_edit(EditAction::Paste("kept draft".into()));
        shell.render_written_test_frame();

        let mut checked = 0;
        let mut copied_rows = 0;
        for row in 0..24 {
            shell.render_written_test_frame();
            let visible = shell
                .state
                .borrow()
                .retained_render_geometry()
                .expect("painted geometry")
                .visible_lines
                .clone();
            let Some(line) = visible.get(row) else {
                break;
            };
            let painted = strip_terminal_sequences(line).trim_end().to_owned();
            if painted.trim().is_empty() {
                continue;
            }
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Down(MouseButton::Left), 0, row as u16),
            );
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Drag(MouseButton::Left), 79, row as u16),
            );
            dispatch(
                &mut shell,
                mouse(MouseEventKind::Up(MouseButton::Left), 79, row as u16),
            );
            let copied = shell
                .copy_selected_plain_text()
                .unwrap_or_default()
                .trim_end()
                .to_owned();
            if copied.is_empty() {
                continue;
            }
            copied_rows += 1;
            assert!(
                painted.contains(&copied),
                "screen row {row} painted {painted:?} but copied {copied:?}"
            );
            checked += 1;
        }
        assert!(checked > 10, "only {checked} rows carried copyable text");
        assert_eq!(copied_rows, checked);
        assert_eq!(shell.pending(), "kept draft");
    }

    #[test]
    fn pointer_selection_copies_the_painted_row_across_scroll_wrap_and_chrome() {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 18);
        shell.capture_mouse = true;
        shell.state.borrow_mut().application_viewport_requested = true;
        shell.isolate_native_test_renderer();
        for index in 0..240 {
            shell.notice(format!("probe row {index:03}"));
        }
        shell.notice(format!("wrapped {}", "wide content ".repeat(12)));
        shell.apply_edit(EditAction::Paste("kept draft".into()));
        let height = i16::try_from(shell.state.borrow().size.1).unwrap();
        shell.scroll_lines(-3 * height);
        shell.render_written_test_frame();

        let (visible, capacity) = {
            let state = shell.state.borrow();
            let geometry = state.retained_render_geometry().expect("painted geometry");
            (geometry.visible_lines.clone(), geometry.viewport_rows)
        };
        assert!(capacity > 6, "viewport too short for the probe");
        let (row, expected) = visible
            .iter()
            .enumerate()
            .take(capacity.saturating_sub(2))
            .skip(2)
            .find_map(|(row, line)| {
                let plain = strip_terminal_sequences(line);
                plain
                    .find("probe row ")
                    .map(|offset| (row, plain[offset..].trim_end().to_owned()))
            })
            .expect("a scrolled probe row in the painted viewport");
        let next = strip_terminal_sequences(&visible[row + 1])
            .trim_end()
            .to_owned();

        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 0, row as u16),
        );
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Drag(MouseButton::Left), 79, row as u16),
        );
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Up(MouseButton::Left), 79, row as u16),
        );
        let copied = shell.copy_selected_plain_text().expect("selection copy");
        assert_eq!(
            copied.trim_end(),
            expected.trim_end(),
            "screen row {row} painted {expected:?} and the next row painted {next:?}"
        );
        assert_eq!(shell.pending(), "kept draft");
    }

    fn search_shell() -> InteractiveShell {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 20);
        for index in 0..35 {
            shell.notice(format!("visible needle {index} and more text"));
        }
        shell.apply_edit(EditAction::Paste("preserved draft".into()));
        shell
    }

    fn open(shell: &mut InteractiveShell, query: &str) {
        shell.scroll_lines(i16::MIN);
        // Explicit map makes platform-default differences irrelevant here.
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("keybindings.json"),
            r#"{"tui.altScreen.search":"ctrl+f"}"#,
        )
        .unwrap();
        shell.input_dispatch.bindings =
            KeybindingsManager::create(directory.path(), "linux", false);
        assert_eq!(
            dispatch(shell, key(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            InputAction::Ignore
        );
        dispatch(shell, Event::Paste(query.into()));
        frame(shell, Instant::now());
    }

    fn search_stats(shell: &InteractiveShell) -> (usize, usize, usize, usize) {
        let state = shell.state.borrow();
        let nav = state.transcript_navigation.borrow();
        let search = nav.search.as_ref().unwrap();
        (
            search.cache.builds,
            search.cache.searches,
            search.cache.matches.len(),
            search.current,
        )
    }

    #[test]
    fn wrapped_error_preserves_live_search_with_working_chrome() {
        for (width, height) in [(42, 16), (24, 12), (24, 8)] {
            let mut shell = search_shell();
            shell.set_size(width, height);
            shell.begin_run("fixture");
            shell.error("long wrapped error ".repeat(100));
            open(&mut shell, "needle");
            let state = shell.state.borrow();
            let chrome = shell_chrome(&state, width, Instant::now());
            assert!(!chrome.working.is_empty(), "{width}x{height}");
            assert_eq!(
                chrome.panel.len(),
                2,
                "{width}x{height}: query and controls"
            );
            assert!(chrome.panel[0].contains(CURSOR_MARKER));
            assert!(strip_terminal_sequences(&chrome.panel[0]).contains("needle"));
            let rows =
                super::super::viewport::render_shell_viewport_at(&state, width, Instant::now());
            assert!(rows.len() <= usize::from(height), "{width}x{height}");
            assert_eq!(rows.join("\n").matches(CURSOR_MARKER).count(), 1);
        }
    }

    #[test]
    fn product_transcript_search_dispatch_styles_clicks_escape_and_preserves_draft() {
        let mut shell = search_shell();
        let cursor = shell.state.borrow().editor.cursor();
        let transcript = shell
            .state
            .borrow()
            .transcript
            .iter()
            .map(block_copy_text)
            .collect::<Vec<_>>();
        open(&mut shell, "NEEDLE");
        let rows = frame(&shell, Instant::now());
        assert_eq!(search_stats(&shell).2, 35);
        let theme = shell.theme();
        assert_ne!(
            theme.transcript_search_match("needle", true),
            theme.transcript_search_match("needle", false)
        );
        assert!(rows
            .iter()
            .any(|row| row.contains(&theme.transcript_search_match("needle", true))));
        assert_eq!(rows.join("\n").matches(CURSOR_MARKER).count(), 1);
        assert!(strip_terminal_sequences(&rows.join("\n")).contains("1/35"));
        assert!(!shell.extension_editor_snapshot().focused);
        assert_eq!(shell.pending(), "preserved draft");
        dispatch(&mut shell, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(search_stats(&shell).3, 1);
        let row = shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .frame
            .unwrap()
            .search_buttons_row
            .unwrap() as u16;
        dispatch(&mut shell, mouse(MouseEventKind::Moved, 1, row));
        assert_eq!(
            shell
                .state
                .borrow()
                .transcript_navigation
                .borrow()
                .search
                .as_ref()
                .unwrap()
                .hover,
            Some(SearchButton::Previous)
        );
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 1, row),
        );
        assert_eq!(search_stats(&shell).3, 0);
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, row),
        );
        assert_eq!(search_stats(&shell).3, 1);
        dispatch(&mut shell, key(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(search_stats(&shell).3, 0);
        assert_eq!(
            dispatch(&mut shell, key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            InputAction::Closed
        );
        dispatch(&mut shell, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!shell.transcript_search_active());
        assert!(shell.extension_editor_snapshot().focused);
        assert_eq!(shell.pending(), "preserved draft");
        assert_eq!(shell.state.borrow().editor.cursor(), cursor);
        assert_eq!(
            shell
                .state
                .borrow()
                .transcript
                .iter()
                .map(block_copy_text)
                .collect::<Vec<_>>(),
            transcript
        );
    }

    #[test]
    fn product_transcript_search_cache_invalidates_content_width_but_not_cursor_or_style() {
        let mut shell = search_shell();
        open(&mut shell, "needle");
        let initial = search_stats(&shell);
        frame(&shell, Instant::now());
        dispatch(&mut shell, key(KeyCode::Left, KeyModifiers::NONE));
        frame(&shell, Instant::now());
        assert_eq!(search_stats(&shell), initial);
        shell.notice("one additional needle");
        frame(&shell, Instant::now());
        let content = search_stats(&shell);
        assert!(content.0 > initial.0);
        assert_eq!(content.2, 36);
        shell.set_size(43, 20);
        frame(&shell, Instant::now());
        assert!(search_stats(&shell).0 > content.0);
        assert_eq!(search_stats(&shell).2, 36);
        // Same rendered content with different SGR, as during status shimmer,
        // updates the observed generation but reuses corpus and literal matches.
        let mut cache = SearchCache::default();
        cache.update(&["\x1b[31mneedle\x1b[0m".into()], 1, 80, 0, "needle");
        cache.update(&["\x1b[32mneedle\x1b[0m".into()], 2, 80, 0, "needle");
        assert_eq!((cache.builds, cache.searches), (1, 1));
    }

    #[test]
    fn product_transcript_search_bounded_paste_narrow_cursor_and_exclusive_focus() {
        let mut shell = search_shell();
        open(&mut shell, "界👩‍💻");
        shell.set_size(9, 16);
        let rows = frame(&shell, Instant::now());
        assert_eq!(rows.join("\n").matches(CURSOR_MARKER).count(), 1);
        assert!(rows.iter().all(|row| visible_width(row) <= 9));
        assert!(shell.intercept_transcript_input(&Event::Paste(format!(
            "\x1b]52;c;attack\x07{}",
            "界".repeat(1000)
        ))));
        let query = shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .search
            .as_ref()
            .unwrap()
            .editor
            .text()
            .to_owned();
        assert!(query.len() <= MAX_QUERY_BYTES);
        assert!(!query.contains('\x1b'));
        assert_eq!(shell.pending(), "preserved draft");
        shell.set_tool_input_prompt(Some("Secret: ".into()));
        assert!(!shell.transcript_search_active());
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .search
            .is_none());
        assert_eq!(
            dispatch(&mut shell, key(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            InputAction::Ignore
        );
        assert_eq!(shell.pending(), "preserved draft");
        shell.set_tool_input_prompt(None);
        open(&mut shell, "needle");
        shell.show_report_text("Report", "Owns focus", "body".into());
        assert!(!shell.transcript_search_active());
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .search
            .is_none());
    }

    #[test]
    fn search_literal_unicode_cross_row_mapping_and_visible_limits() {
        let mut cache = SearchCache::default();
        cache.update(
            &["α [x] 界 👩‍💻".into(), "continued".into()],
            1,
            40,
            0,
            "👩‍💻   continued",
        );
        assert_eq!(cache.matches.len(), 1);
        assert_eq!(cache.matches[0].len(), 2);
        assert_eq!(
            (cache.matches[0][0].start, cache.matches[0][0].end),
            (9, 11)
        );
        cache.update(&["α [x] 界 👩‍💻".into(), "continued".into()], 1, 40, 0, "[x]");
        assert_eq!(cache.matches.len(), 1);
        let lines = vec!["x ".repeat(MAX_MATCHES + 1)];
        cache.update(&lines, 2, 40, 0, "x");
        assert_eq!(cache.matches.len(), MAX_MATCHES);
        assert!(cache.matches_limited);
    }

    #[test]
    fn product_scrollbar_cycle_drag_hover_expiry_and_no_native_history_rewrite() {
        use sexy_tui_rs::Component;
        let mut shell = search_shell();
        let component = ShellComponent::new(shell.state.clone(), false);
        component.render(80);
        assert!(!shell.state.borrow().application_viewport_requested);
        let toggle = || {
            key(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )
        };
        dispatch(&mut shell, toggle());
        assert_eq!(
            shell.state.borrow().transcript_navigation.borrow().mode,
            TranscriptScrollbar::Auto
        );
        let update = component.render_update(80).unwrap();
        assert!(!update.rebuild_scrollback);
        assert!(update.reanchor_viewport);
        let now = Instant::now();
        frame(&shell, now);
        let bar = shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .frame
            .unwrap()
            .scrollbar
            .unwrap();
        assert!(bar.visible);
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), bar.column, 0),
        );
        assert!(!shell.state.borrow().follow_tail);
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_some());
        assert!(!shell.state.borrow().selection_dragging);
        frame(&shell, now);
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Drag(MouseButton::Left), 2, 19),
        );
        assert!(
            shell.state.borrow().follow_tail,
            "captured drag crosses composer and clamps at live tail"
        );
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Up(MouseButton::Left), 2, 19),
        );
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        frame(&shell, now);
        dispatch(&mut shell, mouse(MouseEventKind::Moved, bar.column, 0));
        assert!(shell.state.borrow().transcript_navigation.borrow().hover);
        frame(&shell, now + Duration::from_secs(10));
        assert!(
            shell
                .state
                .borrow()
                .transcript_navigation
                .borrow()
                .frame
                .unwrap()
                .scrollbar
                .unwrap()
                .visible
        );
        dispatch(&mut shell, mouse(MouseEventKind::Moved, 0, 19));
        shell
            .state
            .borrow()
            .expire_transcript_scrollbar(now + Duration::from_secs(10));
        frame(&shell, now + Duration::from_secs(10));
        assert!(
            !shell
                .state
                .borrow()
                .transcript_navigation
                .borrow()
                .frame
                .unwrap()
                .scrollbar
                .unwrap()
                .visible
        );
        dispatch(&mut shell, toggle());
        frame(&shell, now);
        assert_eq!(
            shell.state.borrow().transcript_cache.borrow().width,
            Some(79)
        );
        assert_eq!(
            shell.state.borrow().transcript_navigation.borrow().mode,
            TranscriptScrollbar::Always
        );
        dispatch(&mut shell, toggle());
        frame(&shell, now);
        assert_eq!(
            shell.state.borrow().transcript_cache.borrow().width,
            Some(80)
        );
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .frame
            .unwrap()
            .scrollbar
            .is_none());
        assert_eq!(shell.pending(), "preserved draft");
    }

    #[test]
    fn threaded_mouse_hit_testing_does_not_reborrow_navigation() {
        let mut shell = search_shell();
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        shell.isolate_native_test_renderer();
        shell.render_written_test_frame();

        let bar_column = {
            let state = shell.state.borrow();
            assert!(state.render_threaded);
            let geometry = state
                .retained_render_geometry()
                .expect("the threaded renderer published current geometry");
            let nav = state.transcript_navigation.borrow();
            let frame = nav.frame.expect("the threaded renderer published a frame");
            assert!(std::ptr::eq(
                state
                    .pointer_render_geometry()
                    .expect("painted pointer receipt"),
                geometry,
            ));
            frame
                .scrollbar
                .expect("always-visible scrollbar geometry")
                .column
        };

        dispatch(&mut shell, mouse(MouseEventKind::Moved, bar_column, 0));
        assert!(shell.state.borrow().transcript_navigation.borrow().hover);
    }

    #[test]
    fn product_scrollbar_focus_resize_and_stale_frame_do_not_steal_selection() {
        let mut shell = search_shell();
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        frame(&shell, Instant::now());
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 79, 1),
        );
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_some());
        dispatch(&mut shell, Event::FocusLost);
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        frame(&shell, Instant::now());
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), 79, 1),
        );
        shell.set_size(60, 16);
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .frame
            .is_none());
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        let before = shell.state.borrow().scroll_from_bottom.get();
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Drag(MouseButton::Left), 79, 15),
        );
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), before);
        frame(&shell, Instant::now());
        // An established transcript selection retains capture over the bar.
        shell.state.borrow_mut().selection_dragging = true;
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Drag(MouseButton::Left), 59, 1),
        );
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        assert_eq!(shell.pending(), "preserved draft");
    }

    #[test]
    fn product_search_and_scrollbar_no_color_copy_and_editor_page_actions() {
        use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
        let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
            true,
            false,
            ColorDepth::None,
        ));
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        for index in 0..50 {
            shell.notice(format!("needle {index}"));
        }
        shell.apply_edit(EditAction::Paste("draft".into()));
        dispatch(&mut shell, key(KeyCode::PageUp, KeyModifiers::CONTROL));
        assert!(shell.state.borrow().scroll_from_bottom.get() > 0);
        let scrolled = shell.state.borrow().scroll_from_bottom.get();
        dispatch(&mut shell, key(KeyCode::PageDown, KeyModifiers::CONTROL));
        assert!(shell.state.borrow().scroll_from_bottom.get() < scrolled);
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        open(&mut shell, "needle");
        let rendered = frame(&shell, Instant::now());
        assert!(!rendered
            .join("\n")
            .replace(CURSOR_MARKER, "")
            .contains('\x1b'));
        assert!(rendered.iter().any(|row| row.ends_with('#')));
        let state = shell.state.borrow();
        assert!(state
            .rendered_transcript(state.size.0)
            .iter()
            .all(|row| !row.contains("[<]")));
        assert_eq!(state.editor.text(), "draft");
        assert!(state
            .transcript
            .iter()
            .all(|block| !block_copy_text(block).contains('\x1b')));
    }
    /// One pointer event inside a burst: the whole gesture arrives before the
    /// renderer can publish another frame, so the input owner alone carries the
    /// capture from event to event.
    fn burst(shell: &mut InteractiveShell, kind: MouseEventKind, column: u16, row: u16) -> bool {
        shell.transcript_navigation_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// The painted scrollbar frame the input owner may hit-test.
    fn threaded_bar(shell: &InteractiveShell) -> (u16, usize, usize) {
        let state = shell.state.borrow();
        let frame = state
            .transcript_navigation
            .borrow()
            .frame
            .expect("painted navigation frame");
        let bar = frame.scrollbar.expect("always-visible scrollbar");
        assert!(state.retained_render_geometry().is_some());
        (bar.column, bar.rows, bar.maximum)
    }

    #[test]
    fn threaded_scrollbar_drag_keeps_capture_when_only_the_scroll_changed() {
        let mut shell = search_shell();
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        shell.isolate_native_test_renderer();
        shell.render_written_test_frame();
        let (column, rows, maximum) = threaded_bar(&shell);
        assert!(maximum > 0, "the fixture must scroll");
        // A whole gesture inside one terminal burst: no paint between events.
        assert!(burst(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            column,
            0
        ));
        assert!(!shell.state.borrow().follow_tail);
        let grab = shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .expect("the press captured the bar");
        assert!(burst(
            &mut shell,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            4
        ));
        assert_eq!(
            shell
                .state
                .borrow()
                .transcript_navigation
                .borrow()
                .drag_offset,
            Some(grab),
            "the scroll this drag caused must not release its capture"
        );
        let middle = shell.state.borrow().scroll_from_bottom.get();
        assert!(
            middle > 0 && middle < maximum,
            "middle {middle} of {maximum}"
        );
        assert!(burst(
            &mut shell,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            rows as u16 + 4
        ));
        assert!(
            shell.state.borrow().follow_tail,
            "a drag below the track clamps at the live tail"
        );
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_some());
        assert!(burst(
            &mut shell,
            MouseEventKind::Up(MouseButton::Left),
            column,
            rows as u16 + 4
        ));
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 0);
    }

    #[test]
    fn wheel_over_hint_and_edges_moves_content_while_scrollbar_is_captured() {
        let mut shell = search_shell();
        shell.state.borrow_mut().application_viewport_requested = true;
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        shell.isolate_native_test_renderer();
        shell.render_written_test_frame();
        let (column, rows, maximum) = threaded_bar(&shell);
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Down(MouseButton::Left), column, 4),
        );
        shell.render_written_test_frame();
        let initial = shell.state.borrow().scroll_from_bottom.get();
        assert!(
            initial > 3 && initial + 3 < maximum,
            "initial={initial}, maximum={maximum}"
        );
        let bottom = shell.state.borrow().size.1 - 1;
        for row in [0, rows as u16, bottom] {
            for col in [0, column] {
                for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
                    let before = shell.state.borrow().scroll_from_bottom.get();
                    let action = dispatch(&mut shell, mouse(kind, col, row));
                    assert!(
                        matches!(action, InputAction::ScrollLines(_)),
                        "wheel at ({col}, {row}) was swallowed: {action:?}"
                    );
                    shell.render_written_test_frame();
                    let after = shell.state.borrow().scroll_from_bottom.get();
                    assert_ne!(after, before, "wheel at ({col}, {row}) must move content");
                }
            }
        }
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), initial);
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_some());
        dispatch(
            &mut shell,
            mouse(MouseEventKind::Up(MouseButton::Left), column, 4),
        );
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
    }

    #[test]
    fn a_fresh_press_after_a_wheel_scroll_waits_for_its_own_frame() {
        let mut shell = search_shell();
        shell.isolate_native_test_renderer();
        shell.render_written_test_frame();
        // The wheel moves the view without a paint; a new press then names a
        // cell of a frame the input owner must not guess at.
        shell.scroll_lines(-4);
        assert!(shell.state.borrow().pointer_render_geometry().is_none());
        // Row 1 is a notice text row; row 2 is its non-selectable separator.
        shell.begin_transcript_selection(1, 4, false);
        {
            let state = shell.state.borrow();
            assert!(state.pending_selection_anchor.is_none());
            assert!(
                state.deferred_pointer_press.is_some(),
                "a fresh press waits for the frame that names its cell"
            );
        }
        shell.render_written_test_frame();
        let state = shell.state.borrow();
        assert!(state.deferred_pointer_press.is_none());
        assert!(state.pending_selection_anchor.is_some());
    }

    #[test]
    fn threaded_scrollbar_drag_cancels_on_a_genuine_layout_change() {
        let mut shell = search_shell();
        shell.set_transcript_scrollbar(TranscriptScrollbar::Always);
        shell.isolate_native_test_renderer();
        shell.render_written_test_frame();
        let (column, ..) = threaded_bar(&shell);
        assert!(burst(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            column,
            0
        ));
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_some());
        // A different geometry owns the rows now; the captured bar is gone.
        shell.set_size(60, 16);
        let before = shell.state.borrow().scroll_from_bottom.get();
        assert!(!burst(
            &mut shell,
            MouseEventKind::Drag(MouseButton::Left),
            59,
            1
        ));
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), before);

        // An overlay replaces the surface the gesture was captured on.
        shell.render_written_test_frame();
        let (column, ..) = threaded_bar(&shell);
        assert!(burst(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            column,
            0
        ));
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_some());
        shell.show_report_text("Report", "owns the surface", "body".into());
        let before = shell.state.borrow().scroll_from_bottom.get();
        assert!(!burst(
            &mut shell,
            MouseEventKind::Drag(MouseButton::Left),
            column,
            2
        ));
        assert!(shell
            .state
            .borrow()
            .transcript_navigation
            .borrow()
            .drag_offset
            .is_none());
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), before);
    }

    #[test]
    fn threaded_selection_edge_autoscroll_keeps_extending_without_a_repaint() {
        // One long answer owns every window row, so an edge cell always names
        // semantic text rather than a separator between short blocks.
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 20);
        shell.state.borrow_mut().application_viewport_requested = true;
        let mut text = String::new();
        for index in 0..200 {
            text.push_str(&format!("row {index:03} of a long answer\n"));
        }
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(text),
            )));
        shell.isolate_native_test_renderer();
        shell.render_written_test_frame();
        let capacity = {
            let state = shell.state.borrow();
            state
                .pointer_render_geometry()
                .expect("painted geometry")
                .viewport_rows
        };
        assert!(capacity > 4);
        // Press a content row in the middle of the window, then drag past the
        // top edge four times with no paint in between.
        let anchor = (capacity / 2..capacity)
            .find_map(|row| {
                shell.begin_transcript_selection(row as u16, 4, false);
                shell.state.borrow().pending_selection_anchor
            })
            .expect("a transcript row owns a semantic position");
        let start = shell.state.borrow().scroll_from_bottom.get();
        let mut scrolls = Vec::new();
        for _ in 0..4 {
            shell.extend_transcript_selection(0, 4);
            scrolls.push(shell.state.borrow().scroll_from_bottom.get());
        }
        let state = shell.state.borrow();
        assert!(state.selection_dragging);
        for pair in scrolls.windows(2) {
            assert!(
                pair[1] > pair[0],
                "autoscroll stalled after one step: {scrolls:?} from {start}"
            );
        }
        assert_eq!(scrolls[3], start + 8, "two rows per edge event");
        let focus = state
            .transcript_selection
            .as_ref()
            .expect("selection")
            .focus;
        let edge = InteractiveShell::transcript_position_at_gesture_cell(&state, 0, 4)
            .expect("the top row names content");
        assert_eq!(
            focus, edge,
            "the focus must follow the edge row, not stay at the anchor"
        );
        assert_ne!(focus, anchor);
    }
}
