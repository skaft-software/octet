use sexy_tui_rs::{slice_by_column, strip_terminal_sequences, visible_width, wrap_text_with_ansi};
use unicode_segmentation::UnicodeSegmentation;

use crate::presentation::{format_run_elapsed, RunOutcome};

use super::outcome_render::{bounded_outcome_detail, completion_text};

#[cfg(test)]
thread_local! {
    /// Painted rows the copy mapping had to align again. A frame or pointer
    /// event on an unchanged block must not spend any.
    static COPY_ROW_ALIGNMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Semantic copy texts projected for a block. The projection copies the
    /// complete answer, so it is built only when the block actually changed.
    static COPY_TEXT_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Take the copy-mapping work one measured region performed.
#[cfg(test)]
pub(super) fn take_copy_mapping_work() -> (usize, usize) {
    (
        COPY_ROW_ALIGNMENTS.with(|work| work.replace(0)),
        COPY_TEXT_BUILDS.with(|work| work.replace(0)),
    )
}
use super::terminal_text::sanitize_for_terminal;
use super::tool_render::{bounded_tool_failure_reason, looks_like_diff};
use super::transcript_cache::SurfaceGeometry;
use super::{ShellState, TranscriptBlock};

/// Durable transcript coordinate. It deliberately names a semantic block and
/// an offset in that block's clean copy text, never a terminal row. Reflow,
/// streaming, and composer animation can therefore not invalidate it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptPosition {
    pub(super) block: usize,
    pub(super) offset: usize,
    /// At a wrapped boundary, retain which side the pointer came from.
    pub(super) trailing_affinity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TranscriptSelection {
    pub(super) anchor: TranscriptPosition,
    pub(super) focus: TranscriptPosition,
}

/// A splash word belongs to one painted prefix row, never a conversation block.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct WelcomeWordSelection {
    row: usize,
    line: String,
    range: std::ops::Range<usize>,
}

/// Splash hit ownership precedes transcript selection, including inert history.
pub(super) fn click_splash(
    state: &mut ShellState,
    col: u16,
    row: u16,
    now: std::time::Instant,
) -> bool {
    if super::builtin_welcome_card::click_mark(state, col, row, now) {
        return true;
    }
    let word = state.pointer_render_geometry().and_then(|frame| {
        let absolute = frame.visible_start + usize::from(row);
        if absolute >= frame.welcome_rows || frame.welcome_mark.is_none() {
            return None;
        }
        let line = strip_terminal_sequences(frame.visible_lines.get(usize::from(row))?);
        let range = painted_word_range(&line, col)?;
        Some(WelcomeWordSelection {
            row: absolute,
            line,
            range,
        })
    });
    state.welcome_text_selection = None;
    let Some(word) = word else {
        state.welcome_last_click = None;
        return false;
    };
    // Pi's fullscreen word selection is row-local, with a 500ms click window:
    // packages/tui/src/tui-alt-screen.ts:81-87,1190-1224,1254-1281.
    // Native terminal ownership keeps this in Rust rather than the adapter.
    let repeated = state
        .welcome_last_click
        .as_ref()
        .is_some_and(|(at, previous)| {
            now.saturating_duration_since(*at) <= std::time::Duration::from_millis(500)
                && previous == &word
        });
    state.clear_text_selection();
    state.welcome_text_selection = repeated.then(|| word.clone());
    // Pi compatibility: packages/tui/src/tui-alt-screen.ts:1339-1378,1398-1404
    // retains word-selection press ownership until release-to-copy. Keep the
    // splash range row-local rather than promoting it into transcript dragging.
    state.pending_screen_press = Some(super::ScreenPress {
        cell: (row, col),
        target: super::ScreenPressTarget::Splash,
        moved: false,
        fence: super::renderer_geometry::GeometryFence::capture(state),
    });
    state.welcome_last_click = Some((now, word));
    true
}

fn painted_word_range(line: &str, col: u16) -> Option<std::ops::Range<usize>> {
    let offset = visual_col_to_offset(line, usize::from(col));
    let segments = line.split_word_bound_indices().collect::<Vec<_>>();
    let index = segments
        .iter()
        .position(|(start, text)| offset >= *start && offset < start + text.len())?;
    let joiner = |text: &str| matches!(text, "/" | "-" | "+" | "\\" | "~" | "_" | ".");
    let selectable = |text: &str| text.unicode_words().next().is_some() || joiner(text);
    let joins = |left: &str, right: &str| {
        selectable(left) && selectable(right) && (joiner(left) || joiner(right))
    };
    let mut first = index;
    let mut last = index;
    while first > 0 && joins(segments[first - 1].1, segments[first].1) {
        first -= 1;
    }
    while last + 1 < segments.len() && joins(segments[last].1, segments[last + 1].1) {
        last += 1;
    }
    Some(segments[first].0..segments[last].0 + segments[last].1.len())
}

/// Clean semantic text used by the application-owned selection/copy path.
/// It intentionally never uses visual rows, ANSI styling, borders, elision,
/// composer text, or footer text.
fn shell_copy_header(shell: &super::ShellOutput) -> String {
    let status = if shell.running {
        "running"
    } else if shell.exit_code == 0 {
        "completed"
    } else {
        "failed"
    };
    sanitize_for_terminal(&format!("$ {} [{status}]", shell.command))
}

pub(super) fn shell_output_copy_start(block: &TranscriptBlock) -> Option<usize> {
    match block {
        TranscriptBlock::Shell(shell) => Some(shell_copy_header(shell).len() + 1),
        _ => None,
    }
}

pub(super) fn block_copy_text(block: &TranscriptBlock) -> String {
    match block {
        TranscriptBlock::Subagents(summary) => summary.label(),
        TranscriptBlock::User { text, .. } | TranscriptBlock::Notice(text) => {
            sanitize_for_terminal(text)
        }
        TranscriptBlock::NoticeStatus { text, .. } => sanitize_for_terminal(text),
        TranscriptBlock::UpdateAvailable(version) => super::startup_update::update_message(version),
        TranscriptBlock::Compaction(compaction) => format!(
            "{}\n{}",
            sanitize_for_terminal(&compaction.label),
            sexy_tui_rs::parse_markdown(&compaction.summary).plain_text()
        ),
        TranscriptBlock::Assistant(markdown) | TranscriptBlock::Reasoning(markdown) => {
            markdown.copy_text()
        }
        TranscriptBlock::Tool(panel) => {
            let summary = if panel.finished {
                if panel.is_error {
                    &panel.display.failure
                } else {
                    &panel.display.success
                }
            } else {
                &panel.display.active
            };
            let text = if let Some(source) = super::codemode_render::source(panel) {
                format!("Codemode\n{source}")
            } else if let Some(command) = &panel.display.shell_command {
                format!("$ {command}")
            } else {
                format!("{}  {summary}", panel.display.label)
            };
            let mut text = sanitize_for_terminal(&text);
            if panel.finished && panel.is_error {
                if let Some(reason) = bounded_tool_failure_reason(panel) {
                    text.push('\n');
                    text.push_str(&reason);
                }
            }
            text
        }
        TranscriptBlock::Shell(shell) => {
            let mut text = shell_copy_header(shell);
            let output =
                sanitize_for_terminal(&super::normalize_carriage_return_progress(&shell.output));
            if !output.is_empty() {
                text.push('\n');
                text.push_str(&output);
            }
            text
        }
        TranscriptBlock::Outcome(outcome) => match &outcome.outcome {
            // Both completed variants copy as `completed`: the warning wording is
            // transcript-invisible by decision, so a copy cannot reintroduce it.
            // The warning count stays in the model for exit status and telemetry.
            RunOutcome::Completed { elapsed, .. }
            | RunOutcome::CompletedWithWarnings { elapsed, .. } => {
                completion_text(*elapsed, " · ", outcome.generation.as_ref())
            }
            RunOutcome::Failed { elapsed, reason } => format!(
                "failed · {}\n{}",
                format_run_elapsed(*elapsed),
                bounded_outcome_detail(reason.as_str())
            ),
            RunOutcome::Interrupted { elapsed } => {
                format!("interrupted · {}", format_run_elapsed(*elapsed))
            }
            RunOutcome::NeedsInput { prompt } => format!("needs input · {prompt}"),
        },
    }
}

