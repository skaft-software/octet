//! The append-only tail used while a document is still streaming.
//!
//! Re-rendering a whole transcript on every delta is what makes a streaming UI
//! stutter, so a block that is still growing is tracked here instead: source
//! offsets are retained, complete logical rows are promoted to stable, and only
//! the unproven visual tail is re-laid-out. Nothing in this module is a parser
//! commit — semantic promotion may still replace these rows — which is why the
//! counters in [`StreamingLayoutStats`] are reported separately from the
//! CommonMark ones.
//!
//! It is separate from the static layouts because it is the one place that has
//! to agree with all of them at once: a row it calls stable must be byte-for-
//! byte what [`super::blocks`] would have produced, so it measures the same
//! [`CodeLayout`] and reuses the same wrapping and sanitizing path.

use unicode_segmentation::UnicodeSegmentation;

use super::code::{measured_prefix, visible_code_language, CodeLayout};
use super::lines::{inline_source_bytes, push_run, run_bytes, units, RichLine, RichRun};
use super::{CodeOverflow, RenderedLine, RichRenderer, StreamingLayoutStats};

use crate::rich_text::{Block, CodeBlock, Inline};
use crate::style::TextRole;

/// An append-only literal preview retains source offsets (transformed offsets
/// for code). Only complete logical rows and proven visual wraps become stable.
/// These are NOT parser commits: semantic promotion may replace them.
#[derive(Clone, Debug, Default)]
pub(in crate::rich_text) struct AppendOnlyTail {
    checked: usize,
    disabled: bool,
    restart: usize,
    stable_rows: usize,
    stable_nonempty: usize,
    measure_start: usize,
    completed_width: usize,
    code_layout: Option<CodeLayout>,
    language_width: Option<usize>,
    skip_space: bool,
    // Only the bounded semantic prefix is owned here; growing Raw text remains
    // in the document. Offsets address flattened prefix + borrowed Raw bytes.
    paragraph_prefix: Option<Vec<RichRun>>,
    paragraph_prefix_bytes: usize,
    code_text: AppendCodeText,
}

/// Sanitized code with tab stops carried across deltas. Only the final EGC is
/// replayed: an appended combining mark or ZWJ may change its display width.
#[derive(Clone, Debug, Default)]
struct AppendCodeText {
    text: String,
    raw_checked: usize,
    pending: String,
    pending_start: usize,
    column: usize,
}

impl AppendCodeText {
    fn append(
        &mut self,
        source: &str,
        renderer: &RichRenderer,
        stats: &mut StreamingLayoutStats,
    ) -> bool {
        let delta = &source[self.raw_checked..];
        stats.checked_bytes += delta.len() as u64;
        // Static code layout handles CR per original source line, including a
        // special terminal CRLF. Keep that authoritative path for now.
        if delta.contains('\r') {
            return false;
        }
        // Sanitization is character-local except CRLF (excluded above): even a
        // fragmented ESC/OSC/CSI becomes visible text, never terminal commands.
        let safe = renderer.sanitize(delta);
        stats.copied_bytes += safe.len() as u64;
        self.pending.push_str(&safe);
        self.text.truncate(self.pending_start);
        let mut column = self.column;
        let mut last = 0;
        stats.checked_bytes += self.pending.len() as u64;
        for (offset, grapheme) in self.pending.grapheme_indices(true) {
            last = offset;
            self.pending_start = self.text.len();
            self.column = column;
            let cells = renderer.options.width.grapheme_width(grapheme, column);
            if grapheme == "\t" {
                self.text.extend(std::iter::repeat_n(' ', cells));
                stats.copied_bytes += cells as u64;
            } else {
                self.text.push_str(grapheme);
                stats.copied_bytes += grapheme.len() as u64;
            }
            column = if grapheme == "\n" {
                0
            } else {
                column.saturating_add(cells)
            };
        }
        self.pending.drain(..last);
        self.raw_checked = source.len();
        true
    }
}

