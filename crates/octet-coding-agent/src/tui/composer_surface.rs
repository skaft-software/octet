//! Composer surface: an inset multiline input area framed by stable
//! model-adaptive rules, with a calm semantic status footer below.
//!
//! This surface owns rendering, cache invalidation, geometry, and footer layout
//! only. Attachment admission/composition live in `tui::composer`; popup
//! selection/event loops and model/session/extension/subagent flows remain with
//! the view and picker owners.

use crate::tui::composer::mouse::{ComposerMouseGeometry, ComposerMouseRow};
use std::borrow::Cow;
use std::sync::OnceLock;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

use sexy_tui_rs::{visible_width, TextEditAction, TextEditor, TextEditorProjection, CURSOR_MARKER};

use crate::presentation::compact_context_limit;
use crate::tui::layout::PresentationLayout;
use crate::tui::theme_schema::{StatusFooterLayout, WorkingIndicatorPlacement};
use crate::tui::view::ShellState;

mod status_footer;
use crate::tui::view::{fit_line, footer_width, EditorDisplayMap, FooterSegment};
pub(super) use status_footer::{workspace_branch, FooterUsage};

/// Keep one physical cell after every laid-out text row for the terminal cursor.
const CURSOR_CELL_RESERVATION: usize = 1;

fn composer_cursor_marker(state: &super::view::ShellState) -> &'static str {
    if state.panel.is_some() || state.transcript_search_active() {
        ""
    } else {
        CURSOR_MARKER
    }
}

/// Cache key for one octet-owned composer source. The generic editor's revision
/// lets the shell reuse a transformed layout without hashing a full draft on
/// every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerEditorSource {
    Draft(u64),
    ToolPrompt(u64),
}

/// One transformed display layout for the current source and text-cell width.
pub(crate) struct ComposerEditorCache {
    // Text identity and width own the expensive display-map/wrap work. Cursor
    // motion updates only the structured projection over those shared rows.
    source: ComposerEditorSource,
    source_cursor: usize,
    text_width: usize,
    projection: ComposerEditorProjection,
}

impl ComposerEditorCache {
    #[must_use]
    pub(crate) fn new(
        source: ComposerEditorSource,
        text: &str,
        cursor: usize,
        text_width: usize,
    ) -> Self {
        Self {
            source,
            source_cursor: cursor,
            text_width,
            projection: ComposerEditorProjection::new(text, cursor, text_width),
        }
    }

    #[must_use]
    pub(crate) fn matches(&self, source: ComposerEditorSource, text_width: usize) -> bool {
        self.source == source && self.text_width == text_width
    }

    pub(crate) fn refresh_cursor(&mut self, source_cursor: usize) {
        if self.source_cursor != source_cursor {
            self.source_cursor = source_cursor;
            self.projection.set_source_cursor(source_cursor);
        }
    }