/// A tool's copy projection uses exactly the sanitized fields and disclosure
/// window used by the presenter, not the complete retained tool result.
/// Cached projections contain only copy text/spans; styled rows live in the line cache.
fn presented_tool(
    state: &ShellState,
    index: usize,
    content_width: Option<u16>,
) -> std::sync::Arc<super::tool_render::ToolRows> {
    {
        let cache = state.transcript_cache.borrow();
        if !cache.dirty
            && cache.width == Some(state.transcript_content_width(state.size.0))
            && cache.block_revisions.get(index) == state.block_revisions.get(index)
            && content_width
                .is_none_or(|width| cache.block_geometries[index].content_width == width)
        {
            if let Some(projected) = &cache.block_tool_copy[index] {
                return projected.clone();
            }
        }
    }
    let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
        unreachable!()
    };
    let expanded = state.show_tool_details(index);
    if panel.grouped_child
        && !state.verbose_tools
        && state
            .theme
            .resolve::<bool>("quiet_tool_summaries")
            .unwrap_or(false)
    {
        return Default::default();
    }
    let plan = super::surface_layout::compile_surface_plan_for_render(
        index
            .checked_sub(1)
            .and_then(|previous| state.transcript.get(previous)),
        &state.transcript[index],
        &state.theme,
        state.transcript_content_width(state.size.0),
        state.verbose_tools,
    );
    std::sync::Arc::new(super::transcript_render::render_tool(
        panel,
        &state.theme,
        &state.theme.rich_renderer(),
        content_width.unwrap_or(plan.geometry.content_width),
        expanded,
        plan.content_role,
    ))
}

pub(super) fn presented_block_copy_text(state: &ShellState, index: usize) -> String {
    #[cfg(test)]
    COPY_TEXT_BUILDS.with(|work| work.set(work.get() + 1));
    let block = &state.transcript[index];
    #[cfg(test)]
    super::fullscreen_work_tests::record(|work| work.messages_visited += 1);
    let text = match block {
        TranscriptBlock::Tool(_) => presented_tool(state, index, None).text.clone(),
        TranscriptBlock::Reasoning(reasoning)
            if !reasoning.reasoning_expanded && !state.verbose_tools =>
        {
            String::new()
        }
        TranscriptBlock::Compaction(compaction) if !compaction.expanded && !state.verbose_tools => {
            sanitize_for_terminal(&compaction.label)
        }
        TranscriptBlock::NoticeStatus {
            tone:
                super::NoticeTone::ToolActive
                | super::NoticeTone::ToolSuccess
                | super::NoticeTone::ToolError,
            ..
        } if !state
            .theme
            .resolve::<bool>("quiet_tool_summaries")
            .unwrap_or(false) =>
        {
            String::new()
        }
        _ => block_copy_text(block),
    };
    #[cfg(test)]
    super::fullscreen_work_tests::record(|work| work.copy_text_bytes += text.len());
    text
}

/// Side-effect-free semantic selection projection. Prompt expansion can read
/// this without mutating the retained copy buffer or touching the clipboard.
pub(super) fn semantic_selected_text(state: &ShellState) -> Option<String> {
    if let Some(word) = &state.welcome_text_selection {
        return Some(word.line[word.range.clone()].to_owned());
    }
    let selection = state.transcript_selection.clone()?;
    let (start, end) = if (selection.anchor.block, selection.anchor.offset)
        <= (selection.focus.block, selection.focus.offset)
    {
        (selection.anchor, selection.focus)
    } else {
        (selection.focus, selection.anchor)
    };
    let mut blocks = Vec::new();
    for index in start.block..=end.block {
        let block = state.transcript.get(index)?;
        if let TranscriptBlock::Assistant(block) = block {
            let map = state.markdown_source_map(index);
            let len = map
                .as_ref()
                .map_or_else(|| block.copy_text_len(), |map| map.plain_len);
            if len == 0 {
                continue;
            }
            let from = if index == start.block {
                start.offset.min(len)
            } else {
                0
            };
            let to = if index == end.block {
                end.offset.min(len)
            } else {
                len
            };
            blocks.push(match map {
                // A painted whole block can be older than the live message:
                // never include an unpainted append on release.
                Some(map) => map.copy(from.min(to), to)?,
                None if from == 0 && to == len => block.text.clone(),
                None => return None,
            });
            continue;
        }
        let text = presented_block_copy_text(state, index);
        if text.is_empty() {
            continue;
        }
        let from = if index == start.block {
            clamp_copy_offset(&text, start.offset)
        } else {
            0
        };
        let to = if index == end.block {
            clamp_copy_offset(&text, end.offset)
        } else {
            text.len()
        };
        blocks.push(text[from.min(to)..to].to_owned());
    }
    Some(blocks.join("\n\n"))
}

/// Apply selection only to semantic text cells in the visible transcript.
/// Layout and row-to-copy mapping remain renderer-owned; neither chrome nor
/// decorative separators can acquire the selection's rendition.
pub(super) fn decorate_selection(state: &ShellState, lines: &mut [String], start_row: usize) {
    if state.theme.capabilities().color == crate::tui::terminal::ColorDepth::None {
        return;
    }
    if let Some(word) = &state.welcome_text_selection {
        if let Some(line) = word
            .row
            .checked_sub(start_row)
            .and_then(|row| lines.get_mut(row))
        {
            // A resize or changed header must not move a historical highlight
            // onto different text, a mark cell, or a later transcript block.
            if strip_terminal_sequences(line) == word.line {
                CopyRow {
                    text: word.line.clone(),
                    fragments: Vec::new(),
                    offset: 0,
                    painted_cells: 0,
                }
                .decorate(line, word.range.start, word.range.end);
            }
        }
    }
    let Some(selection) = state.transcript_selection.as_ref() else {
        return;
    };
    let (start, end) = if (selection.anchor.block, selection.anchor.offset)
        <= (selection.focus.block, selection.focus.offset)
    {
        (selection.anchor, selection.focus)
    } else {
        (selection.focus, selection.anchor)
    };
    let cache = state.transcript_cache.borrow();
    // Intersect the selection with the painted window before visiting blocks.
    // An offscreen anchor can otherwise make every drag scan settled history.
    for index in cache
        .blocks_in_rows(start_row, start_row + lines.len())
        .filter(|index| *index >= start.block && *index <= end.block)
    {
        #[cfg(test)]
        super::fullscreen_work_tests::record(|work| work.messages_visited += 1);
        let Some((block_start, block_rows, geometry)) = state.block_span(index) else {
            continue;
        };
        if block_start + block_rows <= start_row || block_start >= start_row + lines.len() {
            continue;
        }
        // This runs on every painted frame. The alignment is reused from the
        // previous frame unless the block, the width, the theme or the
        // disclosure actually changed.
        let Some(mapping) = state.copy_mapping(index, geometry) else {
            continue;
        };
        let from = if index == start.block {
            start.offset
        } else {
            0
        };
        let to = if index == end.block {
            end.offset
        } else {
            mapping.text_len()
        };
        let first = block_start + geometry.transition_rows + geometry.leading_rows;
        let window =
            start_row.saturating_sub(first)..(start_row + lines.len()).saturating_sub(first);
        for content_row in window {
            let Some(row) = mapping.row(content_row) else {
                break;
            };
            let Some(line) = (first + content_row)
                .checked_sub(start_row)
                .and_then(|row| lines.get_mut(row))
            else {
                continue;
            };
            row.decorate(line, from, to);
        }
    }
}

#[derive(Clone, Debug)]
struct CopyFragment {
    text: String,
    offset: usize,
    painted_cells: u16,
}

impl CopyRow {
    fn offset_at(&self, absolute: usize) -> usize {
        if !self.fragments.is_empty() {
            let i = self
                .fragments
                .partition_point(|part| usize::from(part.painted_cells) <= absolute)
                .saturating_sub(1);
            let part = &self.fragments[i];
            return part.offset
                + visual_col_to_offset(
                    &part.text,
                    absolute.saturating_sub(usize::from(part.painted_cells)),
                );
        }
        self.offset
            + visual_col_to_offset(
                &self.text,
                absolute.saturating_sub(usize::from(self.painted_cells)),
            )
    }

    fn decorate(&self, line: &mut String, from: usize, to: usize) {
        if self.fragments.is_empty() {
            decorate_fragment(line, &self.text, self.offset, self.painted_cells, from, to);
        } else {
            for part in &self.fragments {
                decorate_fragment(line, &part.text, part.offset, part.painted_cells, from, to);
            }
        }
    }
}

