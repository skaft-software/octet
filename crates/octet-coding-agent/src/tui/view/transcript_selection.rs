use sexy_tui_rs::{strip_terminal_sequences, visible_width, wrap_text_with_ansi};
use unicode_segmentation::UnicodeSegmentation;

use crate::presentation::{format_duration, RunOutcome};

use super::outcome_render::{bounded_outcome_detail, completion_text};
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

/// Clean semantic text used by the application-owned selection/copy path.
/// It intentionally never uses visual rows, ANSI styling, borders, elision,
/// composer text, or footer text.
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
            let status = if shell.running {
                "running"
            } else if shell.exit_code == 0 {
                "completed"
            } else {
                "failed"
            };
            sanitize_for_terminal(&format!("$ {} [{status}]", shell.command))
        }
        TranscriptBlock::Outcome(outcome) => match &outcome.outcome {
            // Both completed variants copy as `completed`: the warning wording is
            // transcript-invisible by decision, so a copy cannot reintroduce it.
            // The warning count stays in the model for exit status and telemetry.
            RunOutcome::Completed { elapsed, .. }
            | RunOutcome::CompletedWithWarnings { elapsed, .. } => {
                completion_text(*elapsed, " · ", outcome.inference.as_deref())
            }
            RunOutcome::Failed { elapsed, reason } => format!(
                "failed · {}\n{}",
                format_duration(*elapsed),
                bounded_outcome_detail(reason.as_str())
            ),
            RunOutcome::Interrupted { elapsed } => {
                format!("interrupted · {}", format_duration(*elapsed))
            }
            RunOutcome::NeedsInput { prompt } => format!("needs input · {prompt}"),
        },
    }
}

/// Side-effect-free semantic selection projection. Prompt expansion can read
/// this without mutating the retained copy buffer or touching the clipboard.
pub(super) fn semantic_selected_text(state: &ShellState) -> Option<String> {
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
        let text = block_copy_text(state.transcript.get(index)?);
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

/// One painted content row aligned with the block's semantic copy text.
#[derive(Clone, Debug)]
struct CopyRow {
    /// Copy-text segment this row shows, used for the cell-to-offset mapping.
    text: String,
    /// Byte offset in the copy text where `text` starts.
    offset: usize,
    /// Decorative cells painted before `text` (rails, markers, list indents).
    decoration: usize,
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
#[derive(Default)]
struct CopyRows {
    rows: Vec<CopyRow>,
}

impl CopyRows {
    fn build(painted: &[String], copy_text: &str, geometry: SurfaceGeometry) -> Self {
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
        let mut rows = Vec::with_capacity(content.len());
        let mut cursor = 0usize;
        for line in content {
            let plain = strip_terminal_sequences(line);
            let mut row = CopyRow {
                text: String::new(),
                offset: cursor,
                decoration: 0,
            };
            for candidate in copy_candidates(&plain) {
                let Some(relative) = copy_text[cursor..].find(candidate.text) else {
                    continue;
                };
                let matched = cursor + relative;
                if let Some(marker_cells) = candidate.after_marker {
                    // The copy text spells its own marker for this row (`-`,
                    // `1.`, `#`), so the row names that complete line. When the
                    // copy line starts with the matched text instead, the
                    // painted marker is decoration in front of it.
                    let line_start = copy_text[..matched]
                        .rfind('\n')
                        .map_or(0, |newline| newline + 1);
                    let line_end = copy_text[matched..]
                        .find('\n')
                        .map_or(copy_text.len(), |newline| matched + newline);
                    let decoration = if matched == line_start {
                        candidate.decoration
                    } else {
                        candidate.decoration.saturating_sub(marker_cells)
                    };
                    row = CopyRow {
                        text: copy_text[line_start..line_end].to_owned(),
                        offset: line_start,
                        decoration,
                    };
                    cursor = line_end;
                } else {
                    row = CopyRow {
                        text: candidate.text.to_owned(),
                        offset: matched,
                        decoration: candidate.decoration,
                    };
                    cursor = matched + candidate.text.len();
                }
                break;
            }
            rows.push(row);
        }
        Self { rows }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Copy offset for one cell of a painted content row.
    fn offset_for(&self, content_row: usize, col: u16) -> Option<usize> {
        let row = self.rows.get(content_row)?;
        let col = usize::from(col).saturating_sub(row.decoration);
        Some(row.offset + visual_col_to_offset(&row.text, col))
    }

    /// Painted content row holding one copy offset.
    fn row_for_offset(&self, offset: usize, trailing: bool) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let boundary = self.rows.partition_point(|row| row.offset < offset);
        if boundary < self.rows.len() && self.rows[boundary].offset == offset && !trailing {
            return Some(boundary);
        }
        Some(boundary.saturating_sub(1).min(self.rows.len() - 1))
    }
}

/// One candidate source segment for a painted row.
struct CopyCandidate<'a> {
    /// Byte length of the painted marker this candidate stripped, if any.
    after_marker: Option<usize>,
    /// Decorative cells painted before `text`.
    decoration: usize,
    /// Source segment to look for in the copy text.
    text: &'a str,
}

/// Candidate source segments for one painted row, longest first.
///
/// A rendered marker (`•`, `#`, a code frame) may stand for the copy text's own
/// marker, so the row's text after that marker is a second candidate.
fn copy_candidates(plain: &str) -> Vec<CopyCandidate<'_>> {
    let indent = plain.len() - plain.trim_start().len();
    let trimmed = plain.trim_start();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let indent_cells = visible_width(&plain[..indent]);
    let mut candidates = vec![CopyCandidate {
        after_marker: None,
        decoration: indent_cells,
        text: trimmed,
    }];
    if let Some(marker) = rendered_marker(trimmed) {
        let content = trimmed[marker..].trim_start();
        if !content.is_empty() {
            let marker_cells = visible_width(&trimmed[..marker]);
            candidates.push(CopyCandidate {
                after_marker: Some(marker_cells),
                decoration: indent_cells + marker_cells,
                text: content,
            });
        }
    }
    candidates
}

