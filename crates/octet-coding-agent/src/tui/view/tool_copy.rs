//! Copy positions carried by the sanitized tool presentation, before framing.

use std::ops::Range;

use sexy_tui_rs::{slice_by_column, strip_terminal_sequences, visible_width, WidthPolicy};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug)]
pub(in crate::tui::view) struct ToolCopyRow {
    pub(in crate::tui::view) source: Range<usize>,
    pub(in crate::tui::view) left: u16,
}

#[derive(Default)]
pub(in crate::tui::view) struct ToolRows {
    pub(in crate::tui::view) lines: Vec<String>,
    pub(in crate::tui::view) text: String,
    pub(in crate::tui::view) copy_rows: Vec<Option<ToolCopyRow>>,
}

impl ToolRows {
    pub(in crate::tui::view) fn rich_text(text: &str, width: u16) -> String {
        // Match rich literal rendering: tabs expand at four-cell stops and a
        // grapheme wider than the entire content lane is displayed as '?'.
        let expanded = WidthPolicy::default().expand_tabs(text, 0);
        expanded
            .graphemes(true)
            .map(|grapheme| {
                if visible_width(grapheme) > usize::from(width) {
                    "?"
                } else {
                    grapheme
                }
            })
            .collect()
    }

    pub(in crate::tui::view) fn rich_literal(
        lines: Vec<String>,
        text: String,
        left: u16,
        width: u16,
    ) -> Self {
        Self::literal(lines, Self::rich_text(&text, width), left)
    }

    /// Literal rows have a known gutter and no synthetic rows. Searches are
    /// ordered, exact fragments of this one sanitized field, never candidates
    /// against an argument envelope or a different input/output surface.
    /// Call `retain_rows` once all decorative rows and windows are classified.
    pub(in crate::tui::view) fn literal(lines: Vec<String>, text: String, left: u16) -> Self {
        let mut cursor = 0;
        let copy_rows = lines
            .iter()
            .map(|line| {
                let plain = strip_terminal_sequences(line);
                let fragment =
                    slice_by_column(&plain, usize::from(left), visible_width(&plain), false);
                let trimmed = fragment.trim_end();
                let (start, end) = if trimmed.is_empty() {
                    // A whitespace-only soft wrap is not a logical blank line.
                    // Consume only whitespace actually represented by this row,
                    // never search ahead past unpainted source to a newline.
                    let start = cursor;
                    let matched = text[cursor..]
                        .chars()
                        .zip(fragment.chars())
                        .take_while(|(source, shown)| *source != '\n' && source == shown)
                        .map(|(source, _)| source.len_utf8())
                        .sum::<usize>();
                    let end = start + matched;
                    cursor = end + usize::from(text[end..].starts_with('\n'));
                    (start, end)
                } else if let Some(offset) = text[cursor..].find(trimmed) {
                    let start = cursor + offset;
                    let end = start + trimmed.len();
                    cursor = end;
                    (start, end)
                } else {
                    // Copy alignment must not take down the paint path. Leave
                    // the cursor intact so later rows can still match.
                    (cursor, cursor)
                };
                Some(ToolCopyRow {
                    source: start..end,
                    left,
                })
            })
            .collect();
        Self {
            lines,
            text,
            copy_rows,
        }
    }

    /// Keep only represented source, preserving whitespace between adjacent
    /// wrapped rows but never bridging an omitted source interval.
    pub(in crate::tui::view) fn retain_rows(&mut self, retained: &[usize]) {
        let mut text = String::new();
        let mut previous: Option<(usize, usize)> = None;
        let mut rows = Vec::with_capacity(retained.len());
        let mut lines = Vec::with_capacity(retained.len());
        for &index in retained {
            lines.push(self.lines[index].clone());
            let row = self.copy_rows[index].as_ref().map(|row| {
                if let Some((before, end)) = previous {
                    let gap = &self.text[end..row.source.start];
                    if self.copy_rows[before + 1..index]
                        .iter()
                        .any(Option::is_some)
                        || !gap.chars().all(char::is_whitespace)
                    {
                        text.push('\n');
                    } else {
                        text.push_str(gap);
                    }
                } else if index == 0 {
                    text.push_str(&self.text[..row.source.start]);
                }
                let start = text.len();
                text.push_str(&self.text[row.source.clone()]);
                previous = Some((index, row.source.end));
                ToolCopyRow {
                    source: start..text.len(),
                    left: row.left,
                }
            });
            rows.push(row);
        }
        self.lines = lines;
        self.copy_rows = rows;
        self.text = text;
    }

    pub(in crate::tui::view) fn prefix(&mut self, prefix: &str) {
        self.text.insert_str(0, prefix);
        for row in self.copy_rows.iter_mut().flatten() {
            row.source.start += prefix.len();
            row.source.end += prefix.len();
        }
    }

    pub(in crate::tui::view) fn decoration(&mut self, line: String) {
        self.lines.push(line);
        self.copy_rows.push(None);
    }

    pub(in crate::tui::view) fn append(&mut self, mut other: Self) {
        let offset = if other.text.is_empty() {
            self.text.len()
        } else {
            if !self.text.is_empty() {
                self.text.push('\n');
            }
            let offset = self.text.len();
            self.text.push_str(&other.text);
            offset
        };
        for row in other.copy_rows.iter_mut().flatten() {
            row.source.start += offset;
            row.source.end += offset;
        }
        self.lines.extend(other.lines);
        self.copy_rows.extend(other.copy_rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_whitespace_rows_preserve_offsets_and_only_consume_adjacent_newlines() {
        for suffix in ["foo\nbar", "\nfoo\nbar"] {
            let text = format!("{}{suffix}", " ".repeat(8));
            let mut rows = ToolRows::literal(
                ["    ", "    ", "foo", "bar"].map(str::to_owned).to_vec(),
                text.clone(),
                0,
            );
            let foo = text.find("foo").unwrap();
            let bar = text.find("bar").unwrap();
            assert_eq!(
                rows.copy_rows
                    .iter()
                    .flatten()
                    .map(|row| row.source.clone())
                    .collect::<Vec<_>>(),
                [0..4, 4..8, foo..foo + 3, bar..bar + 3]
            );
            rows.retain_rows(&[0, 1, 2, 3]);
            assert_eq!(rows.text, text);
        }
    }

    #[test]
    fn literal_mismatches_map_empty_ranges_without_advancing_the_cursor() {
        let mut rows = ToolRows::literal(
            ["missing", "界", "mismatch", "bar", "missing"]
                .map(str::to_owned)
                .to_vec(),
            "界\nbar".into(),
            0,
        );
        assert_eq!(
            rows.copy_rows
                .iter()
                .flatten()
                .map(|row| row.source.clone())
                .collect::<Vec<_>>(),
            [0..0, 0..3, 3..3, 4..7, 7..7]
        );
        rows.retain_rows(&[0, 1, 2, 3, 4]);
        assert_eq!(rows.text, "界\nbar");
    }
}