    #[must_use]
    pub(crate) fn projection(&self) -> &ComposerEditorProjection {
        &self.projection
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn source(&self) -> ComposerEditorSource {
        self.source
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn text_width(&self) -> usize {
        self.text_width
    }
}

/// A safe, lazily materialized display projection for the composer.
///
/// Sanitization and source/display mapping remain octet policy. Visual wrapping,
/// cursor coordinates, and visual navigation come from the generic editor.
pub(crate) struct ComposerEditorProjection {
    display: EditorDisplayMap,
    projection: TextEditorProjection,
}

impl ComposerEditorProjection {
    pub(crate) fn new(source: &str, source_cursor: usize, text_width: usize) -> Self {
        let display = EditorDisplayMap::from_source(source);
        let display_cursor = display.source_to_display(source_cursor);
        let projection =
            TextEditor::projection_for(display.layout_text(), display_cursor, text_width);
        Self {
            display,
            projection,
        }
    }

    fn set_source_cursor(&mut self, source_cursor: usize) {
        let display_cursor = self.display.source_to_display(source_cursor);
        let layout = self.projection.layout().clone();
        self.projection =
            TextEditor::projection_for_layout(self.display.layout_text(), &layout, display_cursor);
    }

    #[must_use]
    pub(crate) fn line_count(&self) -> usize {
        self.projection.lines().len()
    }

    #[must_use]
    pub(crate) fn cursor_row(&self) -> usize {
        self.projection.cursor_row()
    }

    #[must_use]
    pub(crate) fn visible_row(
        &self,
        row: usize,
        cursor_marker: &str,
        max_text_width: usize,
    ) -> String {
        let Some(line) = self.projection.lines().get(row) else {
            return String::new();
        };
        let cursor = (row == self.projection.cursor_row() && !cursor_marker.is_empty())
            .then(|| self.projection.cursor().offset());
        self.display.terminal_row_bounded(
            line.start(),
            line.visible_end(),
            cursor,
            cursor_marker,
            max_text_width,
        )
    }

    #[must_use]
    pub(crate) fn visual_source_target(
        &self,
        action: &TextEditAction,
        preferred_column: &mut Option<usize>,
    ) -> Option<usize> {
        self.projection
            .visual_target(self.display.layout_text(), action, preferred_column)
            .map(|display| self.display.display_to_source(display))
    }

    fn mouse_row(
        &self,
        row: usize,
        screen_row: usize,
        column: usize,
        width: usize,
    ) -> ComposerMouseRow {
        let line = &self.projection.lines()[row];
        let text = self.display.layout_text();
        let mut cells = Vec::with_capacity(width);
        let mut last = line.start();
        let mut end = line.start();
        for (relative, grapheme) in text[line.start()..line.visible_end()].grapheme_indices(true) {
            let offset = line.start() + relative;
            let size = sexy_tui_rs::WidthPolicy::default().grapheme_width(grapheme, cells.len());
            if cells.len() + size > width {
                break;
            }
            last = offset;
            cells.extend(std::iter::repeat_n(
                self.display.display_to_source(offset),
                size,
            ));
            end = offset + grapheme.len();
        }
        // Pi compatibility: packages/tui/src/components/editor.ts:655-698.
        // A soft-wrapped segment cannot place the cursor on the following row.
        let wrapped = self
            .projection
            .lines()
            .get(row + 1)
            .is_some_and(|next| next.start() == line.end());
        ComposerMouseRow {
            row: screen_row,
            column,
            cells,
            end: self.display.display_to_source(end),
            click_end: self
                .display
                .display_to_source(if wrapped { last } else { end }),
        }
    }

    #[must_use]
    pub(crate) fn layout_text(&self) -> &str {
        self.display.layout_text()
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Internal text rows: starts at one line, grows with content up to a cap.
/// When the editor has more lines than the cap, an overflow indicator is shown.
///
/// `visual_lines` is the number of wrapped editor lines that would be needed
/// at the current terminal width, so long wrapping lines are counted properly.
pub fn composer_content_rows(terminal_rows: u16, visual_lines: usize) -> usize {
    crate::tui::layout::composer_content_rows(terminal_rows, visual_lines)
}

// ---------------------------------------------------------------------------
// Bordered composer box
// ---------------------------------------------------------------------------

fn horiz(state: &super::view::ShellState) -> &str {
    state.theme.glyph("horizontal")
}

fn overflow_label(theme: &super::theme::OctetTheme, count: usize, above: bool) -> String {
    let arrow = match (theme.unicode(), above) {
        (true, true) => "↑",
        (true, false) => "↓",
        (false, true) => "^",
        (false, false) => "v",
    };
    format!("{arrow} {count} more")
}

fn overflow_rule(
    theme: &super::theme::OctetTheme,
    paint: impl Fn(&str) -> String,
    width: usize,
    count: usize,
    above: bool,
) -> String {
    let horizontal = theme.glyph("horizontal");
    if count == 0 {
        return paint(&horizontal.repeat(width));
    }
    let label = overflow_label(theme, count, above);
    let label = if visible_width(&label) + 2 <= width {
        format!(" {label} ")
    } else {
        let short = label.trim_end_matches(" more");
        fit_line(short, width as u16)
    };
    let remaining = width.saturating_sub(visible_width(&label));
    let left = remaining / 2;
    format!(
        "{}{}{}",
        paint(&horizontal.repeat(left)),
        theme.fg("dim", &label),
        paint(&horizontal.repeat(remaining - left)),
    )
}

/// Only a native two-rule editor can host the ordinary Working indicator.
/// Other editors, temporary input and narrow panes retain their own status slot.
pub(super) fn embedded_working(state: &ShellState, width: u16) -> Option<&'static str> {
    if width < 12
        || state.theme.layout().working_indicator != WorkingIndicatorPlacement::Composer
        || composer_chrome(&state.theme) != ComposerChrome::Boxed
    {
        return None;
    }
    state.working_rule_spinner()
}

pub(crate) enum WorkingRuleContent<'a> {
    Label(&'a str),
    Custom(Cow<'a, str>),
}

fn paint_composer_rule(state: &ShellState, text: &str) -> String {
    let theme = &state.theme;
    if let Some(border) = theme.role_rgb("composer_border") {
        return theme.rgb_fg(border, text);
    }
    if theme.layout().composer_effort_color {
        let role = match state.reasoning.split_whitespace().next() {
            Some("minimal") => "extension.pi.thinkingMinimal",
            Some("low") => "extension.pi.thinkingLow",
            Some("medium") => "extension.pi.thinkingMedium",
            Some("high") => "extension.pi.thinkingHigh",
            Some("xhigh" | "ultra") => "extension.pi.thinkingXhigh",
            Some("max") => "extension.pi.thinkingMax",
            _ => "extension.pi.thinkingOff",
        };
        if theme.has_semantic_style(role) {
            return theme.apply_semantic_role(role, text);
        }
    }
    theme.rgb_fg(
        theme.model_rgb(state.model_lab).unwrap_or((128, 128, 128)),
        text,
    )
}

fn working_rule(state: &ShellState, width: usize, hidden: usize, spinner: &str) -> String {
    let theme = &state.theme;
    let dash = horiz(state);
    let paint = |text: &str| paint_composer_rule(state, text);
    let Some(content) = state.working_rule_content() else {
        return overflow_rule(theme, paint, width, hidden, true);
    };
    let overflow = (hidden > 0).then(|| format!(" {} ", overflow_label(theme, hidden, true)));
    let left_width = overflow.as_ref().map_or(width, |label| {
        width.saturating_sub(visible_width(label)) / 2
    });
    let label_budget = left_width.saturating_sub(4);
    let prefix = match content {
        WorkingRuleContent::Label(label) => {
            let full = format!(
                "{spinner} {}",
                crate::tui::view::sanitize_for_terminal(label)
            );
            (visible_width(&full) <= label_budget)
                .then(|| paint(&format!("{} {full} ", dash.repeat(2))))
        }
        WorkingRuleContent::Custom(row) if label_budget > 0 => {
            // This is validated extension ANSI, not a label for the host to
            // recolor. Clip it by cells, then fence its styles from the rule.
            let no_color = theme.capabilities().color == super::terminal::ColorDepth::None;
            let row = if no_color {
                Cow::Owned(sexy_tui_rs::strip_terminal_sequences(&row))
            } else {
                row
            };
            let clipped = sexy_tui_rs::truncate_to_width(&row, label_budget, Some(""));
            let reset = if no_color { "" } else { "\x1b[0m" };
            Some(format!(
                "{}{clipped}{reset} ",
                paint(&format!("{} ", dash.repeat(2)))
            ))
        }
        WorkingRuleContent::Custom(_) => None,
    };
    let prefix = match prefix {
        Some(prefix) => prefix,
        None if visible_width(spinner) < left_width => paint(&format!(
            "{}{spinner}",
            dash.repeat(2.min(left_width - visible_width(spinner)))
        )),
        None => return overflow_rule(theme, paint, width, hidden, true),
    };
    let mut line = format!(
        "{prefix}{}",
        paint(&dash.repeat(left_width - visible_width(&prefix)))
    );
    if let Some(label) = overflow {
        let right = width - left_width - visible_width(&label);
        line.push_str(&paint(&label));
        line.push_str(&paint(&dash.repeat(right)));
    }
    line
}

fn render_topline(state: &super::view::ShellState, width: u16, now: Instant) -> String {
    let theme = &state.theme;
    let border = theme
        .role_rgb("composer_border")
        .unwrap_or_else(|| theme.model_rgb(state.model_lab).unwrap_or((128, 128, 128)));
    let available = usize::from(width).saturating_sub(3);
    // Fit the footer into the space between the rule ends first. Clipping a
    // full-width footer afterward would chop the workspace's rightmost tail.
    let metadata = render_status_footer_with_gap(state, available as u16, now, Some(border))
        .0
        .trim()
        .to_owned();
    let remainder = usize::from(width).saturating_sub(visible_width(&metadata) + 3);
    fit_line(
        &format!(
            "{} {metadata} {}",
            theme.rgb_fg(border, horiz(state)),
            theme.rgb_fg(border, &horiz(state).repeat(remainder))
        ),
        width,
    )
}

// ---------------------------------------------------------------------------
// Unified composer frame
// ---------------------------------------------------------------------------

/// Composer chrome selected by the theme's open `composer` token.
/// `boxed` (default): full-width top/bottom rules, no corners.
/// `framed`: a cornered box (Claude Code / pi style) coloured by
/// `composer_border` (token reference; falls back to the next model's accent).
/// `shaded`: no rules at all — a rectangle painted with `composer_bg`
/// (codex style). Unresolvable backgrounds degrade to plain rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ComposerChrome {
    Boxed,
    Framed,
    Shaded,
    Topline,
}

fn composer_chrome(theme: &super::theme::OctetTheme) -> ComposerChrome {
    match theme
        .resolve::<String>("composer")
        .as_deref()
        .map(str::trim)
    {
        Some("framed") => ComposerChrome::Framed,
        Some("shaded") => ComposerChrome::Shaded,
        Some("topline") => ComposerChrome::Topline,
        _ => ComposerChrome::Boxed,
    }
}

/// Width available to composer content rows for the active chrome. Framed
/// chrome spends two border columns and one space of padding per side;
/// shaded chrome keeps one painted margin column per side.
fn composer_inner_width(theme: &super::theme::OctetTheme, frame_width: u16) -> u16 {
    match composer_chrome(theme) {
        ComposerChrome::Boxed | ComposerChrome::Topline => frame_width,
        ComposerChrome::Framed => frame_width.saturating_sub(4),
        ComposerChrome::Shaded => frame_width.saturating_sub(2),
    }
    .max(1)
}

/// One chrome-aware geometry decision for editor movement, sizing, layout, and
/// rendering. The prompt glyph, its gap, continuation indentation, and one
/// spare hardware-cursor cell are always reserved, even while a panel
/// suppresses the marker, so focus changes never reflow a draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ComposerEditorGeometry {
    inner_width: usize,
    text_width: usize,
}

impl ComposerEditorGeometry {
    #[must_use]
    pub(crate) fn inner_width(self) -> usize {
        self.inner_width
    }

    #[must_use]
    pub(crate) fn text_width(self) -> usize {
        self.text_width
    }
}

#[must_use]
pub(crate) fn composer_editor_geometry(
    state: &super::view::ShellState,
    width: u16,
) -> ComposerEditorGeometry {
    const PROMPT_GAP: usize = 1;
    const CONTINUATION_PREFIX: usize = 2;

    let presentation = PresentationLayout::new(&state.theme, width);
    let compact = usize::from(width) < 12;
    let inner_width = if compact {
        usize::from(presentation.content_width)
    } else {
        usize::from(composer_inner_width(
            &state.theme,
            presentation.content_width,
        ))
    };
    let text_prefix = if state.theme.layout().composer_show_prompt {
        (visible_width(state.theme.glyph("prompt")) + PROMPT_GAP).max(CONTINUATION_PREFIX)
    } else {
        0
    };
    let text_width = inner_width
        .saturating_sub(text_prefix.saturating_add(CURSOR_CELL_RESERVATION))
        .max(1);
    ComposerEditorGeometry {
        inner_width,
        text_width,
    }
}

/// Text cells a specific painted row can materialize without consuming the
/// hardware-cursor reservation. The generic layout keeps a minimum width of
/// one cell, while this may intentionally be zero on a three-column terminal.
fn materialized_text_width(geometry: ComposerEditorGeometry, prefix: &str) -> usize {
    geometry.text_width().min(
        geometry
            .inner_width()
            .saturating_sub(visible_width(prefix).saturating_add(CURSOR_CELL_RESERVATION)),
    )
}

fn render_editor_row(
    state: &super::view::ShellState,
    editor: &ComposerEditorProjection,
    row: usize,
    cursor_marker: &str,
    width: usize,
) -> String {
    let Some((_, selection)) = state
        .composer_selection
        .as_ref()
        .filter(|(revision, _)| *revision == state.editor.text_revision())
    else {
        return editor.visible_row(row, cursor_marker, width);
    };
    let line = &editor.projection.lines()[row];
    let start = editor
        .display
        .source_to_display(selection.start)
        .max(line.start());
    let end = editor
        .display
        .source_to_display(selection.end)
        .min(line.visible_end());
    if start >= end {
        return editor.visible_row(row, cursor_marker, width);
    }
    let policy = sexy_tui_rs::WidthPolicy::default();
    let text = editor.display.layout_text();
    let left = policy.line_width(&text[line.start()..start]).min(width);
    let right = policy.line_width(&text[line.start()..end]).min(width);
    let plain = editor.visible_row(row, "", width);
    let slice = |text: &str, from, count| sexy_tui_rs::slice_by_column(text, from, count, true);
    let selected = format!(
        "{}\x1b[7m{}\x1b[27m{}",
        slice(&plain, 0, left),
        slice(&plain, left, right - left),
        slice(&plain, right, width)
    );
    if row == editor.cursor_row() && !cursor_marker.is_empty() {
        let column = editor.projection.cursor().column().min(width);
        format!(
            "{}{cursor_marker}{}",
            slice(&selected, 0, column),
            slice(&selected, column, width)
        )
    } else {
        selected
    }
}

fn editor_window(
    editor: &ComposerEditorProjection,
    content_rows: usize,
    ruled: bool,
) -> std::ops::Range<usize> {
    let total = editor.line_count();
    let visible = if !ruled && total > content_rows {
        content_rows.saturating_sub(1).max(1)
    } else {
        content_rows.max(1)
    }
    .min(total);
    let start = editor
        .cursor_row()
        .saturating_add(1)
        .saturating_sub(visible);
    start..(start + visible).min(total)
}

/// Capture the exact native composer window from the same projection as paint.
/// The caller supplies its screen origin after laying out the surrounding chrome.
pub(crate) fn composer_mouse_geometry(
    state: &super::view::ShellState,
    top: usize,
    height: usize,
) -> ComposerMouseGeometry {
    let width = state.size.0;
    let geometry = composer_editor_geometry(state, width);
    let editor = state.composer_editor_projection(geometry);
    let content_rows = composer_content_rows(state.size.1, editor.line_count().max(1));
    let chrome = composer_chrome(&state.theme);
    let compact = width < 12;
    let ruled = compact || matches!(chrome, ComposerChrome::Boxed | ComposerChrome::Framed);
    let window = editor_window(&editor, content_rows, ruled);
    let overflow = !ruled && window.start > 0;
    let first_row =
        top + usize::from(!compact || chrome == ComposerChrome::Topline) + usize::from(overflow);
    let inset = usize::from(PresentationLayout::new(&state.theme, width).inset);
    let cushion = if compact {
        0
    } else {
        match chrome {
            ComposerChrome::Framed => 2,
            ComposerChrome::Shaded => 1,
            _ => 0,
        }
    };
    let mut rows = Vec::new();
    if width >= 3 {
        for (screen, visual) in window.enumerate() {
            let prefix = if !state.theme.layout().composer_show_prompt {
                String::new()
            } else if visual == 0 {
                format!("{} ", state.theme.glyph("prompt"))
            } else {
                "  ".into()
            };
            let row = first_row + screen;
            if row >= top + height {
                break;
            }
            rows.push(editor.mouse_row(
                visual,
                row,
                inset + cushion + visible_width(&prefix),
                materialized_text_width(geometry, &prefix),
            ));
        }
    }
    ComposerMouseGeometry { top, height, rows }
}

/// Render composer content inside the shared horizontal inset. Plain boxed
/// rules span the terminal, while explicit framed/shaded theme chrome stays
/// on the content grid. Selected prompt text copies without chrome. Rules
/// retain stable colour and geometry; overflow counts label the rules. Live motion belongs
/// to the semantic transcript entry doing the work.
fn render_composer_box(
    state: &super::view::ShellState,
    width: u16,
    _now: Instant,
    content_rows: usize,
    geometry: ComposerEditorGeometry,
    editor: &ComposerEditorProjection,
) -> Vec<String> {
    let terminal_width = usize::from(width);
    if terminal_width < 4 {
        return render_plain_content(state, width, geometry, editor);
    }

    let theme = &state.theme;
    let layout = PresentationLayout::new(theme, width);
    let inset = usize::from(layout.inset);
    let frame_width = usize::from(layout.content_width);
    let frame_prefix = " ".repeat(inset);
    let chrome = composer_chrome(theme);
    let inner_width = geometry.inner_width();

    let paint_rule = |text: &str| paint_composer_rule(state, text);
    // `composer_bg` fills the shaded rectangle and a framed box; a frame
    // without the token stays border-only.
    let frame_bg = matches!(chrome, ComposerChrome::Shaded | ComposerChrome::Framed)
        .then(|| theme.role_rgb("composer_bg"))
        .flatten();
    let ruled = matches!(chrome, ComposerChrome::Boxed | ComposerChrome::Framed);
    let total_lines = editor.line_count();
    let window = editor_window(editor, content_rows, ruled);
    let start = window.start;
    let end = window.end;
    let hidden_above = start;
    let hidden_below = total_lines.saturating_sub(end);
    let render_rule = |count, above| overflow_rule(theme, paint_rule, terminal_width, count, above);
    let frame_rule = |left: &str, right: &str, count, above| -> String {
        let rule = format!(
            "{}{}{}",
            paint_rule(left),
            overflow_rule(
                theme,
                paint_rule,
                frame_width.saturating_sub(2),
                count,
                above
            ),
            paint_rule(right),
        );
        match frame_bg {
            Some(bg) => format!("{frame_prefix}{}", theme.paint_row_background(bg, &rule)),
            None => format!("{frame_prefix}{rule}"),
        }
    };
    // A theme that caps its reading column would otherwise float a narrow
    // shaded composer in the middle of the terminal, with unpainted columns
    // beside it. The fill reaches the terminal edges instead, while the draft
    // stays inside the column. A theme that only insets its content keeps the
    // historical inset rectangle; `surface_bleed = false` keeps the fill
    // inside the reading column too.
    let bleeds = layout.capped && theme.resolve::<bool>("surface_bleed").unwrap_or(true);
    // One padding cell inside the fill, on both sides of the draft.
    //
    // A capped theme keeps its draft on the reading column, so the column offset
    // is part of the painted row and the fill spans the whole terminal. An
    // uncapped theme keeps the historical inset rectangle, where the offset
    // stays outside the fill.
    let paint_shaded = |lead: &str, row: &str, fill_width: usize| -> String {
        let painted = format!("{lead} {row} ");
        let padding = fill_width.saturating_sub(visible_width(&painted));
        let padded = format!("{painted}{}", " ".repeat(padding));
        match frame_bg {
            Some(bg) => theme.paint_row_background(bg, &padded),
            None => padded,
        }
    };
    let finish_row = |row: String| -> String {
        match chrome {
            ComposerChrome::Boxed | ComposerChrome::Topline => {
                fit_line(&format!("{frame_prefix}{row}"), width)
            }
            ComposerChrome::Framed => {
                let vertical = paint_rule(theme.glyph("vertical"));
                let frame = format!("{vertical} {row} {vertical}");
                match frame_bg {
                    Some(bg) => {
                        let filled = theme.paint_row_background(bg, &frame);
                        fit_line(&format!("{frame_prefix}{filled}"), width)
                    }
                    None => fit_line(&format!("{frame_prefix}{frame}"), width),
                }
            }
            ComposerChrome::Shaded if bleeds => {
                fit_line(&paint_shaded(&frame_prefix, &row, terminal_width), width)
            }
            ComposerChrome::Shaded => fit_line(
                &format!("{frame_prefix}{}", paint_shaded("", &row, frame_width)),
                width,
            ),
        }
    };
    let blank_shaded_row = || -> String {
        if bleeds {
            paint_shaded(&frame_prefix, "", terminal_width)
        } else {
            format!("{frame_prefix}{}", paint_shaded("", "", frame_width))
        }
    };

    let mut lines = Vec::with_capacity(content_rows + 2);
    match chrome {
        ComposerChrome::Boxed => lines.push(match embedded_working(state, width) {
            Some(spinner) => working_rule(state, terminal_width, hidden_above, spinner),
            None => render_rule(hidden_above, true),
        }),
        ComposerChrome::Topline => lines.push(render_topline(state, width, _now)),
        ComposerChrome::Framed => lines.push(frame_rule(
            theme.glyph("top_left"),
            theme.glyph("top_right"),
            hidden_above,
            true,
        )),
        ComposerChrome::Shaded => lines.push(blank_shaded_row()),
    }

    let cursor_marker = composer_cursor_marker(state);
    let render_row = |content: &str| -> String {
        let content_width = visible_width(content);
        if content_width > inner_width {
            fit_line(content, inner_width as u16)
        } else {
            format!(
                "{content}{}",
                " ".repeat(inner_width.saturating_sub(content_width))
            )
        }
    };

    if editor.layout_text().is_empty() {
        // A theme's `composer_placeholder` paints a muted hint on an empty
        // draft; it is paint-only and never enters the draft.
        let placeholder = theme
            .resolve::<String>("composer_placeholder")
            .filter(|hint| !hint.trim().is_empty())
            .map(|hint| theme.fg("muted", hint.trim()));
        for index in 0..content_rows {
            if index == 0 {
                let row = match &placeholder {
                    Some(hint) => format!("{}{cursor_marker} {hint}", composer_prefix(state, true)),
                    None => format!("{}{cursor_marker}", composer_prefix(state, true)),
                };
                lines.push(finish_row(render_row(&row)));
            } else {
                lines.push(finish_row(render_row("")));
            }
        }
    } else {
        let mut rendered = Vec::with_capacity(content_rows);
        if !ruled && hidden_above > 0 {
            let mut label = overflow_label(theme, hidden_above, true);
            if hidden_below > 0 {
                label.push_str(&format!(
                    " / {}",
                    overflow_label(theme, hidden_below, false)
                ));
            }
            rendered.push(render_row(&theme.fg("dim", &label)));
        }

        for index in start..end {
            let prefix = composer_prefix(state, index == 0);
            let row = render_editor_row(
                state,
                editor,
                index,
                cursor_marker,
                materialized_text_width(geometry, &prefix),
            );
            rendered.push(render_row(&format!("{prefix}{row}")));
        }

        if !ruled && hidden_above == 0 && hidden_below > 0 {
            rendered.push(render_row(
                &theme.fg("dim", &overflow_label(theme, hidden_below, false)),
            ));
        }

        while rendered.len() < content_rows {
            rendered.push(render_row(""));
        }
        lines.extend(rendered.into_iter().map(&finish_row));
    }

    match chrome {
        ComposerChrome::Boxed => lines.push(render_rule(hidden_below, false)),
        ComposerChrome::Topline => {}
        ComposerChrome::Framed => lines.push(frame_rule(
            theme.glyph("bottom_left"),
            theme.glyph("bottom_right"),
            hidden_below,
            false,
        )),
        ComposerChrome::Shaded => lines.push(blank_shaded_row()),
    }

    lines
}

// ---------------------------------------------------------------------------
// Plain content fallback (very narrow terminals)
// ---------------------------------------------------------------------------

fn composer_prefix(state: &ShellState, first: bool) -> String {
    if !state.theme.layout().composer_show_prompt {
        String::new()
    } else if first {
        let theme = &state.theme;
        format!(
            "{} ",
            theme.bold(&theme.model_fg(state.model_lab, theme.glyph("prompt")))
        )
    } else {
        "  ".into()
    }
}

fn render_plain_content(
    state: &super::view::ShellState,
    width: u16,
    geometry: ComposerEditorGeometry,
    editor: &ComposerEditorProjection,
) -> Vec<String> {
    let prefix = composer_prefix(state, true);
    let cursor_marker = composer_cursor_marker(state);
    if editor.layout_text().is_empty() {
        return vec![fit_line(
            &format!("{cursor_marker}{}", prefix.trim_end()),
            width,
        )];
    }
    let row = render_editor_row(
        state,
        editor,
        editor.cursor_row(),
        cursor_marker,
        materialized_text_width(geometry, &prefix),
    );
    vec![fit_line(&format!("{prefix}{row}"), width)]
}

// ---------------------------------------------------------------------------
// Status footer (below the composer box)
// ---------------------------------------------------------------------------

/// Semantic footer group. Variants are ordered from most descriptive to
/// most compact; groups disappear as units rather than being byte-truncated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FooterKind {
    Identity,
    Context,
    Cost,
}

/// The composer retains its domain role beside the shared, production footer
/// segment primitive. Ordinary action footers and status chrome therefore use
/// the same width and complete-segment vocabulary without coupling their data.
struct StatusFooterSegment {
    kind: FooterKind,
    segment: FooterSegment,
}

impl StatusFooterSegment {
    fn new(kind: FooterKind, variants: Vec<String>) -> Self {
        Self {
            kind,
            segment: FooterSegment::variants(variants),
        }
    }
}

fn status_footer_width(segments: &[StatusFooterSegment], gap: usize) -> usize {
    let segments = segments
        .iter()
        .filter(|segment| segment.segment.is_visible())
        .map(|segment| segment.segment.clone())
        .collect::<Vec<_>>();
    footer_width(&segments, gap)
}

fn hide_footer_kind(segments: &mut [StatusFooterSegment], kind: FooterKind) {
    if let Some(segment) = segments.iter_mut().find(|segment| segment.kind == kind) {
        segment.segment.hide();
    }
}

fn compact_footer_kind(segments: &mut [StatusFooterSegment], kind: FooterKind) {
    if let Some(segment) = segments.iter_mut().find(|segment| segment.kind == kind) {
        segment.segment.compact_once();
    }
}

pub(crate) fn format_microdollars(microdollars: u64) -> String {
    const MICRODOLLARS_PER_DOLLAR: u128 = 1_000_000;
    const SIGNIFICANT_FIGURES: i32 = 3;

    let microdollars = u128::from(microdollars);
    if microdollars == 0 {
        return "$0.000".to_owned();
    }

    let whole = microdollars / MICRODOLLARS_PER_DOLLAR;
    let exponent = if whole > 0 {
        whole.ilog10() as i32
    } else {
        microdollars.ilog10() as i32 - 6
    };
    let decimal_places = SIGNIFICANT_FIGURES - 1 - exponent;
    let rounding_power = 6 - decimal_places;
    let rounding_unit = if rounding_power > 0 {
        10u128.pow(rounding_power as u32)
    } else {
        1
    };
    let quotient = microdollars / rounding_unit;
    let remainder = microdollars % rounding_unit;
    let rounded = (quotient
        + if remainder >= rounding_unit.div_ceil(2) {
            1
        } else {
            0
        })
        * rounding_unit;

    // A carry can change the number of digits, e.g. 9.999 becomes 10.0.
    let rounded_whole = rounded / MICRODOLLARS_PER_DOLLAR;
    let rounded_exponent = if rounded_whole > 0 {
        rounded_whole.ilog10() as i32
    } else {
        rounded.ilog10() as i32 - 6
    };
    let decimal_places = SIGNIFICANT_FIGURES - 1 - rounded_exponent;
    if decimal_places <= 0 {
        return format!("${rounded_whole}");
    }

    let decimal_places = decimal_places as u32;
    let fraction = rounded % MICRODOLLARS_PER_DOLLAR;
    let fraction = if decimal_places <= 6 {
        fraction / 10u128.pow(6 - decimal_places)
    } else {
        fraction * 10u128.pow(decimal_places - 6)
    };
    let mut fraction = format!("{fraction:0width$}", width = decimal_places as usize);
    if rounded_whole == 0 {
        while fraction.len() > 3 && fraction.ends_with('0') {
            fraction.pop();
        }
        while fraction.len() < 3 {
            fraction.push('0');
        }
    }
    format!("${rounded_whole}.{fraction}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FooterControl {
    Model,
    Thinking,
    Context,
}

/// Painted columns on the last row of the stock composer (the status row).
#[derive(Clone, Debug)]
pub(crate) struct FooterSpan {
    pub(crate) control: FooterControl,
    pub(crate) columns: std::ops::Range<usize>,
}

impl FooterSpan {
    fn new(control: FooterControl, start: usize, end: usize) -> Self {
        Self {
            control,
            columns: start..end,
        }
    }
}

/// Present the host workspace without filesystem lookups. Clip from the left
/// so a long path retains its most specific tail, without splitting graphemes.
fn footer_home() -> Option<&'static std::path::Path> {
    static HOME: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| dirs::home_dir().map(|home| home.canonicalize().unwrap_or(home)))
        .as_deref()
}