fn decorate_fragment(
    line: &mut String,
    text: &str,
    offset: usize,
    painted_cells: u16,
    from: usize,
    to: usize,
) {
    let left = from.saturating_sub(offset).min(text.len());
    let right = to.saturating_sub(offset).min(text.len());
    if left >= right {
        return;
    }
    let left = clamp_copy_offset(text, left);
    let right = clamp_copy_offset(text, right);
    let column = usize::from(painted_cells) + visible_width(&text[..left]);
    let cells = visible_width(&text[left..right]);
    let selected = strip_terminal_sequences(&slice_by_column(line, column, cells, true));
    *line = format!(
        "{}\x1b[0;7m{selected}\x1b[0m{}",
        slice_by_column(line, 0, column, true),
        slice_by_column(
            line,
            column + cells,
            visible_width(line).saturating_sub(column + cells),
            true
        )
    );
    if line.contains("\x1b]8;") {
        line.push_str("\x1b]8;;\x1b\\");
    }
}

fn clamp_copy_offset(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn visual_col_to_offset(line: &str, col: usize) -> usize {
    let mut current_col = 0;
    let mut byte_offset = 0;
    for grapheme in line.graphemes(true) {
        if current_col >= col {
            break;
        }
        let width = unicode_width::UnicodeWidthStr::width(grapheme);
        if current_col + width > col {
            break;
        }
        current_col += width;
        byte_offset += grapheme.len();
    }
    byte_offset
}

fn newline_col_offset(text: &str, line_index: usize, col: u16) -> usize {
    let start_offset = newline_offset(text, line_index);
    let line = text.split('\n').nth(line_index).unwrap_or("");
    let cell_offset = visual_col_to_offset(line, usize::from(col));
    start_offset + cell_offset
}

fn wrapped_lines_with_source_offsets(text: &str, wrap_width: usize) -> (Vec<String>, Vec<usize>) {
    let wrapped = wrap_text_with_ansi(text, wrap_width.max(1));
    let mut offsets = Vec::with_capacity(wrapped.len());
    let mut cursor = 0usize;
    for line in &wrapped {
        if line.is_empty() {
            offsets.push(cursor.min(text.len()));
            continue;
        }
        let start = text[cursor..]
            .find(line)
            .map(|relative| cursor.saturating_add(relative))
            .unwrap_or(cursor)
            .min(text.len());
        offsets.push(start);
        cursor = start.saturating_add(line.len()).min(text.len());
    }
    (wrapped, offsets)
}

fn wrapped_line_col_offset(text: &str, line_index: usize, col: u16, wrap_width: usize) -> usize {
    let (wrapped, offsets) = wrapped_lines_with_source_offsets(text, wrap_width);
    let start_offset = offsets.get(line_index).copied().unwrap_or(text.len());
    let line = wrapped.get(line_index).map(String::as_str).unwrap_or("");
    let cell_offset = visual_col_to_offset(line, usize::from(col));
    (start_offset + cell_offset).min(text.len())
}

/// One painted row's alignment, with what it read to get there.
#[derive(Clone, Debug)]
struct AlignedCopyRow {
    painted: String,
    decoration_only: bool,
    row: CopyRow,
    cursor_before: usize,
    cursor_after: usize,
    /// End of the copy text a successful search read (`..examined_end`).
    examined_end: usize,
    /// Candidates that did not occur after `cursor_before`.
    missed: Vec<String>,
    /// The row's line ran to the end of the copy text.
    to_text_end: bool,
}

impl AlignedCopyRow {
    /// Whether this alignment still holds for `copy_text`, which shares its
    /// first `common` bytes with the text it was made from.
    fn still_holds(&self, copy_text: &str, common: usize, identical: bool) -> bool {
        if identical {
            return true;
        }
        if self.to_text_end || self.examined_end > common {
            return false;
        }
        // A missed candidate did not occur in the shared prefix, so it can
        // only occur now if it ends in the changed tail: search just that.
        self.missed.iter().all(|candidate| {
            let mut start = self
                .cursor_before
                .max(common.saturating_sub(candidate.len().saturating_sub(1)));
            while !copy_text.is_char_boundary(start) {
                start -= 1;
            }
            !copy_text[start..].contains(candidate.as_str())
        })
    }
}

/// Layout inputs that decide one block's copy rows, compared in O(1).
///
/// A frame or a pointer event on an unchanged block therefore costs nothing,
/// while a change to the block, the width, the surface geometry, the theme or
/// the disclosure can never reuse an alignment that named different text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CopyMemoIdentity {
    commit: u64,
    revision: u64,
    width: Option<u16>,
    geometry: SurfaceGeometry,
    theme_epoch: u64,
    verbose: bool,
}

impl CopyMemoIdentity {
    fn capture(state: &ShellState, index: usize, geometry: SurfaceGeometry) -> Self {
        Self {
            commit: state
                .transcript_commit_ids
                .get(index)
                .copied()
                .unwrap_or(u64::MAX),
            revision: state.block_revisions.get(index).copied().unwrap_or(0),
            width: state.transcript_cache.borrow().width,
            geometry,
            theme_epoch: state.theme_epoch,
            verbose: state.show_tool_details(index),
        }
    }

    /// Everything but the block's own revision. A live answer bumps only that,
    /// so the rows above the change stay usable.
    fn same_layout(self, other: Self) -> bool {
        self.commit == other.commit
            && self.width == other.width
            && self.geometry == other.geometry
            && self.theme_epoch == other.theme_epoch
            && self.verbose == other.verbose
    }
}

/// Row alignments of one block, kept between frames by the renderer.
#[derive(Clone, Debug, Default)]
pub(super) struct BlockCopyMemo {
    identity: Option<CopyMemoIdentity>,
    copy_text: String,
    content_left: Option<u16>,
    output_start: Option<usize>,
    shell_output: Option<super::transcript_cache::ShellOutputSource>,
    rows: Vec<AlignedCopyRow>,
    /// Content rows the block painted for this identity. Trailing decorative
    /// rows cannot demand an alignment that will never exist.
    content_rows: usize,
    /// A tool's bounded presented projection. Tools never align painted rows,
    /// so their whole mapping is this projection.
    tool: Option<CopyRows>,
    source_map: Option<std::sync::Arc<super::markdown_source::SourceMap>>,
    /// Table cells wrap independently; row-first offsets are not sufficient
    /// for reverse lookup. Prepared once with layout, never searched by text.
    fragment_rows: std::collections::BTreeMap<usize, (usize, usize)>,
}

/// One block's projections, as read by an alignment pass.
#[derive(Clone, Copy)]
pub(super) struct CopySurface<'a> {
    /// The block's painted rows.
    pub(super) painted: &'a [String],
    decoration_rows: &'a [usize],
    /// The block's semantic copy text.
    pub(super) text: &'a str,
    pub(super) geometry: SurfaceGeometry,
    /// Copy-text offset where the shell command header ends, if any.
    pub(super) output_start: Option<usize>,
}

impl CopyRows {
    /// [`Self::build_visible`], reusing the rows `memo` aligned for an earlier
    /// frame of the same block while its identity is unchanged.
    ///
    /// Rows are aligned top to bottom, each searching the copy text from where
    /// the previous row ended, so a visible row at the bottom of a long block
    /// depends on every row above it. Without the memo, every frame of a
    /// streaming answer re-stripped and re-matched the whole answer.
    fn build_visible_with(
        surface: CopySurface<'_>,
        visible: std::ops::Range<usize>,
        memo: &mut BlockCopyMemo,
        identity: Option<CopyMemoIdentity>,
    ) -> Self {
        let first = surface
            .geometry
            .transition_rows
            .saturating_add(surface.geometry.leading_rows);
        memo.align(surface, visible.end.saturating_sub(first), identity);
        memo.visible_rows(surface.geometry, visible)
    }
}