impl AppendOnlyTail {
    pub(in crate::rich_text) fn update(
        &mut self,
        block: &Block,
        renderer: &RichRenderer,
        width: u16,
        output: &mut Vec<RenderedLine>,
        stats: &mut StreamingLayoutStats,
    ) -> Option<(usize, usize)> {
        if self.disabled {
            return None;
        }
        if let Block::CodeBlock(code) = block {
            if code.language.as_deref().is_some_and(|language| {
                language.eq_ignore_ascii_case("diff") || language.eq_ignore_ascii_case("patch")
            }) {
                return None;
            }
            if !self.code_text.append(&code.code, renderer, stats) {
                stats.literal_transform_fallbacks += 1;
                self.disabled = true;
                return None;
            }
            // Move, never clone, the growing transformed source across layout.
            let normalized = Block::CodeBlock(CodeBlock {
                language: code.language.clone(),
                code: std::mem::take(&mut self.code_text.text),
            });
            let result = self.update_inner(&normalized, renderer, width, output, stats);
            let Block::CodeBlock(code) = normalized else {
                unreachable!()
            };
            self.code_text.text = code.code;
            return result;
        }
        self.update_inner(block, renderer, width, output, stats)
    }

    fn update_inner(
        &mut self,
        block: &Block,
        renderer: &RichRenderer,
        width: u16,
        output: &mut Vec<RenderedLine>,
        stats: &mut StreamingLayoutStats,
    ) -> Option<(usize, usize)> {
        if self.disabled {
            return None;
        }
        let mut prefix_newlines = Vec::new();
        let (source, code) = match block {
            Block::Plain(source) => (source.as_str(), None),
            Block::Paragraph(content) => {
                let (Inline::Raw(source), prefix) = content.split_last()? else {
                    return None;
                };
                if self.paragraph_prefix.is_none() {
                    // A canonical prose preview is represented by an empty
                    // semantic prefix followed by the growing Raw suffix. Do
                    // not route that empty prefix through rich layout: doing
                    // so charges a rich-prefix layout (and invites prefix work) even
                    // though there is no immutable content to flatten.
                    if prefix.is_empty() {
                        self.paragraph_prefix_bytes = 0;
                        self.paragraph_prefix = Some(Vec::new());
                    } else {
                        // The stream owns prefix identity and resets this cache on
                        // semantic replacement, width/options/theme changes. Never
                        // hash, clone, or compare the growing paragraph here.
                        stats.checked_bytes += inline_source_bytes(prefix) as u64;
                        let runs =
                            renderer.inline_runs(prefix, renderer.theme.style(TextRole::Text));
                        stats.copied_bytes += run_bytes(&runs) as u64;
                        let runs = renderer.expand_run_tabs(&runs);
                        stats.copied_bytes += run_bytes(&runs) as u64;
                        let mut offset = 0;
                        for run in &runs {
                            prefix_newlines
                                .extend(run.text.match_indices('\n').map(|(i, _)| offset + i));
                            offset += run.text.len();
                        }
                        stats.checked_bytes += offset as u64;
                        stats.rich_prefix_layouts += 1;
                        self.paragraph_prefix_bytes = offset;
                        self.paragraph_prefix = Some(runs);
                    }
                }
                (source.as_str(), None)
            }
            Block::CodeBlock(code)
                if !code.language.as_deref().is_some_and(|language| {
                    language.eq_ignore_ascii_case("diff") || language.eq_ignore_ascii_case("patch")
                }) =>
            {
                (code.code.as_str(), Some(code))
            }
            _ => return None,
        };
        let prefix_bytes = self.paragraph_prefix_bytes;
        let source_len = prefix_bytes + source.len();
        let checked_raw = self.checked.saturating_sub(prefix_bytes);
        let delta = &source[checked_raw..];
        stats.checked_bytes += delta.len() as u64;
        // Tabs need original logical columns; CRLF and sanitization need source
        // maps. Keep their authoritative general layout rather than guessing at
        // offsets in transformed text. Eligibility checks inspect only the delta.
        let sanitized = renderer.sanitize(delta);
        stats.copied_bytes += sanitized.len() as u64;
        if delta.contains(['\t', '\r']) || sanitized != delta {
            stats.literal_transform_fallbacks += 1;
            self.disabled = true;
            return None;
        }
        let width = if code.is_some() {
            usize::from(width)
        } else {
            renderer.prose_width(usize::from(width))
        };
        // Prose at width zero has exactly one empty physical row regardless of
        // logical newlines. Do not replay a growing invisible frontier.
        if width == 0 && code.is_none() {
            output.clear();
            output.push(RenderedLine::default());
            self.checked = source_len;
            self.restart = source_len;
            return Some((0, 1));
        }
        let mut newlines = prefix_newlines;
        newlines.extend(
            delta
                .match_indices('\n')
                .map(|(offset, _)| prefix_bytes + checked_raw + offset),
        );
        let mut stable_prefix = self.stable_rows;
        let mut reflow = self.checked == 0;
        if let Some(code) = code {
            for &end in &newlines {
                let (_, cells) = measured_prefix(
                    &source[self.measure_start..end],
                    width,
                    renderer.options.width,
                    &mut stats.measured_bytes,
                );
                self.completed_width = self.completed_width.max(cells.min(width));
                self.measure_start = end + 1;
            }
            let (_, cells) = measured_prefix(
                &source[self.measure_start..],
                width,
                renderer.options.width,
                &mut stats.measured_bytes,
            );
            let language_width = *self.language_width.get_or_insert_with(|| {
                visible_code_language(code).map_or(0, |label| {
                    stats.measured_bytes += label.len() as u64;
                    renderer.options.width.line_width(&renderer.sanitize(label))
                })
            });
            let layout = renderer.code_layout(
                language_width,
                width,
                self.completed_width.max(cells.min(width)),
            );
            if self.code_layout != Some(layout) {
                self.code_layout = Some(layout);
                reflow = true;
            }
        }
        if reflow {
            self.restart = 0;
            self.skip_space = false;
            self.stable_rows = 0;
            self.stable_nonempty = 0;
            stable_prefix = 0;
            if self.checked > 0 {
                stats.checked_bytes += source.len() as u64;
                newlines = source
                    .match_indices('\n')
                    .map(|(offset, _)| offset)
                    .collect();
            }
            output.clear();
            if let Some(layout) = &self.code_layout {
                let header = renderer.code_header(layout, visible_code_language(code.unwrap()));
                self.append_rows(header, renderer, width, output, stats);
            }
        }
        output.truncate(self.stable_rows);
        let mut visible = self.stable_nonempty;
        for end in newlines {
            let (rows, _, _, _) =
                self.source_rows(source, self.restart, end, renderer, width, stats);
            self.append_rows(rows, renderer, width, output, stats);
            visible = self.stable_nonempty;
            self.restart = end + 1;
            self.skip_space = false;
        }
        // Code's final LF terminates the preceding row, unlike a Plain block's
        // split('\n'). Empty code still has one body row beneath its label.
        if code.is_none() || !source.ends_with('\n') || source.is_empty() {
            let (rows, stable, restart, skip_space) =
                self.source_rows(source, self.restart, source_len, renderer, width, stats);
            let base = output.len();
            for (index, row) in rows.into_iter().enumerate() {
                if !row.is_empty() {
                    visible = output.len() + 1;
                }
                stats.encoded_rows += 1;
                let row = renderer.encode_line(row, width);
                stats.copied_bytes += (row.plain.len() + row.styled.len()) as u64;
                output.push(row);
                if index < stable {
                    self.stable_rows = base + index + 1;
                    self.stable_nonempty = visible;
                }
            }
            self.restart += restart;
            self.skip_space = skip_space;
        }
        if let Some(layout) = &self.code_layout {
            for row in renderer.code_footer(layout) {
                if !row.is_empty() {
                    visible = output.len() + 1;
                }
                stats.encoded_rows += 1;
                let row = renderer.encode_line(row, width);
                stats.copied_bytes += (row.plain.len() + row.styled.len()) as u64;
                output.push(row);
            }
        }
        // Retain trailing empty stable rows internally so a long blank suffix
        // is not rescanned. Expose exactly render_blocks' trimmed physical view.
        if output.is_empty() {
            output.push(RenderedLine::default());
        }
        self.checked = source_len;
        Some((stable_prefix, visible.max(1)))
    }

