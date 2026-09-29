//! Cursor placement arithmetic for the retained-frame renderer.
//!
//! A component marks where the cursor belongs with a zero-width APC marker
//! embedded in a rendered row ([`CURSOR_MARKER`]). This module owns everything
//! between that marker and the escape sequence that actually parks the
//! terminal cursor: stripping the marker out of the row, resolving the logical
//! row/column it named into the viewport that will be painted, and turning a
//! row delta into a signed vertical move.
//!
//! It is separate from the renderers because the mapping is *pure arithmetic
//! over geometry* and has no terminal side effects, while each renderer owns
//! the decision of when to apply it. That split is what lets the normative Pi
//! path, the alternate-screen path and the inline-scrollback path share one
//! definition of "which row is the cursor on" instead of three that drift.

use std::cmp::Ordering;

use crate::utils::visible_width;

use super::CURSOR_MARKER;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LogicalCursorPosition {
    pub(super) row: usize,
    pub(super) column: usize,
}

pub(super) fn is_termux_session() -> bool {
    std::env::var_os("TERMUX_VERSION").is_some()
}

pub(super) fn extract_logical_cursor_position_from(
    lines: &mut [String],
    row_offset: usize,
) -> Option<LogicalCursorPosition> {
    let mut cursor = None;
    for (local_row, line) in lines.iter_mut().enumerate() {
        while let Some(marker) = line.find(CURSOR_MARKER) {
            cursor = Some(LogicalCursorPosition {
                row: row_offset.saturating_add(local_row),
                column: visible_width(&line[..marker]),
            });
            line.replace_range(marker..marker + CURSOR_MARKER.len(), "");
        }
    }
    cursor
}

pub(super) fn pi_cursor_position(
    cursor: Option<LogicalCursorPosition>,
    total_lines: usize,
    height: usize,
) -> Option<(usize, usize)> {
    let viewport_top = total_lines.saturating_sub(height);
    cursor
        .filter(|cursor| cursor.row >= viewport_top && cursor.row < total_lines)
        .map(|cursor| (cursor.row, cursor.column))
}

pub(super) fn extended_cursor_position(
    cursor: Option<LogicalCursorPosition>,
    total_lines: usize,
    width: u16,
    height: u16,
) -> Option<(u16, u16)> {
    let viewport_start = total_lines.saturating_sub(usize::from(height));
    let max_column = usize::from(width.saturating_sub(1));
    cursor
        .filter(|cursor| cursor.row >= viewport_start && cursor.row < total_lines)
        .map(|cursor| {
            (
                (cursor.row - viewport_start) as u16,
                cursor.column.min(max_column) as u16,
            )
        })
}

pub(super) fn signed_difference(left: usize, right: usize) -> i64 {
    if left >= right {
        i64::try_from(left - right).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(right - left).unwrap_or(i64::MAX)
    }
}

pub(super) fn pi_line_difference(
    hardware_cursor_row: usize,
    previous_viewport_top: usize,
    target_row: usize,
    viewport_top: usize,
) -> i64 {
    let current_screen_row = signed_difference(hardware_cursor_row, previous_viewport_top);
    let target_screen_row = signed_difference(target_row, viewport_top);
    target_screen_row.saturating_sub(current_screen_row)
}

pub(super) fn push_cursor_up(buffer: &mut String, rows: usize) {
    if rows > 0 {
        buffer.push_str(&format!("\x1b[{rows}A"));
    }
}

pub(super) fn push_cursor_down(buffer: &mut String, rows: usize) {
    if rows > 0 {
        buffer.push_str(&format!("\x1b[{rows}B"));
    }
}

pub(super) fn push_vertical_move(buffer: &mut String, rows: i64) {
    match rows.cmp(&0) {
        Ordering::Greater => push_cursor_down(buffer, rows as usize),
        Ordering::Less => push_cursor_up(buffer, rows.unsigned_abs() as usize),
        Ordering::Equal => {}
    }
}

pub(super) fn extract_cursor_position(
    lines: &mut [String],
    width: u16,
    height: u16,
) -> Option<(u16, u16)> {
    let total_lines = lines.len();
    let cursor = extract_logical_cursor_position_from(lines, 0);
    extended_cursor_position(cursor, total_lines, width, height)
}

pub(super) fn extract_cursor_position_from(
    lines: &mut [String],
    row_offset: usize,
    total_lines: usize,
    width: u16,
    height: u16,
) -> Option<(u16, u16)> {
    let cursor = extract_logical_cursor_position_from(lines, row_offset);
    extended_cursor_position(cursor, total_lines, width, height)
}