impl BlockCopyMemo {
    /// Extend this block's alignment through `through` content rows.
    ///
    /// `identity` is the block's O(1) layout identity: when it still matches
    /// and already covers `through`, the rows are known to describe the same
    /// text and painted cells without reading either. A partial alignment (a
    /// frame painted at the head of a long answer) is extended, never rebuilt.
    /// `None` keeps the validate-each-row behaviour for one-shot projections
    /// that own no identity.
    fn align(
        &mut self,
        surface: CopySurface<'_>,
        through: usize,
        identity: Option<CopyMemoIdentity>,
    ) {
        let CopySurface {
            painted,
            text: copy_text,
            decoration_rows,
            geometry,
            output_start,
        } = surface;
        if let Some(identity) = identity {
            if self.identity == Some(identity) && self.covers(through) {
                return;
            }
        }
        let first = geometry
            .transition_rows
            .saturating_add(geometry.leading_rows);
        let content_rows = painted
            .len()
            .saturating_sub(first)
            .saturating_sub(geometry.trailing_rows);
        let content = painted
            .get(first..first.saturating_add(content_rows))
            .unwrap_or_default();
        let reusable = if self.output_start != output_start
            || self.shell_output != geometry.shell_output
        {
            0
        } else {
            match (identity, self.identity) {
                (Some(current), Some(previous)) if current == previous => self.rows.len(),
                (Some(current), Some(previous)) if current.same_layout(previous) => {
                    self.reusable_rows(content, copy_text, geometry.content_left, decoration_rows)
                }
                (Some(_), _) => 0,
                (None, _) => {
                    self.reusable_rows(content, copy_text, geometry.content_left, decoration_rows)
                }
            }
        };
        self.identity = identity;
        self.output_start = output_start;
        self.shell_output = geometry.shell_output;
        self.tool = None;
        self.content_rows = content.len();
        self.rows.truncate(reusable);
        let table_rows = self
            .source_map
            .as_ref()
            .map(|map| align_table_rows(content, copy_text, map, geometry, decoration_rows))
            .unwrap_or_default();
        self.fragment_rows = table_rows
            .iter()
            .flat_map(|(row, copy)| {
                copy.fragments.iter().map(move |fragment| {
                    (
                        fragment.offset,
                        (fragment.offset + fragment.text.len(), *row),
                    )
                })
            })
            .collect();
        if self.copy_text != copy_text {
            copy_text.clone_into(&mut self.copy_text);
        }
        self.content_left = Some(geometry.content_left);
        let mut cursor = self.rows.last().map_or(0, |row| row.cursor_after);
        for (row, line) in content
            .iter()
            .enumerate()
            .take(through.min(content.len()))
            .skip(self.rows.len())
        {
            let source = match (output_start, row) {
                (Some(start), 0) => &copy_text[..start.saturating_sub(1).min(copy_text.len())],
                (Some(start), 1) => {
                    cursor = (start + geometry.shell_output.map_or(0, |source| source.start))
                        .min(copy_text.len());
                    copy_text
                }
                _ => copy_text,
            };
            let metadata = output_start.is_some()
                && row > 0
                && geometry
                    .shell_output
                    .is_some_and(|source| row <= source.metadata_rows);
            let decoration_only = decoration_rows.binary_search(&row).is_ok();
            let mut aligned = align_copy_row(
                if metadata || decoration_only {
                    ""
                } else {
                    line
                },
                source,
                cursor,
                geometry.content_left,
                output_start.is_some() && row > 0,
            );
            aligned.decoration_only = decoration_only;
            if metadata || decoration_only {
                line.clone_into(&mut aligned.painted);
            }
            if let Some(table) = table_rows.get(&row) {
                aligned.row = table.clone();
                aligned.cursor_after = table.offset + table.text.len();
                aligned.examined_end = aligned.cursor_after;
            }
            if output_start.is_some() && row == 0 {
                aligned.to_text_end = false;
                aligned.missed.clear();
            }
            cursor = aligned.cursor_after;
            self.rows.push(aligned);
        }
    }

    /// How many leading rows still align exactly as recorded: the same painted
    /// row, and the same copy text wherever the row's searches could look.
    ///
    /// Only a revision-only change to the same layout reaches this, so a live
    /// answer verifies the prefix once instead of re-matching the whole block.
    fn reusable_rows(
        &self,
        content: &[String],
        copy_text: &str,
        content_left: u16,
        decoration_rows: &[usize],
    ) -> usize {
        if self.content_left != Some(content_left) {
            return 0;
        }
        let common = self
            .copy_text
            .bytes()
            .zip(copy_text.bytes())
            .take_while(|(old, new)| old == new)
            .count();
        let identical = common == self.copy_text.len() && common == copy_text.len();
        self.rows
            .iter()
            .zip(content)
            .enumerate()
            .take_while(|(i, (aligned, line))| {
                aligned.decoration_only == decoration_rows.binary_search(i).is_ok()
                    && aligned.painted == **line
                    && aligned.still_holds(copy_text, common, identical)
            })
            .count()
    }

    /// Whether every content row a window reaches is already aligned.
    fn covers(&self, through: usize) -> bool {
        self.tool.is_some() || self.rows.len() >= through.min(self.content_rows)
    }

    /// One content-row window of this block, projected for a bounded receipt.
    fn visible_rows(&self, geometry: SurfaceGeometry, visible: std::ops::Range<usize>) -> CopyRows {
        let first = geometry
            .transition_rows
            .saturating_add(geometry.leading_rows);
        let from = visible.start.saturating_sub(first).min(self.rows.len());
        let to = visible.end.saturating_sub(first).min(self.rows.len());
        CopyRows {
            first_row: from,
            content_left: geometry.content_left,
            rows: self.rows[from..to]
                .iter()
                .map(|aligned| aligned.row.clone())
                .collect(),
        }
    }
}

impl CopyMapping for BlockCopyMemo {
    fn offset_for(&self, content_row: usize, col: u16) -> Option<usize> {
        if let Some(tool) = &self.tool {
            return tool.offset_for(content_row, col);
        }
        let row = &self.rows.get(content_row)?.row;
        let absolute = usize::from(col).saturating_add(usize::from(self.content_left?));
        Some(row.offset_at(absolute))
    }

    fn row_for_offset(&self, offset: usize, trailing: bool) -> Option<usize> {
        if let Some(tool) = &self.tool {
            return tool.row_for_offset(offset, trailing);
        }
        let fragment = if trailing {
            self.fragment_rows.range(..offset).next_back()
        } else {
            self.fragment_rows.range(..=offset).next_back()
        };
        if let Some((_, (_, row))) = fragment.filter(|(_, (end, _))| offset <= *end) {
            return Some(*row);
        }
        if self.rows.is_empty() {
            return None;
        }
        let boundary = self.rows.partition_point(|row| row.row.offset < offset);
        if boundary < self.rows.len() && self.rows[boundary].row.offset == offset && !trailing {
            return Some(boundary);
        }
        Some(boundary.saturating_sub(1).min(self.rows.len() - 1))
    }

    fn row(&self, content_row: usize) -> Option<&CopyRow> {
        match &self.tool {
            Some(tool) => tool.row(content_row),
            None => Some(&self.rows.get(content_row)?.row),
        }
    }

    fn text_len(&self) -> usize {
        match &self.tool {
            Some(tool) => tool.text_len(),
            None => self.copy_text.len(),
        }
    }
}

/// Renderer-owned [`BlockCopyMemo`]s, keyed by transcript block index.
///
/// The index travels with the block's identity, so a shifted index, a replaced
/// block or a relaid-out width invalidates its entry instead of reusing it.
#[derive(Default)]
pub(super) struct CopyRowsMemo(std::collections::HashMap<usize, BlockCopyMemo>);

impl CopyRowsMemo {
    pub(super) fn get(&self, index: usize) -> Option<&BlockCopyMemo> {
        self.0.get(&index)
    }

    /// The block's alignment when it still describes this exact layout, without
    /// reading the block's text or its painted rows.
    fn current(
        &self,
        state: &ShellState,
        index: usize,
        geometry: SurfaceGeometry,
    ) -> Option<&BlockCopyMemo> {
        let identity = CopyMemoIdentity::capture(state, index, geometry);
        self.0
            .get(&index)
            .filter(|memo| memo.identity == Some(identity))
    }

    pub(super) fn block(&mut self, index: usize) -> &mut BlockCopyMemo {
        self.0.entry(index).or_default()
    }