    fn source_rows(
        &self,
        source: &str,
        start: usize,
        end: usize,
        renderer: &RichRenderer,
        width: usize,
        stats: &mut StreamingLayoutStats,
    ) -> (Vec<RichLine>, usize, usize, bool) {
        let prefix_bytes = self.paragraph_prefix_bytes;
        if start >= prefix_bytes {
            return self.rows(
                &source[start - prefix_bytes..end - prefix_bytes],
                renderer,
                width,
                stats,
            );
        }
        // Replay just the mutable visual frontier, preserving run boundaries:
        // grapheme segmentation is per semantic run in the authoritative layout.
        let mut runs = Vec::new();
        let mut offset = 0;
        for run in self.paragraph_prefix.as_ref().unwrap() {
            let run_end = offset + run.text.len();
            if run_end >= start && offset <= end {
                let text = run.text
                    [start.saturating_sub(offset)..(end - offset).min(run.text.len())]
                    .to_owned();
                runs.push(RichRun::new(text, run.style, run.link.clone()));
            }
            offset = run_end;
            if offset > end {
                break;
            }
        }
        if end >= prefix_bytes {
            // flatten_inline merges the last prefix run with Raw exactly when
            // their style/link agree. This also re-segments a split final EGC.
            push_run(
                &mut runs,
                source[..end - prefix_bytes].to_owned(),
                renderer.theme.style(TextRole::Text),
                None,
            );
        }
        stats.copied_bytes += run_bytes(&runs) as u64;
        stats.laid_out_bytes += (end - start) as u64;
        let units = units(&runs, renderer.options.width);
        if units.is_empty() {
            return (vec![RichLine::default()], 0, 0, false);
        }
        let leading = if self.skip_space {
            match units.iter().position(|unit| !unit.whitespace) {
                Some(leading) => leading,
                None => return (Vec::new(), 0, units.last().unwrap().source_start, true),
            }
        } else {
            0
        };
        let (rows, stable, restart, skip_space) =
            renderer.wrap_literal_units(&runs, &units[leading..], width, true);
        (
            rows,
            stable,
            restart.max(units[leading].source_start),
            skip_space,
        )
    }