fn footer_workspace(
    path: &std::path::Path,
    home: Option<&std::path::Path>,
    width: usize,
    unicode: bool,
) -> String {
    if width < 4 {
        return String::new();
    }
    let display = home
        .and_then(|home| path.strip_prefix(home).ok())
        .map_or_else(
            || crate::presentation::tool_display::human_path(path),
            |relative| {
                if relative.as_os_str().is_empty() {
                    "~".to_owned()
                } else {
                    let relative = relative.display().to_string();
                    #[cfg(windows)]
                    let relative = relative.replace('\\', "/");
                    format!("~/{relative}")
                }
            },
        );
    let display = super::view::sanitize_ordinary_surface_cell(&display, unicode);
    let cells = visible_width(&display);
    if cells <= width {
        return display;
    }
    let ellipsis = if unicode { "…" } else { "..." };
    let tail_width = width - visible_width(ellipsis);
    let tail = sexy_tui_rs::slice_by_column(&display, cells - tail_width, tail_width, true);
    format!("{ellipsis}{tail}")
}

fn context_percent(used: u64, limit: u64) -> u64 {
    if limit == 0 {
        return 0;
    }
    let rounded = (u128::from(used) * 100 + u128::from(limit) / 2) / u128::from(limit);
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

/// Render one calm, width-aware status row. Detailed cache and input/output
/// accounting stays in `/status`, `/cost`, and `/cache`; default chrome keeps
/// identity, context pressure and durable session spend together on the left,
/// with the workspace right-aligned when space permits.
#[cfg(test)]
fn render_status_footer(state: &super::view::ShellState, width: u16, now: Instant) -> String {
    render_status_footer_with_gap(state, width, now, None).0
}

fn render_status_footer_with_gap(
    state: &super::view::ShellState,
    width: u16,
    _now: Instant,
    gap_rule: Option<(u8, u8, u8)>,
) -> (String, Vec<FooterSpan>) {
    let theme_layout = state.theme.layout_for_width(width);
    let total_width = usize::from(width);
    if total_width == 0 {
        return (String::new(), Vec::new());
    }
    // The footer is metadata, so it begins on the shared primary-text column
    // rather than on the full-width surface edge used by rules and cards.
    // Keep this inset independent from the composer frame itself.
    let requested_inset = 1usize.saturating_add(usize::from(theme_layout.composer_padding));
    // The topline already supplies the rule's own one-cell inset. Do not
    // charge the footer's separate inset again when fitting embedded metadata.
    let left_inset = if gap_rule.is_some() {
        0
    } else if width >= 5 {
        requested_inset
            .max(usize::from(
                PresentationLayout::new(&state.theme, width).inset,
            ))
            .min(total_width.saturating_sub(1) / 2)
    } else {
        0
    };
    let available = total_width.saturating_sub(left_inset.saturating_mul(2));
    let separator = super::view::semantic_separator(&state.theme);
    let gap = visible_width(separator);
    let active = state.run.current().is_some_and(|run| run.is_active());

    // Active runs retain the identity captured at submission. An idle footer
    // immediately reflects the selected model without recolouring the chrome.
    let full_model = if active {
        state
            .run_model_display
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| {
                if state.model_display.trim().is_empty() {
                    state.model.as_str()
                } else {
                    state.model_display.as_str()
                }
            })
            .to_owned()
    } else if !state.model_display.trim().is_empty() {
        state.model_display.clone()
    } else if !state.model.trim().is_empty() {
        state.model.clone()
    } else {
        state.theme.glyph("wordmark").to_owned()
    };
    let effort = if active {
        state.run_reasoning.as_deref().unwrap_or(&state.reasoning)
    } else {
        &state.reasoning
    }
    .trim();
    // Pi compatibility: footer.ts:220-255 keeps the full ID, dropping only
    // the optional provider when space is tight. Themes share this policy.
    let mut variants = Vec::with_capacity(3);
    if !effort.is_empty() && (gap_rule.is_none() || !effort.eq_ignore_ascii_case("off")) {
        variants.push(format!("{full_model}{separator}{effort}"));
    }
    variants.push(full_model.clone());
    let provider = if active {
        state.run_provider.as_deref().unwrap_or(&state.provider)
    } else {
        &state.provider
    };
    let provider_prefix = if state.model_labels.provider_count() > 1 && !provider.is_empty() {
        let prefix = format!("({provider}) ");
        variants.insert(0, format!("{prefix}{}", variants[0]));
        prefix
    } else {
        String::new()
    };
    let mut segments = vec![StatusFooterSegment::new(FooterKind::Identity, variants)];

    let base_context = if active {
        state.run_context_estimate
    } else {
        state.context_estimate
    };
    let displayed_context = base_context.map(|(used, limit)| {
        (
            used.saturating_add(if active {
                state.live_generated_tokens().unwrap_or_default()
            } else {
                0
            }),
            limit,
        )
    });
    if let Some((used, limit)) = displayed_context.filter(|(_, limit)| *limit > 0) {
        let estimated = active && state.turn_generation_started_at.is_some();
        let marker = if estimated { "~" } else { "" };
        let percent = context_percent(used, limit);
        let limit_display = compact_context_limit(limit);
        segments.push(StatusFooterSegment::new(
            FooterKind::Context,
            vec![
                format!("{marker}{percent}%/{limit_display}"),
                format!("{marker}{percent}%"),
            ],
        ));
    }

    let price_display = if active {
        state.run_price_display.unwrap_or(state.price_display)
    } else {
        state.price_display
    };
    let cost = if let Some(cost) = state.displayed_session_cost_microdollars() {
        Some(format!(
            "{}{}",
            format_microdollars(cost),
            state.session_cost_suffix(cost)
        ))
    } else {
        match price_display {
            crate::presentation::PriceDisplay::ExplicitZero => Some("$0".to_owned()),
            crate::presentation::PriceDisplay::Unknown
            | crate::presentation::PriceDisplay::Priced => None,
        }
    };
    // Plain dollars, nothing else: the coding agent provides the estimate and
    // the provider API is the source of truth. `usage_uncertain` stays a durable
    // state fact (surfaced by `/telemetry` and the non-TUI channels) and never
    // rewrites the footer cost as a subtotal or a `+ ?` marker.
    if let Some(cost) = cost {
        segments.push(StatusFooterSegment::new(FooterKind::Cost, vec![cost]));
    }

    if !theme_layout.show_footer || theme_layout.show_header {
        hide_footer_kind(&mut segments, FooterKind::Identity);
    }
    if !theme_layout.show_status_line {
        hide_footer_kind(&mut segments, FooterKind::Context);
        hide_footer_kind(&mut segments, FooterKind::Cost);
    }

    // An ID that cannot fit even on its own must not crowd out every other
    // fact. Keep the complete-label policy and retain context/cost instead.
    if visible_width(&full_model) > available {
        hide_footer_kind(&mut segments, FooterKind::Identity);
    }
    if status_footer_width(&segments, gap) > available {
        compact_footer_kind(&mut segments, FooterKind::Identity);
    }
    if status_footer_width(&segments, gap) > available {
        compact_footer_kind(&mut segments, FooterKind::Context);
    }
    if status_footer_width(&segments, gap) > available {
        hide_footer_kind(&mut segments, FooterKind::Cost);
    }
    while status_footer_width(&segments, gap) > available {
        let before = status_footer_width(&segments, gap);
        compact_footer_kind(&mut segments, FooterKind::Identity);
        if status_footer_width(&segments, gap) == before {
            break;
        }
    }
    if status_footer_width(&segments, gap) > available {
        hide_footer_kind(&mut segments, FooterKind::Context);
    }
    if status_footer_width(&segments, gap) > available {
        hide_footer_kind(&mut segments, FooterKind::Identity);
    }

    let context_is_urgent = displayed_context
        .is_some_and(|(used, limit)| limit > 0 && u128::from(used) * 100 >= u128::from(limit) * 90);
    let style_segment = |segment: &StatusFooterSegment| match segment.kind {
        FooterKind::Identity => state.theme.fg("foreground", segment.segment.text()),
        FooterKind::Context if context_is_urgent => state.theme.fg("error", segment.segment.text()),
        FooterKind::Cost
            if state
                .session_cost_microdollars
                .zip(state.max_session_cost_microdollars)
                .is_some_and(|(cost, limit)| limit > 0 && cost >= limit.saturating_mul(9) / 10) =>
        {
            state.theme.fg("error", segment.segment.text())
        }
        FooterKind::Cost
            if state
                .session_cost_microdollars
                .zip(state.max_session_cost_microdollars)
                .is_some_and(|(cost, limit)| limit > 0 && cost >= limit / 2) =>
        {
            state.theme.fg("warning", segment.segment.text())
        }
        _ => state.theme.fg("muted", segment.segment.text()),
    };

    let mut left_text = String::new();
    let mut column = left_inset;
    let mut spans = Vec::new();
    for segment in segments
        .iter()
        .filter(|segment| segment.segment.is_visible())
    {
        if !left_text.is_empty() {
            left_text.push_str(&state.theme.fg("muted", separator));
            column += gap;
        }
        let text = segment.segment.text();
        let end = column + visible_width(text);
        if gap_rule.is_none() {
            match segment.kind {
                FooterKind::Identity => {
                    let model_only = text == full_model
                        || text.strip_prefix(&provider_prefix) == Some(&full_model);
                    let model_end = if model_only {
                        end
                    } else {
                        let effort_start = end - visible_width(effort);
                        spans.push(FooterSpan::new(FooterControl::Thinking, effort_start, end));
                        effort_start - gap
                    };
                    spans.push(FooterSpan::new(FooterControl::Model, column, model_end));
                }
                FooterKind::Context => {
                    spans.push(FooterSpan::new(FooterControl::Context, column, end))
                }
                FooterKind::Cost => {}
            }
        }
        left_text.push_str(&style_segment(segment));
        column = end;
    }
    let left_width = column - left_inset;
    // The directory is secondary: shorten or omit it before compromising any
    // left-hand metadata. Keep the same two-cell right inset as the composer.
    let cwd = if !left_text.is_empty() && theme_layout.show_footer && !theme_layout.show_header {
        state.workspace.as_deref().map_or_else(String::new, |path| {
            // Workspace roots are canonicalized by the CLI. Resolve the same
            // spelling for the platform home (including Windows verbatim paths
            // and Unix symlinks), once rather than doing I/O on every frame.
            footer_workspace(
                path,
                footer_home(),
                available.saturating_sub(left_width + 2),
                state.theme.unicode(),
            )
        })
    } else {
        String::new()
    };
    let body = if cwd.is_empty() {
        left_text
    } else {
        let spacing = available.saturating_sub(left_width + visible_width(&cwd));
        let gap = gap_rule.map_or_else(
            || " ".repeat(spacing),
            |color| {
                if spacing < 2 {
                    " ".repeat(spacing)
                } else {
                    format!(
                        " {} ",
                        state.theme.rgb_fg(color, &horiz(state).repeat(spacing - 2))
                    )
                }
            },
        );
        format!("{left_text}{gap}{}", state.theme.fg("muted", &cwd))
    };
    (
        fit_line(&format!("{}{body}", " ".repeat(left_inset)), width),
        spans,
    )
}