    /// Align one block's painted rows, reusing the ones already recorded.
    fn align(&mut self, state: &ShellState, index: usize, geometry: SurfaceGeometry) -> bool {
        let identity = CopyMemoIdentity::capture(state, index, geometry);
        if self
            .0
            .get(&index)
            .is_some_and(|memo| memo.identity == Some(identity) && memo.covers(usize::MAX))
        {
            return true;
        }
        let mut entry = self.0.remove(&index).unwrap_or_default();
        let Some((start, length)) = state
            .block_span(index)
            .map(|(start, length, _)| (start, length))
        else {
            return false;
        };
        if matches!(state.transcript.get(index), Some(TranscriptBlock::Tool(_))) {
            let projected = presented_tool(state, index, Some(geometry.content_width));
            entry.tool = Some(CopyRows::tool(&projected, geometry, length, 0..length));
            entry.identity = Some(identity);
            self.0.insert(index, entry);
            return true;
        }
        let copy_text = presented_block_copy_text(state, index);
        entry.source_map = match &state.transcript[index] {
            TranscriptBlock::Assistant(block) => Some(block.prepare_source_map(&copy_text)),
            _ => None,
        };
        let output_start = shell_output_copy_start(&state.transcript[index]);
        let cache = state.transcript_cache.borrow();
        let Some(painted) = cache.lines.get(start..start.saturating_add(length)) else {
            return false;
        };
        let decoration_rows = markdown_decoration_rows(&state.transcript[index]);
        entry.align(
            CopySurface {
                painted,
                decoration_rows: decoration_rows.as_deref().map_or(&[], Vec::as_slice),
                text: &copy_text,
                geometry,
                output_start,
            },
            usize::MAX,
            Some(identity),
        );
        drop(cache);
        self.0.insert(index, entry);
        true
    }

    /// Forget blocks that left the viewport.
    pub(super) fn retain(&mut self, visible: &std::collections::HashSet<usize>) {
        self.0.retain(|index, _| visible.contains(index));
    }

    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
}

/// Align one painted row with the copy text, searching from `cursor`.
fn align_copy_row(
    line: &str,
    copy_text: &str,
    cursor: usize,
    content_left: u16,
    literal: bool,
) -> AlignedCopyRow {
    #[cfg(test)]
    super::fullscreen_work_tests::record(|work| work.rows_mapped += 1);
    #[cfg(test)]
    COPY_ROW_ALIGNMENTS.with(|work| work.set(work.get() + 1));
    sexy_tui_rs::render_metrics::record(|work| work.mapped += 1);
    let plain = strip_terminal_sequences(line);
    let mut aligned = AlignedCopyRow {
        painted: line.to_owned(),
        decoration_only: false,
        row: CopyRow {
            text: String::new(),
            offset: cursor,
            painted_cells: content_left,
            fragments: Vec::new(),
        },
        cursor_before: cursor,
        cursor_after: cursor,
        examined_end: cursor,
        missed: Vec::new(),
        to_text_end: false,
    };
    // Try every spelling before choosing an occurrence. A painted bullet or
    // heading can also occur literally later in the answer; prioritizing that
    // spelling would jump past the actual, decorated source row.
    let mut best = None;
    // The event margin is outside the content surface. Its marker must not
    // consume the one Markdown marker the content row can carry.
    let content_start = visual_col_to_offset(&plain, usize::from(content_left));
    for mut candidate in copy_candidates(&plain[content_start..]) {
        candidate.painted_cells = candidate.painted_cells.saturating_add(content_left);
        let Some(relative) = copy_text[cursor..].find(candidate.text) else {
            aligned.missed.push(candidate.text.to_owned());
            continue;
        };
        let matched = cursor + relative;
        if best.as_ref().is_none_or(|(offset, _)| matched < *offset) {
            best = Some((matched, candidate));
        }
    }
    if let Some((matched, candidate)) = best {
        let end = matched + candidate.text.len();
        let mut start = matched;
        let mut painted_cells = candidate.painted_cells;
        if candidate.line && !literal {
            // A list/quote marker has a semantic spelling of its own. Include
            // that prefix, not the unpainted rest of a wrapped logical line.
            start = copy_text[..matched]
                .rfind('\n')
                .map_or(0, |newline| newline + 1)
                .max(cursor);
            painted_cells =
                painted_cells.saturating_sub(visible_width(&copy_text[start..matched]) as u16);
        }
        aligned.cursor_after = end;
        aligned.examined_end = end;
        aligned.row = CopyRow {
            text: copy_text[start..end].to_owned(),
            offset: start,
            painted_cells,
            fragments: Vec::new(),
        };
    }
    aligned
}

/// Cell-to-offset mapping for one block's painted rows.
///
/// Both projections answer the same two questions: which copy offset one
/// painted cell names, and which painted row one copy offset names. The
/// renderer's bounded paint receipt and its full alignment index therefore
/// serve the same pointer and anchor code.
trait CopyMapping {
    /// Copy offset for one cell of a content row. `col` is relative to the
    /// block's content inset, the same cell the renderer used.
    fn offset_for(&self, content_row: usize, col: u16) -> Option<usize>;
    /// Content row holding one copy offset.
    fn row_for_offset(&self, offset: usize, trailing: bool) -> Option<usize>;
    /// One aligned painted row, for decoration.
    fn row(&self, content_row: usize) -> Option<&CopyRow>;
    /// End of the semantic text this mapping aligns.
    fn text_len(&self) -> usize;
}

impl CopyMapping for CopyRows {
    fn offset_for(&self, content_row: usize, col: u16) -> Option<usize> {
        Self::offset_for(self, content_row, col)
    }

    fn row_for_offset(&self, offset: usize, trailing: bool) -> Option<usize> {
        Self::row_for_offset(self, offset, trailing)
    }

    fn row(&self, content_row: usize) -> Option<&CopyRow> {
        self.rows.get(content_row.checked_sub(self.first_row)?)
    }

    fn text_len(&self) -> usize {
        self.rows
            .last()
            .map_or(0, |row| row.offset.saturating_add(row.text.len()))
    }
}

/// One painted content row aligned with the block's semantic copy text.
#[derive(Clone, Debug)]
struct CopyRow {
    fragments: Vec<CopyFragment>,
    /// Copy-text segment this row shows, used for the cell-to-offset mapping.
    text: String,
    /// Byte offset in the copy text where `text` starts.
    offset: usize,
    /// Painted cell where `text` starts (rails, markers, list indents).
    painted_cells: u16,
}

/// The painted content rows of one block, each aligned with the semantic copy
/// text the selection names.
///
/// The renderer inserts rows the copy text does not contain - blank separators
/// between Markdown blocks, list continuation indents, code and table frames -
/// and wraps prose at the width left after its own decoration. Re-wrapping the
/// copy text therefore cannot locate the row the reader clicked. When both
/// projections have the same row count the plain wrap already locates every
/// row exactly and is kept; otherwise each painted row is matched against the
/// copy text, so a selection names the text on screen. A row that carries no
/// source text (a separator or a frame) keeps the next unconsumed offset
/// instead of stealing the following line.
#[derive(Clone, Debug, Default)]
pub(super) struct CopyRows {
    first_row: usize,
    /// Painted cell where the block's content inset starts.
    content_left: u16,
    rows: Vec<CopyRow>,
}

impl CopyRows {
    #[cfg(test)]
    fn build(
        painted: &[String],
        copy_text: &str,
        geometry: SurfaceGeometry,
        output_start: Option<usize>,
    ) -> Self {
        Self::build_visible(painted, copy_text, geometry, output_start, 0..painted.len())
    }

    #[cfg(test)]
    fn build_visible(
        painted: &[String],
        copy_text: &str,
        geometry: SurfaceGeometry,
        output_start: Option<usize>,
        visible: std::ops::Range<usize>,
    ) -> Self {
        Self::build_visible_with(
            CopySurface {
                painted,
                decoration_rows: &[],
                text: copy_text,
                geometry,
                output_start,
            },
            visible,
            &mut BlockCopyMemo::default(),
            None,
        )
    }