    fn rows(
        &self,
        source: &str,
        renderer: &RichRenderer,
        width: usize,
        stats: &mut StreamingLayoutStats,
    ) -> (Vec<RichLine>, usize, usize, bool) {
        let mut leading = 0;
        if self.skip_space {
            let mut last = 0;
            let mut nonspace = false;
            for (offset, grapheme) in source.grapheme_indices(true) {
                last = offset;
                if !grapheme.chars().all(char::is_whitespace) {
                    leading = offset;
                    nonspace = true;
                    break;
                }
            }
            if !nonspace {
                stats.laid_out_bytes += source.len() as u64;
                return (Vec::new(), 0, last, true);
            }
        }
        let source = &source[leading..];
        stats.laid_out_bytes += leading as u64;
        let (content_width, style) = self
            .code_layout
            .map_or((width, renderer.theme.style(TextRole::Text)), |layout| {
                (layout.content_width, layout.code_text_style)
            });
        let (rows, stable, restart, skip_space) =
            if self.code_layout.is_some() && renderer.options.code_overflow == CodeOverflow::Clip {
                // Once overflow is proven, code clipping cannot depend on hidden
                // suffix bytes. Keep every source byte for copy/final parsing, but
                // submit only the exact visible prefix plus overflow witness.
                let (prefix, _) = measured_prefix(
                    source,
                    content_width,
                    renderer.options.width,
                    &mut stats.measured_bytes,
                );
                stats.laid_out_bytes += prefix.len() as u64;
                stats.copied_bytes += prefix.len() as u64;
                let runs = [RichRun::new(prefix.to_owned(), style, None)];
                (vec![renderer.clip_runs(&runs, content_width)], 0, 0, false)
            } else {
                stats.laid_out_bytes += source.len() as u64;
                stats.copied_bytes += source.len() as u64;
                renderer.literal_rows(source, content_width, style, self.code_layout.is_none())
            };
        let rows = rows
            .into_iter()
            .map(|row| match self.code_layout {
                Some(layout) => layout.row(renderer, row),
                None => row,
            })
            .collect();
        (rows, stable, restart + leading, skip_space)
    }

    fn append_rows(
        &mut self,
        rows: Vec<RichLine>,
        renderer: &RichRenderer,
        width: usize,
        output: &mut Vec<RenderedLine>,
        stats: &mut StreamingLayoutStats,
    ) {
        for row in rows {
            if !row.is_empty() {
                self.stable_nonempty = output.len() + 1;
            }
            stats.encoded_rows += 1;
            let row = renderer.encode_line(row, width);
            stats.copied_bytes += (row.plain.len() + row.styled.len()) as u64;
            output.push(row);
        }
        self.stable_rows = output.len();
    }
}
