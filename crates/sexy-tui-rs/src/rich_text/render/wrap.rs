//! Inline flattening, wrapping, clipping and the terminal-boundary sanitizer.
//!
//! These are the operations that turn a semantic inline into display rows, and
//! they are the only place that decides what a cell is allowed to contain. The
//! sanitizer is here rather than in `render` because it is the same policy for
//! every caller — block rows, diff rows and the streaming tail all reach it
//! through one method — and because a layout must never be able to emit a
//! terminal control sequence that did not pass through it.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;

use super::lines::{
    line_from_units, push_run, split_runs_at_newlines, units, RichLine, RichRun, Unit,
};
use super::RenderedLine;
use super::RichRenderer;

use crate::capabilities::SupportLevel;
use crate::glyphs::GlyphSet;
use crate::rich_text::{Inline, StatusKind};
use crate::sanitize::{sanitize_text, ControlPictures, SafeUrl, SanitizeOptions};
use crate::style::{Color, TextRole, TextStyle};

impl RichRenderer {
    pub(super) fn inline_runs(&self, content: &[Inline], base: TextStyle) -> Vec<RichRun> {
        let mut runs = Vec::new();
        self.flatten_inline(content, base, None, &mut runs);
        runs
    }

    pub(super) fn flatten_inline(
        &self,
        content: &[Inline],
        base: TextStyle,
        link: Option<&str>,
        output: &mut Vec<RichRun>,
    ) {
        for inline in content {
            match inline {
                Inline::Text(text) | Inline::Raw(text) => {
                    push_run(output, self.sanitize(text), base, link.map(str::to_owned));
                }
                Inline::Styled(span) => {
                    self.flatten_inline(&span.content, base.merge(span.style), link, output)
                }
                Inline::Role { role, content } => {
                    self.flatten_inline(content, base.merge(self.theme.style(*role)), link, output)
                }
                Inline::Status { kind, content } => {
                    let glyphs = GlyphSet::for_capabilities(self.capabilities);
                    let (marker, role) = match kind {
                        StatusKind::Success => (glyphs.success, TextRole::Success),
                        StatusKind::Warning => (glyphs.warning, TextRole::Warning),
                        StatusKind::Error => (glyphs.error, TextRole::Error),
                        StatusKind::Pending => (glyphs.pending, TextRole::Muted),
                    };
                    let style = base.merge(self.theme.style(role));
                    push_run(output, format!("{marker} "), style, link.map(str::to_owned));
                    self.flatten_inline(content, style, link, output);
                }
                Inline::Emphasis(content) => {
                    let mut style = self.theme.style(TextRole::Emphasis);
                    // Unsupported italics should degrade to regular text, not
                    // underline: underline is reserved for explicit Markdown
                    // links/underline semantics.
                    if self.capabilities.italics != SupportLevel::Supported {
                        style.attributes.italic = false;
                    }
                    self.flatten_inline(content, base.merge(style), link, output);
                }
                Inline::Strong(content) => self.flatten_inline(
                    content,
                    base.merge(self.theme.style(TextRole::Strong)),
                    link,
                    output,
                ),
                Inline::Strikethrough(content) => {
                    let mut style = TextStyle::plain();
                    style.attributes.strikethrough = true;
                    self.flatten_inline(content, base.merge(style), link, output);
                }
                Inline::Code(code) => {
                    let mut style = base.merge(self.theme.style(TextRole::InlineCode));
                    // Inline code is a technical surface, not prose. Do not
                    // inherit an enclosing paragraph's faint/italic treatment
                    // (for example, from a subdued reasoning renderer).
                    style.attributes.dim = false;
                    style.attributes.italic = false;
                    // Backgrounds are a theme decision, never inferred from
                    // content. The terminal-neutral default is foreground-only.
                    if let Some(background) = self
                        .theme
                        .resolve_color("md_code_inline_bg")
                        .filter(|color| *color != Color::Default)
                    {
                        style.background = background;
                    }
                    push_run(output, self.sanitize(code), style, link.map(str::to_owned));
                }
                Inline::Link { label, target } => {
                    let before = output.len();
                    self.flatten_inline(
                        label,
                        base.merge(self.theme.style(TextRole::Link)),
                        Some(target),
                        output,
                    );
                    let label_text = output[before..]
                        .iter()
                        .map(|run| run.text.as_str())
                        .collect::<String>();
                    let safe_target = self.sanitize(target);
                    if label_text.trim() != safe_target.trim() {
                        push_run(
                            output,
                            format!(" ({safe_target})"),
                            base.merge(self.theme.style(TextRole::Link)),
                            Some(target.clone()),
                        );
                    }
                }
                Inline::SoftBreak => push_run(output, " ".into(), base, link.map(str::to_owned)),
                Inline::HardBreak => push_run(output, "\n".into(), base, link.map(str::to_owned)),
            }
        }
    }

