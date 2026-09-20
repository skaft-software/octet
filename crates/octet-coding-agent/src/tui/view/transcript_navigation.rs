//! Application-owned transcript find/scroll controls. Search uses rendered,
//! sanitized rows, never raw protocol messages; decorations never enter copy.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use sexy_tui_rs::{slice_by_column, CURSOR_MARKER};
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
    #[default]
    Hidden,
    Auto,
    Always,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchSegment {
    row: usize,
    start: usize,
    end: usize,
}

struct CorpusSpan {
    start: usize,
    end: usize,
    segment: MatchSegment,
    linear: bool,
}

#[derive(Default)]
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
    cache: SearchCache,
    current: usize,
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
struct NavigationFrame {
    size: (u16, u16),
    generation: u64,
    scrollbar: Option<ScrollbarGeometry>,
    search_buttons_row: Option<usize>,
}

#[derive(Default)]
pub(super) struct TranscriptNavigation {
    search: Option<TranscriptSearch>,
    mode: TranscriptScrollbar,
    hover: bool,
    drag_offset: Option<usize>,
    hide_at: Option<Instant>,
    frame: Option<NavigationFrame>,
}

impl TranscriptNavigation {
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
        if !self.transcript_search_active() {
            return;
        }
        let lines = self.rendered_transcript(width);
        let cache = self.transcript_cache.borrow();
        let mut nav = self.transcript_navigation.borrow_mut();
        let search = nav.search.as_mut().expect("active search");
        search.cache.update(
            &lines,
            cache.generation,
            cache.width.unwrap_or(width),
            cache.last_update_start,
            search.editor.text(),
        );
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
            let thumb_rows = (rows.saturating_mul(rows) / length.max(rows)).clamp(1, rows);
            let thumb_top = maximum
                .saturating_sub(scroll)
                .saturating_mul(rows - thumb_rows)
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
        if let Some(bar) = scrollbar.filter(|bar| bar.visible) {
            lines.resize_with(lines.len().max(bar.rows), String::new);
            for (row, line) in lines.iter_mut().take(bar.rows).enumerate() {
                let prefix = slice_by_column(line, 0, usize::from(bar.column), true);
                let padding = usize::from(bar.column).saturating_sub(visible_width(&prefix));
                let thumb = row >= bar.thumb_top && row < bar.thumb_top + bar.thumb_rows;
                let glyph = match (thumb, self.theme.unicode()) {
                    (true, true) => "█",
                    (true, false) => "#",
                    (false, true) => "│",
                    (false, false) => ".",
                };
                let painted = self.theme.fg(if thumb { "accent" } else { "muted" }, glyph);
                let reset =
                    if self.theme.capabilities().color == crate::tui::terminal::ColorDepth::None {
                        ""
                    } else {
                        "\x1b[0m"
                    };
                *line = format!("{prefix}{reset}{}{painted}", " ".repeat(padding));
            }
        }
        nav.frame = Some(NavigationFrame {
            size: self.size,
            generation: self.transcript_cache.borrow().generation,
            scrollbar,
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
        let chrome = shell_chrome(&state, state.size.0, Instant::now());
        let length = state.rendered_transcript(state.size.0).len();
        let scroll = resolved_scroll_from_bottom(&state, length, chrome.transcript_rows);
        let anchor_row =
            length
                .saturating_sub(scroll)
                .saturating_sub(transcript_viewport_capacity(
                    chrome.transcript_rows,
                    scroll > 0,
                ));
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
        if matches!(
            event,
            Event::Resize(..) | Event::FocusLost | Event::FocusGained
        ) {
            self.state
                .borrow()
                .transcript_navigation
                .borrow_mut()
                .reset_pointer();
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
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    let latest = {
                        let state = self.state.borrow();
                        let chrome = shell_chrome(&state, state.size.0, Instant::now());
                        state.scroll_from_bottom.get() > 0
                            && usize::from(mouse.row)
                                == transcript_viewport_capacity(chrome.transcript_rows, true)
                            && mouse.column < state.size.0
                    };
                    if latest {
                        self.jump_to_tail();
                        self.render();
                        return true;
                    }
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let step = if mouse.modifiers.contains(KeyModifiers::ALT) {
                            15
                        } else {
                            3
                        };
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
        let now = Instant::now();
        let mut state = self.state.borrow_mut();
        if !state.application_viewport_requested {
            return false;
        }
        let mut nav = state.transcript_navigation.borrow_mut();
        let frame = nav.frame.filter(|frame| {
            frame.size == state.size && {
                let cache = state.transcript_cache.borrow();
                !cache.dirty && frame.generation == cache.generation
            }
        });
        let Some(frame) = frame else {
            nav.reset_pointer();
            return false;
        };
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
        if starting {
            let row = usize::from(mouse.row);
            nav.drag_offset = Some(
                if row >= bar.thumb_top && row < bar.thumb_top + bar.thumb_rows {
                    row - bar.thumb_top
                } else {
                    bar.thumb_rows / 2
                },
            );
        }
        if let Some(grab) = nav.drag_offset {
            if starting
                || matches!(
                    mouse.kind,
                    MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                )
            {
                let travel = bar.rows.saturating_sub(bar.thumb_rows);
                let offset = usize::from(mouse.row).saturating_sub(grab).min(travel);
                let top = (offset.saturating_mul(bar.maximum) + travel / 2)
                    .checked_div(travel)
                    .unwrap_or(0);
                nav.activity(now);
                drop(nav);
                state.pending_selection_anchor = None;
                state.selection_dragging = false;
                state.transcript_selection = None;
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
}