pub(crate) fn status_footer_rows(state: &ShellState, width: u16) -> usize {
    // Pi setFooter replaces the stock footer rather than stacking another one.
    // Retirement clears the contribution, restoring ordinary status chrome.
    if state.extension_footer_replaces_status() {
        return 0;
    }
    let layout = state.theme.layout_for_width(width);
    let has_identity = layout.show_footer && !layout.show_header;
    if (!has_identity && !layout.show_status_line)
        || composer_chrome(&state.theme) == ComposerChrome::Topline
    {
        return 0;
    }
    1 + usize::from(uses_two_line_footer(state) && status_footer::show_workspace(state, width))
}

pub(super) fn uses_two_line_footer(state: &ShellState) -> bool {
    state.theme.layout().status_footer == StatusFooterLayout::TwoLine
}

/// Render the full composer surface: a top rule, content rows, a bottom rule,
/// then a status footer line.
pub fn render_composer_surface(state: &ShellState, width: u16, now: Instant) -> Vec<String> {
    render_composer_with_footer(state, width, now).0
}

pub(crate) fn render_composer_with_footer(
    state: &ShellState,
    width: u16,
    now: Instant,
) -> (Vec<String>, Vec<FooterSpan>) {
    let mut lines = render_composer_body(state, width, now);
    let spans = if status_footer_rows(state, width) == 0 {
        Vec::new()
    } else if uses_two_line_footer(state) {
        let (rows, spans) = status_footer::render(state, width);
        lines.extend(rows);
        spans
    } else {
        let (row, spans) = render_status_footer_with_gap(state, width, now, None);
        lines.push(row);
        spans
    };
    (lines, spans)
}

