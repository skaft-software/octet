//! The styled-row primitives every block layout is built from.
//!
//! A [`RichLine`] is a row of [`RichRun`]s and a `Unit` is one grapheme with
//! its measured cell width. Splitting a run list into units, breaking it at
//! newlines and turning a row back into runs are operations that every block
//! kind needs and that belong to no single block kind. They live here so the
//! block layouts never re-derive cell measurement, and so a change to how a
//! run is measured lands in exactly one place.

use unicode_segmentation::UnicodeSegmentation;

use crate::rich_text::Inline;
use crate::style::TextStyle;
use crate::width::WidthPolicy;

pub(super) fn run_bytes(runs: &[RichRun]) -> usize {
    runs.iter()
        .map(|run| run.text.len() + run.link.as_ref().map_or(0, String::len))
        .sum()
}

// Count the bounded semantic input, including link targets and transformed
// source bytes, rather than charging only its possibly shorter display text.
pub(super) fn inline_source_bytes(content: &[Inline]) -> usize {
    content
        .iter()
        .map(|inline| match inline {
            Inline::Text(text) | Inline::Raw(text) | Inline::Code(text) => text.len(),
            Inline::Styled(span) => inline_source_bytes(&span.content),
            Inline::Role { content, .. }
            | Inline::Status { content, .. }
            | Inline::Emphasis(content)
            | Inline::Strong(content)
            | Inline::Strikethrough(content) => inline_source_bytes(content),
            Inline::Link { label, target } => inline_source_bytes(label) + target.len(),
            Inline::SoftBreak | Inline::HardBreak => 1,
        })
        .sum()
}

#[derive(Clone, Debug, Default)]
pub(super) struct RichLine {
    pub(super) runs: Vec<RichRun>,
}

impl RichLine {
    pub(super) fn from_plain(text: String) -> Self {
        let mut line = Self::default();
        line.push(text, TextStyle::plain(), None);
        line
    }

    pub(super) fn push(&mut self, text: String, style: TextStyle, link: Option<String>) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self
            .runs
            .last_mut()
            .filter(|last| last.style == style && last.link == link)
        {
            last.text.push_str(&text);
        } else {
            self.runs.push(RichRun::new(text, style, link));
        }
    }

    pub(super) fn extend(&mut self, other: RichLine) {
        for run in other.runs {
            self.push(run.text, run.style, run.link);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }
}

#[derive(Clone, Debug)]
pub(super) struct RichRun {
    pub(super) text: String,
    pub(super) style: TextStyle,
    pub(super) link: Option<String>,
}

impl RichRun {
    pub(super) fn new(text: String, style: TextStyle, link: Option<String>) -> Self {
        Self { text, style, link }
    }
}

pub(super) fn push_run(
    output: &mut Vec<RichRun>,
    text: String,
    style: TextStyle,
    link: Option<String>,
) {
    if let Some(last) = output
        .last_mut()
        .filter(|last| last.style == style && last.link == link)
    {
        last.text.push_str(&text);
    } else {
        output.push(RichRun::new(text, style, link));
    }
}

pub(super) fn push_blank(output: &mut Vec<RichLine>) {
    if !output.last().is_some_and(RichLine::is_empty) {
        output.push(RichLine::default());
    }
}

pub(super) fn split_runs_at_newlines(runs: &[RichRun]) -> Vec<Vec<RichRun>> {
    let mut output = vec![Vec::new()];
    for run in runs {
        for (index, part) in run.text.split('\n').enumerate() {
            if index > 0 {
                output.push(Vec::new());
            }
            if !part.is_empty() {
                output.last_mut().unwrap().push(RichRun::new(
                    part.to_owned(),
                    run.style,
                    run.link.clone(),
                ));
            }
        }
    }
    output
}

pub(super) struct Unit {
    pub(super) run: usize,
    // Offset in concatenated runs, while start/end remain run-local for slicing.
    pub(super) source_start: usize,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) width: usize,
    pub(super) whitespace: bool,
}

pub(super) fn units(runs: &[RichRun], policy: WidthPolicy) -> Vec<Unit> {
    let mut output = Vec::new();
    let mut column = 0usize;
    let mut source_offset = 0usize;
    for (run_index, run) in runs.iter().enumerate() {
        for (start, grapheme) in run.text.grapheme_indices(true) {
            let width = policy.grapheme_width(grapheme, column);
            output.push(Unit {
                run: run_index,
                source_start: source_offset + start,
                start,
                end: start + grapheme.len(),
                width,
                whitespace: grapheme.chars().all(char::is_whitespace),
            });
            column = column.saturating_add(width);
        }
        source_offset += run.text.len();
    }
    output
}

pub(super) fn line_from_units(runs: &[RichRun], units: &[Unit]) -> RichLine {
    let mut line = RichLine::default();
    let mut index = 0usize;
    while index < units.len() {
        let first = &units[index];
        let mut end = first.end;
        let mut next = index + 1;
        while next < units.len() && units[next].run == first.run && units[next].start == end {
            end = units[next].end;
            next += 1;
        }
        let source = &runs[first.run];
        line.push(
            source.text[first.start..end].to_owned(),
            source.style,
            source.link.clone(),
        );
        index = next;
    }
    line
}