    pub(super) fn wrap_runs(&self, runs: &[RichRun], width: usize) -> Vec<RichLine> {
        if width == 0 {
            return vec![RichLine::default()];
        }
        let runs = self.expand_run_tabs(runs);
        let logical = split_runs_at_newlines(&runs);
        let mut output = Vec::new();
        for line_runs in logical {
            let units = units(&line_runs, self.options.width);
            if units.is_empty() {
                output.push(RichLine::default());
                continue;
            }
            output.extend(self.wrap_literal_units(&line_runs, &units, width, true).0);
        }
        if output.is_empty() {
            output.push(RichLine::default());
        }
        output
    }

    // In addition to rows, report a prefix whose wrap decisions do not depend
    // on the final (still extendable) grapheme. The caller keeps the source for
    // the remaining one or two visual rows, not the entire logical paragraph.
    pub(super) fn wrap_literal_units(
        &self,
        line_runs: &[RichRun],
        units: &[Unit],
        width: usize,
        prose: bool,
    ) -> (Vec<RichLine>, usize, usize, bool) {
        let mut output = Vec::new();
        let mut stable_rows = 0;
        let mut restart = 0;
        let mut skip_space = false;
        let mut start = 0usize;
        while start < units.len() {
            let mut end = start;
            let mut cells = 0usize;
            let mut last_space = None;
            while end < units.len() {
                let unit = &units[end];
                if cells.saturating_add(unit.width) > width {
                    break;
                }
                cells += unit.width;
                if unit.whitespace {
                    last_space = Some(end);
                }
                end += 1;
            }
            if end == start {
                // A wide grapheme cannot fit at width one. A visible ASCII
                // fallback keeps the line within the promised cell bound.
                let mut replacement = RichLine::default();
                let source = &line_runs[units[start].run];
                replacement.push("?".into(), source.style, source.link.clone());
                output.push(replacement);
                start += 1;
                if start + 1 < units.len() {
                    stable_rows = output.len();
                    restart = units[start].source_start;
                }
                continue;
            }

            let mut next = end;
            let line_end = if end < units.len() {
                if let Some(space) = last_space.filter(|space| prose && *space > start) {
                    next = space + 1;
                    while prose && next < units.len() && units[next].whitespace {
                        next += 1;
                    }
                    space
                } else {
                    end
                }
            } else {
                end
            };
            while prose && next < units.len() && units[next].whitespace {
                next += 1;
            }
            output.push(line_from_units(line_runs, &units[start..line_end]));
            if end + 1 < units.len() && next + 1 < units.len() {
                stable_rows = output.len();
                restart = units[next].source_start;
                skip_space = false;
            } else if prose
                && end + 1 < units.len()
                && next == units.len()
                && units.last().is_some_and(|unit| unit.whitespace)
            {
                // The wrap is proven, but the skipped final whitespace
                // grapheme can still acquire a combining suffix. Retain it
                // and the skip state, not the whole growing whitespace run.
                stable_rows = output.len();
                restart = units.last().unwrap().source_start;
                skip_space = true;
            }
            start = next;
        }
        (output, stable_rows, restart, skip_space)
    }

    pub(super) fn literal_rows(
        &self,
        source: &str,
        width: usize,
        style: TextStyle,
        prose: bool,
    ) -> (Vec<RichLine>, usize, usize, bool) {
        if width == 0 || source.is_empty() {
            return (vec![RichLine::default()], 0, 0, false);
        }
        let runs = [RichRun::new(source.to_owned(), style, None)];
        let units = units(&runs, self.options.width);
        self.wrap_literal_units(&runs, &units, width, prose)
    }
    /// Code wrapping is deliberately literal: unlike prose wrapping it never
    /// discards indentation or separator whitespace to find a word boundary.
    pub(super) fn hard_wrap_runs(&self, runs: &[RichRun], width: usize) -> Vec<RichLine> {
        if width == 0 {
            return vec![RichLine::default()];
        }
        let runs = self.expand_run_tabs(runs);
        let logical = split_runs_at_newlines(&runs);
        let mut output = Vec::new();
        for line_runs in logical {
            let units = units(&line_runs, self.options.width);
            if units.is_empty() {
                output.push(RichLine::default());
                continue;
            }
            let mut start = 0usize;
            while start < units.len() {
                let mut end = start;
                let mut cells = 0usize;
                while end < units.len() && cells.saturating_add(units[end].width) <= width {
                    cells = cells.saturating_add(units[end].width);
                    end += 1;
                }
                if end == start {
                    let source = &line_runs[units[start].run];
                    let mut replacement = RichLine::default();
                    replacement.push("?".into(), source.style, source.link.clone());
                    output.push(replacement);
                    start += 1;
                } else {
                    output.push(line_from_units(&line_runs, &units[start..end]));
                    start = end;
                }
            }
        }
        if output.is_empty() {
            output.push(RichLine::default());
        }
        output
    }