/// Byte length of a rendered list, heading or frame marker, if any.
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
        return Some(trimmed.len() - rest.len());
    } else if let Some(character) = trimmed.chars().next() {
        if matches!(
            character,
            '•' | '●' | '◦' | '▪' | '‣' | '-' | '*' | '+' | '>' | '#' | '|' | '│' | '┃'
        ) {
            end = character.len_utf8();
        }
    }
    (end > 0 && trimmed[end..].starts_with(' ')).then_some(end)
}

/// The painted content rows of one block, aligned with its copy text.
fn painted_copy_rows(
    state: &ShellState,
    block: usize,
    copy_text: &str,
    geometry: SurfaceGeometry,
) -> Option<CopyRows> {
    let (start, length) = {
        let cache = state.transcript_cache.borrow();
        (
            *cache.block_starts.get(block)?,
            *cache.block_lengths.get(block)?,
        )
    };
    let painted = {
        let cache = state.transcript_cache.borrow();
        cache.lines.get(start..start.checked_add(length)?)?.to_vec()
    };
    let rows = CopyRows::build(&painted, copy_text, geometry);
    (!rows.is_empty()).then_some(rows)
}

fn visual_cell_to_copy_offset(
    block: &TranscriptBlock,
    copy_text: &str,
    rows: Option<&CopyRows>,
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
        let cache = state.transcript_cache.borrow();
        (
            *cache.block_starts.get(position.block)?,
            *cache.block_lengths.get(position.block)?,
            *cache.block_geometries.get(position.block)?,
        )
    };
    let content_rows = total_rows
        .saturating_sub(geometry.transition_rows)
        .saturating_sub(geometry.leading_rows)
        .saturating_sub(geometry.trailing_rows);
    if content_rows == 0 {
        return None;
    }
    let block = state.transcript.get(position.block)?;
    let copy_text = block_copy_text(block);
    let rows = painted_copy_rows(state, position.block, &copy_text, geometry);
    let content_row = copy_offset_to_visual_row(
        block,
        &copy_text,
        rows.as_ref(),
        position.offset,
        position.trailing_affinity,
        geometry.content_width,
    )
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
            .retained_render_geometry()?
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
    let text = block_copy_text(transcript_block);
    let rows = painted_copy_rows(state, block, &text, geometry);
    let offset = visual_cell_to_copy_offset(
        transcript_block,
        &text,
        rows.as_ref(),
        content_row,
        content_col,
        geometry.content_width,
    );
    Some(TranscriptPosition {
        block,
        offset: clamp_copy_offset(&text, offset),
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