fn render_composer_body(state: &ShellState, width: u16, now: Instant) -> Vec<String> {
    let w = usize::from(width);
    let geometry = composer_editor_geometry(state, width);

    if w < 3 {
        // There is no room for both prompt chrome and draft text. Put the
        // trusted cursor token first so the terminal backend can still place
        // its hardware cursor on a non-empty focused draft at widths 1 and 2.
        let cursor_marker = composer_cursor_marker(state);
        return if cursor_marker.is_empty() || !state.theme.layout().composer_show_prompt {
            let editor = state.composer_editor_projection(geometry);
            render_plain_content(state, width, geometry, &editor)
        } else {
            let prompt = state.theme.bold(
                &state
                    .theme
                    .model_fg(state.model_lab, state.theme.glyph("prompt")),
            );
            vec![fit_line(&format!("{cursor_marker}{prompt}"), width)]
        };
    }

    // One borrowed projection drives height calculation and painting. This
    // avoids a second cache lookup/projection refresh for every normal frame.
    let editor = state.composer_editor_projection(geometry);
    let visual_lines = editor.line_count().max(1);
    let content_rows = composer_content_rows(state.size.1, visual_lines);

    if w < 12 {
        return render_compact(state, width, now, content_rows, geometry, &editor);
    }

    // Unified inset frame with stable octet-owned focus rules.
    render_composer_box(state, width, now, content_rows, geometry, &editor)
}