    fn tool(
        projected: &super::tool_render::ToolRows,
        geometry: SurfaceGeometry,
        total_rows: usize,
        visible: std::ops::Range<usize>,
    ) -> Self {
        let first = geometry.transition_rows + geometry.leading_rows;
        let count = total_rows
            .saturating_sub(first + geometry.trailing_rows)
            .min(projected.copy_rows.len());
        let from = visible.start.saturating_sub(first).min(count);
        let to = visible.end.saturating_sub(first).min(count);
        let mut cursor = 0;
        let rows = projected
            .copy_rows
            .iter()
            .take(to)
            .enumerate()
            .filter_map(|(index, source)| {
                let row = if let Some(source) = source {
                    cursor = source.source.end;
                    CopyRow {
                        text: projected.text[source.source.clone()].to_owned(),
                        offset: source.source.start,
                        painted_cells: geometry.content_left.saturating_add(source.left),
                        fragments: Vec::new(),
                    }
                } else {
                    CopyRow {
                        text: String::new(),
                        offset: cursor,
                        painted_cells: geometry.content_left,
                        fragments: Vec::new(),
                    }
                };
                (index >= from).then_some(row)
            })
            .collect();
        Self {
            first_row: from,
            content_left: geometry.content_left,
            rows,
        }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Copy offset for one cell of a painted content row. `col` is relative to
    /// the block's content inset, the same cell the renderer used.
    fn offset_for(&self, content_row: usize, col: u16) -> Option<usize> {
        let row = self.rows.get(content_row.checked_sub(self.first_row)?)?;
        let absolute = usize::from(col).saturating_add(usize::from(self.content_left));
        Some(row.offset_at(absolute))
    }

    /// Painted content row holding one copy offset.
    fn row_for_offset(&self, offset: usize, trailing: bool) -> Option<usize> {
        // Receipts contain only the viewport's bounded rows. Prefer an exact
        // fragment start, then the fragment ending here, over a later rail.
        let fragment = (!trailing)
            .then(|| {
                self.rows
                    .iter()
                    .position(|row| row.fragments.iter().any(|part| part.offset == offset))
            })
            .flatten()
            .or_else(|| {
                self.rows.iter().position(|row| {
                    row.fragments
                        .iter()
                        .any(|part| part.offset < offset && offset <= part.offset + part.text.len())
                })
            });
        if let Some(row) = fragment {
            return Some(self.first_row + row);
        }
        if self.rows.is_empty() {
            return None;
        }
        let boundary = self.rows.partition_point(|row| row.offset < offset);
        if boundary < self.rows.len() && self.rows[boundary].offset == offset && !trailing {
            return Some(self.first_row + boundary);
        }
        Some(self.first_row + boundary.saturating_sub(1).min(self.rows.len() - 1))
    }
}

/// One candidate source segment for a painted row.
struct CopyCandidate<'a> {
    /// Whether the candidate is the row's text after its painted decoration,
    /// which the copy text may spell with a marker of its own.
    line: bool,
    /// Painted cell where `text` starts.
    painted_cells: u16,
    /// Source segment to look for in the copy text.
    text: &'a str,
}

/// Candidate source segments for one painted row, most specific first.
///
/// The row's own text is the primary candidate: when it appears verbatim in the
/// copy text the row maps exactly as before. A row whose painted decoration is
/// not part of the copy text (`•`, a card rail, a code frame) also offers the
/// text after that decoration, which then names the complete copy line.
fn copy_candidates(plain: &str) -> Vec<CopyCandidate<'_>> {
    let line = plain.trim_end();
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let indent_cells = visible_width(&line[..indent]) as u16;
    let mut candidates = vec![CopyCandidate {
        line: false,
        painted_cells: indent_cells,
        text: trimmed,
    }];
    if let Some((decoration_cells, core)) = strip_row_decoration(trimmed) {
        if !core.is_empty() && core != trimmed {
            candidates.push(CopyCandidate {
                line: true,
                painted_cells: indent_cells + decoration_cells,
                text: core,
            });
        }
    }
    candidates
}

/// Strip one row's painted decoration and return its cell width with the text.
///
/// Decoration is whitespace, box/rail glyphs, a prompt chevron, and at most one
/// list, heading, task or quote marker - exactly the cells a surface paints
/// before the semantic text, and a trailing frame or padding run.
fn strip_row_decoration(trimmed: &str) -> Option<(u16, &str)> {
    let mut end = 0;
    let mut marker_seen = false;
    let mut rest = trimmed;
    while let Some(character) = rest.chars().next() {
        let width = character.len_utf8();
        if character.is_whitespace() || is_frame_glyph(character) {
            rest = &rest[width..];
            end += width;
            continue;
        }
        if marker_seen {
            break;
        }
        marker_seen = true;
        if let Some(marker) = rendered_marker(rest) {
            rest = &rest[marker..];
            end += marker;
            continue;
        }
        break;
    }
    let core_end = rest
        .trim_end_matches(|character: char| character.is_whitespace() || is_frame_glyph(character));
    let width = visible_width(&trimmed[..end]) as u16;
    Some((width, &rest[..core_end.len()]))
}

/// Byte length of one list, heading, task or quote marker, if any.
///
/// The marker is only decoration when the row continues with whitespace, so a
/// prose row that happens to start with a dash keeps its own text.
fn rendered_marker(trimmed: &str) -> Option<usize> {
    let digits = trimmed
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let mut end = 0;
    if digits > 0 && matches!(trimmed[digits..].chars().next(), Some('.') | Some(')')) {
        end = digits + 1;
    } else if let Some(rest) = trimmed
        .strip_prefix("[ ]")
        .or_else(|| trimmed.strip_prefix("[x]"))
    {
        end = trimmed.len() - rest.len();
    } else if let Some(character) = trimmed.chars().next() {
        if matches!(
            character,
            '•' | '●'
                | '◦'
                | '▪'
                | '‣'
                | '❯'
                | '›'
                | '»'
                | '→'
                | '-'
                | '—'
                | '–'
                | '*'
                | '+'
                | '>'
                | '#'
        ) {
            end = character.len_utf8();
        }
    }
    (end > 0 && trimmed[end..].starts_with(' ')).then_some(end)
}

fn is_frame_glyph(character: char) -> bool {
    matches!(
        character,
        '│' | '|'
            | '┃'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
            | '┌'
            | '┐'
            | '└'
            | '┘'
            | '├'
            | '┤'
            | '┬'
            | '┴'
            | '┼'
            | '─'
            | '━'
            | '═'
            | '❯'
            | '›'
            | '»'
            | '→'
    )
}

/// Copy mapping for the visible part of a block, captured by the renderer.
/// The receipt retains at most viewport-height rows even for a huge block.
#[cfg(test)]
pub(super) fn visible_copy_rows(
    painted: &[String],
    text: &str,
    geometry: SurfaceGeometry,
    output_start: Option<usize>,
    visible: std::ops::Range<usize>,
    memo: &mut BlockCopyMemo,
) -> Option<CopyRows> {
    let rows = CopyRows::build_visible_with(
        CopySurface {
            painted,
            decoration_rows: &[],
            text,
            geometry,
            output_start,
        },
        visible,
        memo,
        None,
    );
    (!rows.is_empty()).then_some(rows)
}

fn markdown_decoration_rows(block: &TranscriptBlock) -> Option<std::cell::Ref<'_, Vec<usize>>> {
    match block {
        TranscriptBlock::Assistant(block) => Some(block.decoration_rows.borrow()),
        _ => None,
    }
}

/// The renderer and the synchronous frontend share this projection. Only the
/// visible row ranges enter the completed-frame pointer receipt, and the rows
/// come from the same alignment index the frame decorated.
pub(super) fn visible_block_copy_rows(
    state: &ShellState,
    index: usize,
    painted: &[String],
    geometry: SurfaceGeometry,
    visible: std::ops::Range<usize>,
) -> Option<CopyRows> {
    if matches!(state.transcript[index], TranscriptBlock::Tool(_)) {
        let projected = presented_tool(state, index, Some(geometry.content_width));
        let rows = CopyRows::tool(&projected, geometry, painted.len(), visible);
        return (!rows.is_empty()).then_some(rows);
    }
    let first = geometry
        .transition_rows
        .saturating_add(geometry.leading_rows);
    let through = visible.end.saturating_sub(first);
    {
        let index_rows = state.copy_rows.borrow();
        if let Some(rows) = index_rows
            .current(state, index, geometry)
            .filter(|rows| rows.covers(through))
        {
            let rows = rows.visible_rows(geometry, visible);
            return (!rows.is_empty()).then_some(rows);
        }
    }
    let copy_text = presented_block_copy_text(state, index);
    let source_map = match &state.transcript[index] {
        TranscriptBlock::Assistant(block) => Some(block.prepare_source_map(&copy_text)),
        _ => None,
    };
    let identity = CopyMemoIdentity::capture(state, index, geometry);
    let output_start = shell_output_copy_start(&state.transcript[index]);
    let mut index_rows = state.copy_rows.borrow_mut();
    let memo = index_rows.block(index);
    memo.source_map = source_map;
    let decoration_rows = markdown_decoration_rows(&state.transcript[index]);
    let rows = CopyRows::build_visible_with(
        CopySurface {
            painted,
            decoration_rows: decoration_rows.as_deref().map_or(&[], Vec::as_slice),
            text: &copy_text,
            geometry,
            output_start,
        },
        visible,
        memo,
        Some(identity),
    );
    (!rows.is_empty()).then_some(rows)
}