    pub(super) fn clip_runs(&self, runs: &[RichRun], width: usize) -> RichLine {
        if width == 0 {
            return RichLine::default();
        }
        let runs = self.expand_run_tabs(runs);
        let logical = split_runs_at_newlines(&runs);
        let line_runs = logical.first().cloned().unwrap_or_default();
        if self.runs_width(&line_runs) <= width {
            return RichLine { runs: line_runs };
        }
        let glyphs = GlyphSet::for_capabilities(self.capabilities);
        let indicator = glyphs.ellipsis;
        let indicator_width = self.options.width.line_width(indicator).min(width);
        let available = width.saturating_sub(indicator_width);
        let line_units = units(&line_runs, self.options.width);
        let mut end = 0;
        let mut cells = 0usize;
        while end < line_units.len() && cells + line_units[end].width <= available {
            cells += line_units[end].width;
            end += 1;
        }
        let mut line = line_from_units(&line_runs, &line_units[..end]);
        let indicator = if indicator_width == 0 {
            ""
        } else if self.options.width.line_width(indicator) <= width {
            indicator
        } else {
            "."
        };
        line.push(
            indicator.to_owned(),
            self.theme.style(TextRole::Subtle),
            None,
        );
        line
    }

    pub(super) fn clip_line(&self, line: RichLine, width: usize) -> RichLine {
        // Wrapped rows already fit. Keep their owned runs instead of expanding,
        // splitting and cloning them again at the encoding boundary. Empty runs
        // and line/tab controls retain clip_runs' exact projection semantics.
        if width > 0
            && line
                .runs
                .iter()
                .all(|run| !run.text.is_empty() && !run.text.contains(['\t', '\r', '\n']))
            && self.runs_width(&line.runs) <= width
        {
            return line;
        }
        self.clip_runs(&line.runs, width)
    }

    pub(super) fn expand_run_tabs<'a>(&self, runs: &'a [RichRun]) -> Cow<'a, [RichRun]> {
        // A column is needed only if a later run contains a tab. Do not copy
        // or re-segment all the no-tab prose/code merely to rediscover its width.
        if !runs.iter().any(|run| run.text.contains('\t')) {
            return Cow::Borrowed(runs);
        }
        let mut column = 0usize;
        Cow::Owned(
            runs.iter()
                .map(|run| {
                    let expanded = self
                        .options
                        .width
                        .expand_tabs(&run.text, column)
                        .into_owned();
                    for grapheme in expanded.graphemes(true) {
                        if matches!(grapheme, "\n" | "\r") {
                            column = 0;
                        } else {
                            column = column.saturating_add(
                                self.options.width.grapheme_width(grapheme, column),
                            );
                        }
                    }
                    RichRun::new(expanded, run.style, run.link.clone())
                })
                .collect(),
        )
    }

    pub(super) fn runs_width(&self, runs: &[RichRun]) -> usize {
        let mut column = 0usize;
        for run in runs {
            column = column.saturating_add(self.options.width.line_width_from(&run.text, column));
        }
        column
    }

    pub(super) fn line_width(&self, line: &RichLine) -> usize {
        self.runs_width(&line.runs)
    }

    pub(super) fn encode_line(&self, line: RichLine, width: usize) -> RenderedLine {
        let line = self.clip_line(line, width);
        let mut plain = String::new();
        let mut styled = String::new();
        for run in line.runs {
            plain.push_str(&run.text);
            let styled_text = self.theme.apply_style(run.style, &run.text);
            if self.capabilities.hyperlinks {
                if let Some(target) = run.link.as_deref().and_then(SafeUrl::parse) {
                    styled.push_str("\x1b]8;;");
                    styled.push_str(target.as_str());
                    styled.push_str("\x1b\\");
                    styled.push_str(&styled_text);
                    styled.push_str("\x1b]8;;\x1b\\");
                    continue;
                }
            }
            styled.push_str(&styled_text);
        }
        RenderedLine { styled, plain }
    }

    /// Escape-free copy/log representation under this renderer's ASCII or
    /// Unicode fallback policy.
    pub fn sanitize_copy(&self, text: &str) -> String {
        self.sanitize(text)
    }

    pub(super) fn sanitize(&self, text: &str) -> String {
        sanitize_text(
            text,
            SanitizeOptions {
                controls: if self.capabilities.unicode {
                    ControlPictures::Unicode
                } else {
                    ControlPictures::Ascii
                },
                preserve_newlines: true,
                preserve_tabs: true,
            },
        )
        .into_owned()
    }
}