/// Narrow-terminal fallback: no box, just model line + prompt.
fn render_compact(
    state: &super::view::ShellState,
    width: u16,
    now: Instant,
    content_rows: usize,
    geometry: ComposerEditorGeometry,
    editor: &ComposerEditorProjection,
) -> Vec<String> {
    let mut lines = Vec::new();
    if composer_chrome(&state.theme) == ComposerChrome::Topline {
        lines.push(render_topline(state, width, now));
    }
    let plan = PresentationLayout::new(&state.theme, width);
    let padding_width = usize::from(plan.inset);
    let padding = " ".repeat(padding_width);

    // Prompt. The single status row is appended below it, matching the boxed
    // composer geometry used at ordinary widths.
    let cursor_marker = composer_cursor_marker(state);

    if editor.layout_text().is_empty() {
        lines.push(fit_line(
            &format!("{padding}{}{cursor_marker}", composer_prefix(state, true)),
            width,
        ));
        return lines;
    }

    let total_lines = editor.line_count();
    let visible_rows = content_rows.max(1).min(total_lines);
    let mut start = editor
        .cursor_row()
        .saturating_add(1)
        .saturating_sub(visible_rows);
    let end = (start + visible_rows).min(total_lines);
    if end.saturating_sub(start) < visible_rows {
        start = end.saturating_sub(visible_rows);
    }

    for index in start..end {
        let content_prefix = composer_prefix(state, index == 0);
        let prefix = format!("{padding}{content_prefix}");
        let row = render_editor_row(
            state,
            editor,
            index,
            cursor_marker,
            materialized_text_width(geometry, &content_prefix),
        );
        lines.push(fit_line(&format!("{prefix}{row}"), width));
    }
    lines
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Columns of one rendered row carrying the shaded fill.
    fn shaded_columns(row: &str, width: u16) -> Vec<u16> {
        let mut terminal = vt100::Parser::new(1, width, 0);
        terminal.process(row.as_bytes());
        (0..width)
            .filter(|column| {
                !matches!(
                    terminal.screen().cell(0, *column).expect("cell").bgcolor(),
                    vt100::Color::Default
                )
            })
            .collect()
    }

    #[test]
    fn built_in_still_composer_uses_full_width_with_left_aligned_draft() {
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme_from_source(include_str!(
            "../../../../examples/themes/Still.toml"
        ));
        state.model_display = "Test Model".into();
        for width in [48, 160] {
            let rows = render_composer_surface(&state, width, Instant::now());
            let fill = shaded_columns(&rows[0], width);
            assert_eq!(
                fill.len(),
                usize::from(width),
                "Still composer fill at {width}"
            );
            assert_eq!((fill.first(), fill.last()), (Some(&0), Some(&(width - 1))));
            let prompt = sexy_tui_rs::strip_terminal_sequences(&rows[1]);
            assert_eq!(
                prompt.find(state.theme.glyph("prompt")),
                Some(1),
                "{width}: {prompt:?}"
            );
        }
    }

    #[test]
    fn framed_composer_fills_its_frame_when_a_bg_is_set() {
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme_from_source(
            "[colors]\ncomposer = \"framed\"\ncomposer_bg = \"#24252a\"\n[layout]\ntranscript_inset = 6\ncomposer_padding = 1",
        );
        state.model_display = "Test Model".into();
        let width = 48;
        let rows = render_composer_surface(&state, width, Instant::now());
        assert!(rows.len() >= 3, "{rows:?}");
        let plain_top = sexy_tui_rs::strip_terminal_sequences(&rows[0]).to_owned();
        assert!(
            plain_top.contains(state.theme.glyph("top_left")),
            "framed top border missing: {plain_top:?}"
        );
        let painted = shaded_columns(&rows[1], width);
        assert!(
            !painted.is_empty(),
            "framed composer must fill its interior: {:?}",
            rows[1]
        );
        assert!(
            !painted.contains(&0),
            "columns left of the frame must stay unpainted: {:?}",
            rows[1]
        );
        let mut plain = crate::tui::view::ShellState::default();
        plain.theme = crate::tui::theme::test_theme_from_source(
            "[colors]\ncomposer = \"framed\"\n[layout]\ntranscript_inset = 6\ncomposer_padding = 1",
        );
        plain.model_display = "Test Model".into();
        let rows = render_composer_surface(&plain, width, Instant::now());
        assert!(
            shaded_columns(&rows[1], width).is_empty(),
            "border-only frame grew a fill: {:?}",
            rows[1]
        );
    }

    #[test]
    fn composer_placeholder_hints_an_empty_draft_and_never_enters_it() {
        let hint = "Message octet — @ to include context, / for commands";
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme_from_source(&format!(
            "[colors]\ncomposer_placeholder = \"{hint}\""
        ));
        state.model_display = "Test Model".into();
        let rows = render_composer_surface(&state, 80, Instant::now());
        let plain = sexy_tui_rs::strip_terminal_sequences(&rows[1]).to_owned();
        assert!(plain.contains(hint), "{plain:?}");
        assert!(
            state.editor.text().is_empty(),
            "the placeholder is paint-only and must not enter the draft"
        );
        state.editor.set_text("hello");
        let rows = render_composer_surface(&state, 80, Instant::now());
        let plain = sexy_tui_rs::strip_terminal_sequences(&rows[1]).to_owned();
        assert!(plain.contains("hello"), "{plain:?}");
        assert!(!plain.contains(hint), "{plain:?}");
        let mut bare = crate::tui::view::ShellState::default();
        bare.model_display = "Test Model".into();
        let rows = render_composer_surface(&bare, 80, Instant::now());
        let plain = sexy_tui_rs::strip_terminal_sequences(&rows[1]).to_owned();
        assert!(!plain.contains("Message octet"), "{plain:?}");
    }

    #[test]
    fn shaded_composer_fills_the_terminal_while_the_draft_holds_the_reading_column() {
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme_from_source(
            "[colors]\ncomposer = \"shaded\"\ncomposer_bg = \"#202020\"\ncontent_max_width = 112\n[layout]\ntranscript_inset = 2\ncomposer_padding = 1\nshow_footer = true",
        );
        state.model_display = "Test Model".into();
        for (width, text_column) in [(160, 25), (48, 1)] {
            let rows = render_composer_surface(&state, width, Instant::now());
            assert!(rows.len() >= 4, "{width}: {rows:?}");
            // The fill reaches both terminal edges: no unpainted bars beside a
            // capped theme's shaded composer. The blank rows are used for
            // whole-row coverage; a row holding the draft carries the hardware
            // cursor's reverse-video cell, which inverts the rest of its row.
            for row in [&rows[0], &rows[2]] {
                let painted = shaded_columns(row, width);
                assert_eq!(
                    (
                        painted.first().copied(),
                        painted.last().copied(),
                        painted.len()
                    ),
                    (Some(0), width.checked_sub(1), usize::from(width)),
                    "composer row left bars at width {width}: {row:?}"
                );
            }
            // The draft row starts filled too, so the fill has no left edge.
            let draft = shaded_columns(&rows[1], width);
            assert_eq!(
                draft.first().copied(),
                Some(0),
                "draft row left a bar at width {width}: {:?}",
                rows[1]
            );
            // The draft still starts on the reading column, inset and all.
            let plain = rows
                .iter()
                .map(|row| sexy_tui_rs::strip_terminal_sequences(row).to_owned())
                .collect::<Vec<_>>();
            let prompt = plain
                .iter()
                .find(|row| row.contains(state.theme.glyph("prompt")))
                .expect("composer prompt row");
            assert_eq!(
                prompt
                    .find(state.theme.glyph("prompt"))
                    .expect("prompt cell"),
                text_column,
                "{width}: {plain:?}"
            );
            assert!(
                !plain.iter().any(|row| row.contains('─')),
                "{width}: {plain:?}"
            );
        }
    }

    #[test]
    fn an_uncapped_shaded_composer_keeps_its_column_width() {
        // `Cards` insets its content without capping the column, so its shaded
        // composer keeps the historical column width instead of bleeding. The
        // two paths must differ, or the cap check is doing nothing.
        let render = |max_width: Option<&str>| {
            let mut state = crate::tui::view::ShellState::default();
            let cap = max_width.map_or_else(String::new, |width| {
                format!("content_max_width = {width}\n")
            });
            state.theme = crate::tui::theme::test_theme_from_source(&format!(
                "[colors]\ncomposer = \"shaded\"\ncomposer_bg = \"#202020\"\n{cap}[layout]\ntranscript_inset = 6\ncomposer_padding = 1"
            ));
            state.model_display = "Test Model".into();
            render_composer_surface(&state, 160, Instant::now())
        };
        let capped = shaded_columns(&render(Some("112"))[0], 160);
        let uncapped = shaded_columns(&render(None)[0], 160);
        assert_eq!(capped.first().copied(), Some(0), "{capped:?}");
        assert_eq!(
            capped.len(),
            160,
            "a capped fill spans the terminal: {capped:?}"
        );
        assert!(
            uncapped.first().copied().unwrap_or(0) > 0,
            "an uncapped theme keeps its inset rectangle: {uncapped:?}"
        );
        assert!(
            uncapped.len() < capped.len(),
            "an uncapped theme must keep its column width"
        );
    }

    #[test]
    fn topline_preserves_the_workspace_tail_at_the_right_edge() {
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme_from_source(
            "[colors]\ncomposer = \"topline\"\n[layout]\nshow_footer = true\nshow_status_line = true",
        );
        state.model_display = "Test".into();
        state.workspace = Some(std::path::PathBuf::from("/work/long/project-directory"));
        let row =
            sexy_tui_rs::strip_terminal_sequences(&render_topline(&state, 45, Instant::now()))
                .to_owned();
        assert!(row.contains("project-directory"), "{row:?}");
        assert!(row.contains("Test"), "{row:?}");
        assert_eq!(visible_width(&row), 45);

        let wide =
            sexy_tui_rs::strip_terminal_sequences(&render_topline(&state, 80, Instant::now()))
                .to_owned();
        let between = &wide[wide.find("Test").unwrap() + 4..wide.find("/work").unwrap()];
        assert!(between.contains("────"), "{wide:?}");
        assert_eq!(visible_width(&wide), 80);
    }

    #[test]
    fn topline_embeds_metadata_above_prompt_without_a_bottom_rule() {
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme_from_source(
            "[colors]\ncomposer = \"topline\"\n[layout]\nshow_footer = true\nshow_status_line = true",
        );
        state.model_display = "Test Model".into();
        for width in [80, 16, 4] {
            let plain = render_composer_surface(&state, width, Instant::now())
                .into_iter()
                .map(|line| sexy_tui_rs::strip_terminal_sequences(&line))
                .collect::<Vec<_>>();
            assert_eq!(plain.len(), 2, "{width}: {plain:?}");
            assert!(plain[0].contains("Test") || width < 8, "{width}: {plain:?}");
            assert!(
                plain[1].contains(state.theme.glyph("prompt")),
                "{width}: {plain:?}"
            );
            assert!(
                plain
                    .iter()
                    .all(|line| visible_width(line) <= width as usize),
                "{width}: {plain:?}"
            );
        }
    }

    #[test]
    fn overflow_labels_live_in_rules_without_consuming_draft_rows() {
        let mut state = crate::tui::view::ShellState::default();
        state.theme = crate::tui::theme::test_theme();
        let text = (0..12)
            .map(|n| format!("row {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        for width in [12, 40, 81] {
            let geometry = composer_editor_geometry(&state, width);
            for cursor in [0, text.find("row 6").unwrap(), text.len()] {
                let editor = ComposerEditorProjection::new(&text, cursor, geometry.text_width());
                let rows = render_composer_box(&state, width, Instant::now(), 4, geometry, &editor);
                assert_eq!(rows.len(), 6);
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.contains(CURSOR_MARKER))
                        .count(),
                    1
                );
                assert!(rows
                    .iter()
                    .all(|row| visible_width(row) <= usize::from(width)));
                let plain = rows
                    .iter()
                    .map(|row| sexy_tui_rs::strip_terminal_sequences(row))
                    .collect::<Vec<_>>();
                assert_eq!(
                    plain[1..5]
                        .iter()
                        .filter(|row| row.contains("row "))
                        .count(),
                    4
                );
                let start = editor.cursor_row().saturating_add(1).saturating_sub(4);
                let below = editor.line_count().saturating_sub(start + 4);
                if start > 0 {
                    assert!(plain[0].contains(&format!("↑ {start} more")), "{plain:?}");
                }
                if below > 0 {
                    assert!(plain[5].contains(&format!("↓ {below} more")), "{plain:?}");
                }
                assert!(!plain.join("\n").contains("more lines"));
            }
        }
    }

    #[test]
    fn overflow_rules_center_labels_and_support_ascii_and_tiny_widths() {
        use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
        for unicode in [true, false] {
            let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
                true,
                unicode,
                ColorDepth::None,
            ));
            for width in 0..100 {
                for count in [0, 1, 123456] {
                    for above in [true, false] {
                        let row = overflow_rule(
                            &theme,
                            |text| theme.rgb_fg((128, 128, 128), text),
                            width,
                            count,
                            above,
                        );
                        let plain = sexy_tui_rs::strip_terminal_sequences(&row);
                        assert_eq!(visible_width(&plain), width);
                        if !unicode {
                            assert!(plain.is_ascii(), "{plain:?}");
                        }
                        let label = format!(" {} ", overflow_label(&theme, count, above));
                        if count > 0 && visible_width(&label) <= width {
                            let offset = plain.find(&label).unwrap();
                            assert_eq!(
                                visible_width(&plain[..offset]),
                                (width - visible_width(&label)) / 2
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn overflow_preserves_custom_chrome_and_cursor_through_wrapping() {
        for chrome in ["boxed", "framed", "shaded", "topline"] {
            let mut state = crate::tui::view::ShellState::default();
            state.theme = crate::tui::theme::test_theme_from_source(&format!(
                "[colors]\ncomposer = \"{chrome}\"\n[layout]\nshow_footer = true"
            ));
            let text = "界e\u{301} long wrapped draft ".repeat(30);
            for width in [12, 40, 81] {
                let geometry = composer_editor_geometry(&state, width);
                let cursor = text.char_indices().nth(350).unwrap().0;
                let editor = ComposerEditorProjection::new(&text, cursor, geometry.text_width());
                let rows = render_composer_box(&state, width, Instant::now(), 4, geometry, &editor);
                assert_eq!(
                    rows.len(),
                    if chrome == "topline" { 5 } else { 6 },
                    "{chrome}"
                );
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.contains(CURSOR_MARKER))
                        .count(),
                    1
                );
                assert!(rows
                    .iter()
                    .all(|row| visible_width(row) <= usize::from(width)));
                let plain = rows
                    .iter()
                    .map(|row| sexy_tui_rs::strip_terminal_sequences(row))
                    .collect::<Vec<_>>();
                if chrome == "shaded" {
                    assert!(!plain.join("\n").contains('─'));
                }
                if chrome == "framed" {
                    assert!(plain[0].contains(state.theme.glyph("top_left")));
                    assert!(plain[5].contains(state.theme.glyph("bottom_right")));
                }
                if matches!(chrome, "boxed" | "framed") {
                    assert!(plain[0].contains('↑'), "{plain:?}");
                    assert!(plain[5].contains('↓'), "{plain:?}");
                }
            }
        }
    }

    #[test]
    fn content_rows_starts_at_one() {
        // 40-row terminal → max 8 rows (40/5 clamped to 4..14)
        assert_eq!(composer_content_rows(40, 1), 1);
        assert_eq!(composer_content_rows(40, 4), 4);
        assert_eq!(composer_content_rows(40, 20), 8); // capped at 8
                                                      // 12-row terminal → max 3 rows
        assert_eq!(composer_content_rows(12, 1), 1);
        assert_eq!(composer_content_rows(12, 10), 3);
        // 20-row terminal → max 5 rows
        assert_eq!(composer_content_rows(20, 7), 5); // capped at 5
    }

    #[test]
    fn footer_never_falls_back_to_a_first_word_model_name() {
        let name = "Claude Sonnet 4.6";
        for custom in [false, true] {
            let mut state = crate::tui::view::ShellState::default();
            state.theme = if custom {
                crate::tui::theme::test_theme_from_source("[layout]\nprompt_padding = true")
            } else {
                crate::tui::theme::test_theme()
            };
            state.model_display = name.to_owned();
            let wide = sexy_tui_rs::strip_terminal_sequences(&render_status_footer(
                &state,
                80,
                Instant::now(),
            ));
            assert!(wide.contains(name), "{wide:?}");
            let narrow = sexy_tui_rs::strip_terminal_sequences(&render_status_footer(
                &state,
                16,
                Instant::now(),
            ));
            assert!(!narrow.contains("Claude"), "{narrow:?}");
        }
    }

    #[test]
    fn default_footer_masks_the_platform_home_directory() {
        use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

        // The CLI canonicalizes workspace roots. On Windows this also adds the
        // verbatim prefix, unlike USERPROFILE or Git Bash's POSIX-style HOME.
        let home = dirs::home_dir().expect("platform home directory");
        let home = home.canonicalize().expect("canonical home directory");
        for unicode in [true, false] {
            let mut state = crate::tui::view::ShellState::default();
            state.theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
                true,
                unicode,
                ColorDepth::None,
            ));
            state.model_display = "Test".into();
            for (path, expected) in [
                (home.clone(), "~"),
                (
                    home.join("projects").join("footer-fixture"),
                    "~/projects/footer-fixture",
                ),
            ] {
                state.workspace = Some(path.clone());
                let footer = sexy_tui_rs::strip_terminal_sequences(&render_status_footer(
                    &state,
                    160,
                    Instant::now(),
                ));
                assert!(footer.ends_with(expected), "{footer:?}");
                assert!(!footer.contains(&home.display().to_string()), "{footer:?}");
                assert_eq!(state.workspace.as_deref(), Some(path.as_path()));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn default_footer_masks_a_symlinked_home_directory() {
        let directory = tempfile::tempdir().unwrap();
        let alias = directory.path().join("home-alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        // A fresh process avoids changing HOME or the cached home of parallel
        // tests, and exercises the renderer with a canonicalized workspace.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui::composer_surface::tests::default_footer_masks_the_platform_home_directory",
            ])
            .env("HOME", &alias)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn footer_workspace_uses_home_components_not_string_prefixes() {
        use std::path::Path;
        let home = Some(Path::new("/home/user"));
        for (path, expected) in [
            ("/home/user", "~"),
            ("/home/user/project", "~/project"),
            ("/home/user-other/project", "/home/user-other/project"),
            ("/", "/"),
        ] {
            assert_eq!(footer_workspace(Path::new(path), home, 80, true), expected);
        }
        assert_eq!(
            footer_workspace(Path::new("/work/project"), None, 80, true),
            "/work/project"
        );
    }

    #[cfg(windows)]
    #[test]
    fn footer_workspace_masks_canonical_windows_home_paths() {
        use std::path::Path;

        for (home, project) in [
            (
                r"\\?\C:\Users\developer",
                r"\\?\C:\Users\developer\projects\octet",
            ),
            (
                r"\\?\UNC\server\profiles\developer",
                r"\\?\UNC\server\profiles\developer\projects\octet",
            ),
        ] {
            for unicode in [true, false] {
                let home = Path::new(home);
                assert_eq!(footer_workspace(home, Some(home), 80, unicode), "~");
                assert_eq!(
                    footer_workspace(Path::new(project), Some(home), 80, unicode),
                    "~/projects/octet"
                );
                // Outside home the path keeps its spelling, minus the verbatim
                // prefix (`C:\Users\...`, `\\server\...`).
                let sibling = home.with_file_name("developer-other").join("octet");
                let shown = footer_workspace(&sibling, Some(home), 80, unicode);
                assert!(!shown.starts_with(r"\\?\"), "{shown}");
                assert_eq!(
                    shown,
                    crate::presentation::tool_display::human_path(&sibling)
                );
            }
        }
    }

    #[test]
    fn footer_workspace_stays_one_safe_row_and_preserves_grapheme_tails() {
        use std::path::Path;
        let path = Path::new("/work/\x1b[31mparent\x1b[0m\nline\t/家/e\u{301}-👩‍💻");
        for unicode in [true, false] {
            for width in 0..81 {
                let display = footer_workspace(path, None, width, unicode);
                assert!(visible_width(&display) <= width, "{width}: {display:?}");
                assert!(!display.chars().any(char::is_control), "{display:?}");
                if width < 4 {
                    assert!(display.is_empty());
                } else if width >= 6 {
                    assert!(display.ends_with("👩‍💻"), "{width}: {display:?}");
                }
            }
        }
        assert_eq!(
            footer_workspace(Path::new("/long/parent/project"), None, 10, true),
            "…t/project"
        );
        assert_eq!(
            footer_workspace(Path::new("/long/parent/project"), None, 10, false),
            "...project"
        );
    }

    #[test]
    fn footer_cost_is_plain_dollars_even_when_usage_is_uncertain() {
        for usage_uncertain in [true, false] {
            let mut state = crate::tui::view::ShellState::default();
            state.theme = crate::tui::theme::test_theme();
            state.usage_uncertain = usage_uncertain;
            state.price_display = crate::presentation::PriceDisplay::Priced;
            state.session_cost_microdollars = Some(123_456);
            let footer = sexy_tui_rs::strip_terminal_sequences(&render_status_footer(
                &state,
                120,
                Instant::now(),
            ));
            assert!(footer.contains("$0.123"), "{footer:?}");
            for forbidden in ["subtotal", "+", "?", "unknown", "usage/cost"] {
                assert!(
                    !footer.contains(forbidden),
                    "{forbidden:?} leaked into {footer:?}"
                );
            }
            // The durable uncertainty fact is untouched by rendering.
            assert_eq!(state.usage_uncertain, usage_uncertain);
        }

        // A configured zero price is still a plain dollar figure.
        let mut zero = crate::tui::view::ShellState::default();
        zero.theme = crate::tui::theme::test_theme();
        zero.usage_uncertain = true;
        zero.price_display = crate::presentation::PriceDisplay::ExplicitZero;
        let footer = sexy_tui_rs::strip_terminal_sequences(&render_status_footer(
            &zero,
            120,
            Instant::now(),
        ));
        assert!(footer.contains("$0"), "{footer:?}");
        assert!(!footer.contains("subtotal"), "{footer:?}");
    }

    #[test]
    fn footer_cost_rounds_to_three_significant_figures() {
        for (microdollars, expected) in [
            (0, "$0.000"),
            (123_456, "$0.123"),
            (12_345, "$0.0123"),
            (1, "$0.000001"),
            (1_234_567, "$1.23"),
            (12_345_678, "$12.3"),
            (123_456_789, "$123"),
            (9_999_999, "$10.0"),
        ] {
            assert_eq!(format_microdollars(microdollars), expected);
        }
    }
}