fn visual_cell_to_copy_offset(
    block: &TranscriptBlock,
    copy_text: &str,
    rows: Option<&dyn CopyMapping>,
    local_row: usize,
    col: u16,
    width: u16,
) -> usize {
    if let Some(offset) = rows.and_then(|rows| rows.offset_for(local_row, col)) {
        return offset;
    }
    match block {
        TranscriptBlock::Assistant(assistant) => {
            if looks_like_diff(&assistant.text) {
                return newline_col_offset(copy_text, local_row, col);
            }
            wrapped_line_col_offset(copy_text, local_row, col, usize::from(width).max(1))
        }
        TranscriptBlock::Reasoning(_) => {
            wrapped_line_col_offset(copy_text, local_row, col, usize::from(width).max(1))
        }
        TranscriptBlock::User { .. } => {
            let inner_width = (width.saturating_sub(2) as usize).max(1);
            let col_in_text = col.saturating_sub(2);
            wrapped_line_col_offset(copy_text, local_row, col_in_text, inner_width)
        }
        TranscriptBlock::UpdateAvailable(_)
        | TranscriptBlock::Notice(_)
        | TranscriptBlock::NoticeStatus { .. }
        | TranscriptBlock::Compaction(_) => {
            wrapped_line_col_offset(copy_text, local_row, col, usize::from(width).max(1))
        }
        TranscriptBlock::Outcome(_) | TranscriptBlock::Subagents(_) => {
            visual_col_to_offset(copy_text, usize::from(col))
        }
        TranscriptBlock::Tool(_) => {
            let indent = if width < 60 { 7 } else { 8 };
            let col_in_text = col.saturating_sub(indent);
            newline_col_offset(copy_text, local_row, col_in_text)
        }
        TranscriptBlock::Shell(_) => {
            wrapped_line_col_offset(copy_text, local_row, col, usize::from(width).max(1))
        }
    }
}

fn wrapped_offset_to_line(text: &str, offset: usize, wrap_width: usize, trailing: bool) -> usize {
    let (wrapped, offsets) = wrapped_lines_with_source_offsets(text, wrap_width);
    let offset = clamp_copy_offset(text, offset);
    let boundary = offsets.partition_point(|start| *start < offset);
    if boundary < offsets.len() && offsets[boundary] == offset && !trailing {
        return boundary;
    }
    boundary
        .saturating_sub(1)
        .min(wrapped.len().saturating_sub(1))
}

fn newline_offset_to_line(text: &str, offset: usize, trailing: bool) -> usize {
    let offset = clamp_copy_offset(text, offset);
    let mut start = 0usize;
    for (line_index, line) in text.split_inclusive('\n').enumerate() {
        let end = start.saturating_add(line.len());
        if offset < end || (trailing && offset == end) {
            return line_index;
        }
        start = end;
    }
    text.split('\n').count().saturating_sub(1)
}

fn copy_offset_to_visual_row(
    block: &TranscriptBlock,
    copy_text: &str,
    rows: Option<&CopyRows>,
    offset: usize,
    trailing_affinity: bool,
    width: u16,
) -> usize {
    if let Some(row) = rows.and_then(|rows| rows.row_for_offset(offset, trailing_affinity)) {
        return row;
    }
    match block {
        TranscriptBlock::Assistant(assistant) if looks_like_diff(&assistant.text) => {
            newline_offset_to_line(copy_text, offset, trailing_affinity)
        }
        TranscriptBlock::Assistant(_)
        | TranscriptBlock::Reasoning(_)
        | TranscriptBlock::UpdateAvailable(_)
        | TranscriptBlock::Notice(_)
        | TranscriptBlock::NoticeStatus { .. }
        | TranscriptBlock::Compaction(_)
        | TranscriptBlock::Shell(_) => wrapped_offset_to_line(
            copy_text,
            offset,
            usize::from(width).max(1),
            trailing_affinity,
        ),
        TranscriptBlock::User { .. } => wrapped_offset_to_line(
            copy_text,
            offset,
            usize::from(width.saturating_sub(2)).max(1),
            trailing_affinity,
        ),
        TranscriptBlock::Outcome(_) | TranscriptBlock::Subagents(_) => 0,
        TranscriptBlock::Tool(_) => newline_offset_to_line(copy_text, offset, trailing_affinity),
    }
}

pub(super) fn visual_line_for_transcript_position(
    state: &ShellState,
    position: TranscriptPosition,
) -> Option<usize> {
    let (start, total_rows, geometry) = if state.render_threaded {
        let block = state
            .retained_render_geometry()?
            .blocks
            .iter()
            .find(|block| block.index == position.block)?;
        if state.transcript_commit_ids.get(block.index) != Some(&block.id) {
            return None;
        }
        (block.start, block.rows, block.surface)
    } else {
        state.block_span(position.block)?
    };
    let content_rows = total_rows
        .saturating_sub(geometry.transition_rows)
        .saturating_sub(geometry.leading_rows)
        .saturating_sub(geometry.trailing_rows);
    if content_rows == 0 {
        return None;
    }
    let block = state.transcript.get(position.block)?;
    let mapped = state
        .copy_mapping(position.block, geometry)
        .and_then(|rows| rows.row_for_offset(position.offset, position.trailing_affinity));
    let content_row = match mapped {
        Some(row) => row,
        None => {
            let copy_text = presented_block_copy_text(state, position.block);
            copy_offset_to_visual_row(
                block,
                &copy_text,
                None,
                position.offset,
                position.trailing_affinity,
                geometry.content_width,
            )
        }
    }
    .min(content_rows.saturating_sub(1));
    Some(
        start
            .saturating_add(geometry.transition_rows)
            .saturating_add(geometry.leading_rows)
            .saturating_add(content_row),
    )
}

pub(super) fn selection_position_for_visual_cell(
    state: &ShellState,
    visual_line: usize,
    col: u16,
) -> Option<TranscriptPosition> {
    let (block, local_row, total_rows, geometry) = if state.render_threaded {
        let block = state
            .gesture_render_geometry()?
            .blocks
            .iter()
            .find(|block| visual_line >= block.start && visual_line < block.start + block.rows)?;
        if state.transcript_commit_ids.get(block.index) != Some(&block.id) {
            return None;
        }
        (
            block.index,
            visual_line - block.start,
            block.rows,
            block.surface,
        )
    } else {
        let cache = state.transcript_cache.borrow();
        let block = cache
            .block_starts
            .partition_point(|start| *start <= visual_line)
            .checked_sub(1)?;
        (
            block,
            visual_line.checked_sub(cache.block_starts[block])?,
            *cache.block_lengths.get(block)?,
            *cache.block_geometries.get(block)?,
        )
    };
    if local_row >= total_rows {
        return None;
    }
    let content_row = geometry.content_row(local_row, total_rows)?;
    let content_col = geometry.content_col(col);
    let transcript_block = state.transcript.get(block)?;
    let mapped = state
        .copy_mapping(block, geometry)
        .and_then(|rows| rows.offset_for(content_row, content_col));
    let offset = match mapped {
        Some(offset) => offset,
        None => {
            let text = presented_block_copy_text(state, block);
            let offset = visual_cell_to_copy_offset(
                transcript_block,
                &text,
                None,
                content_row,
                content_col,
                geometry.content_width,
            );
            clamp_copy_offset(&text, offset)
        }
    };
    state.retain_markdown_source_map(block);
    Some(TranscriptPosition {
        block,
        offset,
        trailing_affinity: false,
    })
}

/// Byte-offset after `line_index` newline-delimited segments (current
/// behaviour for blocks where wrapping correspondence is unavailable).
fn newline_offset(text: &str, line_index: usize) -> usize {
    text.split_inclusive('\n')
        .take(line_index)
        .map(str::len)
        .sum::<usize>()
        .min(text.len())
}

/// One block's copy mapping, borrowed from wherever it is owned: the renderer's
/// alignment index, or the bounded receipt a completed threaded frame published.
enum CopyMappingRef<'a> {
    Aligned {
        index_rows: std::cell::Ref<'a, CopyRowsMemo>,
        index: usize,
    },
    Painted(&'a CopyRows),
}

impl CopyMappingRef<'_> {
    fn mapping(&self) -> &dyn CopyMapping {
        match self {
            Self::Aligned { index_rows, index } => {
                index_rows.get(*index).expect("aligned copy mapping")
            }
            Self::Painted(rows) => *rows,
        }
    }
}

impl CopyMapping for CopyMappingRef<'_> {
    fn offset_for(&self, content_row: usize, col: u16) -> Option<usize> {
        self.mapping().offset_for(content_row, col)
    }

    fn row_for_offset(&self, offset: usize, trailing: bool) -> Option<usize> {
        self.mapping().row_for_offset(offset, trailing)
    }

    fn row(&self, content_row: usize) -> Option<&CopyRow> {
        self.mapping().row(content_row)
    }

    fn text_len(&self) -> usize {
        self.mapping().text_len()
    }
}

impl ShellState {
    /// Painted row span and surface geometry of one transcript block.
    fn block_span(&self, index: usize) -> Option<(usize, usize, SurfaceGeometry)> {
        let cache = self.transcript_cache.borrow();
        Some((
            *cache.block_starts.get(index)?,
            *cache.block_lengths.get(index)?,
            *cache.block_geometries.get(index)?,
        ))
    }

    /// Copy mapping of one block for pointer and anchor work.
    ///
    /// Pointer input in the threaded mode reads only the bounded receipt a
    /// completed frame published, never a whole-block mapping. The renderer and
    /// the synchronous frontend read this state's alignment index, which is
    /// reused across frames until the block's identity changes.
    fn copy_mapping(&self, index: usize, geometry: SurfaceGeometry) -> Option<CopyMappingRef<'_>> {
        if self.render_threaded {
            let rows = self
                .gesture_render_geometry()?
                .blocks
                .iter()
                .find(|block| block.index == index)?
                .copy_rows
                .as_ref()?;
            return Some(CopyMappingRef::Painted(rows));
        }
        // Bind the alignment borrow out before reading: a temporary in an `if`
        // condition would still be alive inside the block below.
        let aligned = self.copy_rows.borrow_mut().align(self, index, geometry);
        if !aligned {
            return None;
        }
        Some(CopyMappingRef::Aligned {
            index_rows: self.copy_rows.borrow(),
            index,
        })
    }
}

#[cfg(test)]
#[path = "transcript_selection_tests.rs"]
mod tests;

/// The narrow labeled-list table presentation repeats headers as chrome.
/// Only value fragments acquire the original cell's semantic coordinates.
fn align_narrow_tables(
    content: &[String],
    plain: &str,
    source: &super::markdown_source::SourceMap,
    geometry: SurfaceGeometry,
    decoration_rows: &[usize],
) -> std::collections::HashMap<usize, CopyRow> {
    let mut output = std::collections::HashMap::new();
    let mut painted_row = 0;
    for table in &source.tables {
        let header = &source.table_rows[table.start];
        // Match rich_text/render/blocks.rs:399-403: rails/padding plus at
        // least three cells of text per column, otherwise a labeled list.
        if usize::from(geometry.content_width) > header.len() * 6 {
            continue;
        }
        let body_start = table.start + usize::from(table.len() > 1);
        for cells in &source.table_rows[body_start..table.end] {
            for (column, span) in cells.iter().enumerate() {
                let label = if table.len() > 1 {
                    format!("{}: ", &plain[header[column].clone()])
                } else {
                    String::new()
                };
                let value_start = label.len();
                let combined = label + &plain[span.clone()];
                let mut cursor = 0;
                while painted_row < content.len() && cursor < combined.len() {
                    let row = painted_row;
                    painted_row += 1;
                    if decoration_rows.binary_search(&row).is_ok() {
                        continue;
                    }
                    let line = strip_terminal_sequences(&content[row]);
                    let inset = visual_col_to_offset(&line, usize::from(geometry.content_left));
                    let tail = &line[inset..];
                    let text = tail.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let Some(relative) = combined[cursor..].find(text) else {
                        continue;
                    };
                    let start = cursor + relative;
                    cursor = start + text.len();
                    let from = start.max(value_start);
                    let end = cursor.max(value_start);
                    let offset = span.start + from - value_start;
                    let painted_cells = (visible_width(&line[..inset])
                        + visible_width(&tail[..tail.len() - tail.trim_start().len()])
                        + visible_width(&combined[start..from]))
                        as u16;
                    let text = combined[from..end].to_owned();
                    output.insert(
                        row,
                        CopyRow {
                            fragments: if text.is_empty() {
                                Vec::new()
                            } else {
                                vec![CopyFragment {
                                    text: text.clone(),
                                    offset,
                                    painted_cells,
                                }]
                            },
                            text,
                            offset,
                            painted_cells,
                        },
                    );
                    if combined[cursor..].trim().is_empty() {
                        break;
                    }
                }
            }
        }
    }
    output
}

/// Tables lay out cells independently. A row therefore carries one provenance
/// fragment per cell rather than pretending its rails/padding are source text.
fn align_table_rows(
    content: &[String],
    plain: &str,
    source: &super::markdown_source::SourceMap,
    geometry: SurfaceGeometry,
    decoration_rows: &[usize],
) -> std::collections::HashMap<usize, CopyRow> {
    let content_left = geometry.content_left;
    let mut output = align_narrow_tables(content, plain, source, geometry, decoration_rows);
    if source.table_rows.is_empty() {
        return output;
    }
    let mut row_index = 0;
    let mut consumed = Vec::new();
    for (row, line) in content.iter().enumerate() {
        if output.contains_key(&row) || decoration_rows.binary_search(&row).is_ok() {
            continue;
        }
        let line = strip_terminal_sequences(line);
        let inset = visual_col_to_offset(&line, usize::from(content_left));
        let rail = if line[inset..].contains('│') {
            '│'
        } else {
            '|'
        };
        let bars = line[inset..]
            .char_indices()
            .filter(|(_, c)| *c == rail)
            .map(|(i, c)| (inset + i, c.len_utf8()))
            .collect::<Vec<_>>();
        if bars.len() < 2 {
            continue;
        }
        let cells = bars
            .windows(2)
            .map(|pair| {
                let start = pair[0].0 + pair[0].1;
                let value = &line[start..pair[1].0];
                let left = value.len() - value.trim_start().len();
                (value.trim(), visible_width(&line[..start + left]) as u16)
            })
            .collect::<Vec<_>>();
        let mut found = None;
        for candidate in row_index..source.table_rows.len() {
            let spans = &source.table_rows[candidate];
            if spans.len() != cells.len() {
                continue;
            }
            let starts = if candidate == row_index && consumed.len() == spans.len() {
                consumed.clone()
            } else {
                spans.iter().map(|span| span.start).collect()
            };
            let mut next = starts.clone();
            let mut fragments = Vec::new();
            let mut valid = true;
            for (column, ((text, left), span)) in cells.iter().zip(spans).enumerate() {
                if text.is_empty() {
                    continue;
                }
                let Some(relative) = plain[starts[column]..span.end].find(text) else {
                    valid = false;
                    break;
                };
                let offset = starts[column] + relative;
                next[column] = offset + text.len();
                fragments.push(CopyFragment {
                    text: (*text).to_owned(),
                    offset,
                    painted_cells: *left,
                });
            }
            if valid && !fragments.is_empty() {
                found = Some((candidate, next, fragments));
                break;
            }
        }
        let Some((candidate, next, fragments)) = found else {
            continue;
        };
        row_index = candidate;
        consumed = next;
        let offset = fragments.first().unwrap().offset;
        let end = fragments.last().unwrap().offset + fragments.last().unwrap().text.len();
        output.insert(
            row,
            CopyRow {
                text: plain[offset..end].to_owned(),
                offset,
                painted_cells: fragments[0].painted_cells,
                fragments,
            },
        );
        if consumed
            .iter()
            .zip(&source.table_rows[row_index])
            .all(|(end, span)| *end >= span.end)
        {
            row_index += 1;
            consumed.clear();
        }
    }
    output
}
