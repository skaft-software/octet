//! Stream-safe Markdown with a committed prefix and bounded mutable tail.
//!
//! Complete top-level blocks are committed once a following block proves their
//! boundary. The final document is parsed once from the complete raw text, so
//! completion is semantically identical to static parsing. An unstable suffix
//! is never reparsed beyond [`MAX_UNSTABLE_PARSE_BYTES`].

use std::str;

use super::markdown;
pub use super::render::StreamingLayoutStats;
use super::render::{AppendOnlyTail, RenderOptions, RenderedDocument, RenderedLine, RichRenderer};
use super::{Block, CodeBlock, Document};

/// Maximum suffix considered by the CommonMark parser during an active stream.
pub const MAX_UNSTABLE_PARSE_BYTES: usize = 64 * 1024;
/// Small inline tails are cheap enough to keep semantically current after a
/// delimiter closes. Beyond this bound the geometric parser remains in charge.
const MAX_LIVE_INLINE_PREVIEW_BYTES: usize = 8 * 1024;

/// Streaming work counters for performance regression tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamingStats {
    /// Bytes searched for line endings, plus completed lines classified as fences.
    pub fence_scanned_bytes: u64,
    /// Bytes copied/appended into literal and open-code previews (not parser allocations).
    pub preview_copied_bytes: u64,
    /// Bytes covered by lexical blank-boundary searches and marker-prefix checks.
    /// Separate from fence scanning, parser input, and preview copying.
    pub lexical_scanned_bytes: u64,
    pub parse_passes: u64,
    pub reparsed_bytes: u64,
    pub committed_blocks: usize,
    pub pending_utf8_bytes: usize,
}

#[derive(Clone, Debug)]
struct FenceState {
    marker: char,
    count: usize,
    start: usize,
    code_start: usize,
    language: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct FenceScanner {
    offset: usize,
    searched: usize,
    scanned_bytes: u64,
    open: Option<FenceState>,
}

impl FenceScanner {
    fn scan(&mut self, source: &str) -> bool {
        let mut completed_fence = false;
        while self.offset < source.len() {
            let remaining = &source[self.searched..];
            let found = remaining.find('\n');
            let searched = found.map_or(remaining.len(), |end| end + 1);
            self.scanned_bytes += searched as u64;
            self.searched += searched;
            if found.is_none() {
                break;
            }
            let end = self.searched;
            self.scanned_bytes += (end - self.offset) as u64;
            let line = source[self.offset..end].trim_end_matches(['\r', '\n']);
            if let Some(open) = &self.open {
                if is_closing_fence(line, open.marker, open.count) {
                    self.open = None;
                    completed_fence = true;
                }
            } else if let Some((marker, count, info)) = opening_fence(line) {
                self.open = Some(FenceState {
                    marker,
                    count,
                    start: self.offset,
                    code_start: end,
                    language: info,
                });
            }
            self.offset = end;
        }
        completed_fence
    }

    fn drain_prefix(&mut self, bytes: usize) {
        self.offset = self.offset.saturating_sub(bytes);
        self.searched = self.searched.saturating_sub(bytes);
        if let Some(open) = &mut self.open {
            open.start = open.start.saturating_sub(bytes);
            open.code_start = open.code_start.saturating_sub(bytes);
        }
    }
}

/// Incremental Markdown state. `raw_bytes()` always retains the original input,
/// including invalid UTF-8 bytes used for logging or diagnostics.
#[derive(Clone, Debug, Default)]
pub struct StreamingMarkdown {
    raw: Vec<u8>,
    decoded: String,
    pending_utf8: Vec<u8>,
    committed: Document,
    tail: String,
    preview: Document,
    scanner: FenceScanner,
    lexical_first: Option<LexicalLinePrefix>,
    finished: bool,
    committed_revision: u64,
    tail_revision: u64,
    // Changes only when the preview ceases to be an append of its old value.
    preview_epoch: u64,
    next_parse_at: usize,
    tail_semantic_parsed: bool,
    stats: StreamingStats,
}

impl StreamingMarkdown {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: &str) -> Self {
        let mut stream = Self::new();
        stream.push_str(text);
        stream
    }

    pub fn push_str(&mut self, chunk: &str) {
        self.push_bytes(chunk.as_bytes());
    }

    /// Ingest arbitrary byte chunks, buffering incomplete UTF-8 scalar values
    /// and replacing only permanently malformed sequences in the display text.
    pub fn push_bytes(&mut self, chunk: &[u8]) {
        if self.finished || chunk.is_empty() {
            return;
        }
        self.raw.extend_from_slice(chunk);
        self.pending_utf8.extend_from_slice(chunk);
        let decoded_chunk = decode_available(&mut self.pending_utf8, false);
        self.stats.pending_utf8_bytes = self.pending_utf8.len();
        if decoded_chunk.is_empty() {
            return;
        }
        let had_open_fence = self.scanner.open.is_some();
        self.decoded.push_str(&decoded_chunk);
        let proven_boundary = self.tail.ends_with("\n\n")
            || decoded_chunk.contains("\n\n")
            || (self.tail.ends_with('\n') && decoded_chunk.starts_with('\n'));
        self.tail.push_str(&decoded_chunk);
        if had_open_fence {
            if let [Block::CodeBlock(code)] = self.preview.blocks.as_mut_slice() {
                code.code.push_str(&decoded_chunk);
                self.stats.preview_copied_bytes += decoded_chunk.len() as u64;
            }
        }
        let completed_fence = self.scanner.scan(&self.tail);
        self.stabilize(
            had_open_fence,
            decoded_chunk.contains('\n'),
            completed_fence,
            proven_boundary,
        );
        self.tail_revision = self.tail_revision.saturating_add(1);
    }

    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Lossy UTF-8 view used for terminal display. The raw bytes remain exact.
    pub fn raw_text(&self) -> &str {
        &self.decoded
    }

    pub fn committed(&self) -> &Document {
        &self.committed
    }

    pub fn unstable_source(&self) -> &str {
        &self.tail
    }

    pub fn preview(&self) -> &Document {
        &self.preview
    }

    /// Current semantic copy text. Incomplete delimiters in the mutable tail
    /// remain visible until they become valid syntax.
    pub fn copy_text(&self) -> String {
        let mut output = self.committed.plain_text();
        let tail = self.preview.plain_text();
        if !output.is_empty() && !output.ends_with('\n') && !tail.is_empty() {
            output.push('\n');
        }
        output.push_str(&tail);
        output
    }

    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    pub const fn committed_revision(&self) -> u64 {
        self.committed_revision
    }

    pub const fn tail_revision(&self) -> u64 {
        self.tail_revision
    }

    pub fn stats(&self) -> StreamingStats {
        StreamingStats {
            fence_scanned_bytes: self.scanner.scanned_bytes,
            committed_blocks: self.committed.blocks.len(),
            pending_utf8_bytes: self.pending_utf8.len(),
            ..self.stats
        }
    }

    /// Finish the stream. The result is exactly the static parser's semantic
    /// output for the decoded text.
    pub fn finish(&mut self) -> &Document {
        if self.finished {
            return &self.committed;
        }
        let remainder = decode_available(&mut self.pending_utf8, true);
        if !remainder.is_empty() {
            self.decoded.push_str(&remainder);
            self.tail.push_str(&remainder);
        }
        self.stats.pending_utf8_bytes = 0;
        self.stats.parse_passes = self.stats.parse_passes.saturating_add(1);
        self.stats.reparsed_bytes = self
            .stats
            .reparsed_bytes
            .saturating_add(self.decoded.len() as u64);
        self.committed = markdown::parse(&self.decoded);
        self.tail.clear();
        self.preview = Document::default();
        self.finished = true;
        self.committed_revision = self.committed_revision.saturating_add(1);
        self.tail_revision = self.tail_revision.saturating_add(1);
        &self.committed
    }

    fn stabilize(
        &mut self,
        had_open_fence: bool,
        saw_newline: bool,
        completed_fence: bool,
        proven_boundary: bool,
    ) {
        if let Some(open) = self.scanner.open.as_ref() {
            if !had_open_fence || completed_fence {
                if open.start > 0 {
                    self.commit_prefix(open.start);
                }
                // Prefix draining adjusts the scanner's offsets.
                let open = self.scanner.open.as_ref().expect("open fence retained");
                self.preview_epoch += 1;
                self.stats.preview_copied_bytes += (self.tail.len() - open.code_start) as u64;
                self.preview = Document::new(vec![Block::CodeBlock(CodeBlock {
                    language: open.language.clone(),
                    code: self.tail[open.code_start..].to_owned(),
                })]);
                self.tail_semantic_parsed = true;
            }
            return;
        }

        if had_open_fence {
            // The close marker may be present in `open_code`; discard the
            // incremental preview and parse the now-complete fenced block once.
        }

        if !saw_newline {
            // Reasoning summaries and short answers often contain complete
            // inline Markdown but no newline (for example `**Planning**`).
            // Promote those tails immediately, then keep their bounded preview
            // current as more tokens arrive. Huge paragraphs still use the
            // geometric fallback below and never incur unbounded reparsing.
            let live_inline = self.tail.len() <= MAX_LIVE_INLINE_PREVIEW_BYTES
                && (self.tail_semantic_parsed || likely_complete_inline(&self.tail));
            if live_inline {
                self.record_parse(self.tail.len());
                self.preview_epoch += 1;
                self.preview = markdown::parse(&self.tail);
                self.tail_semantic_parsed = true;
            } else {
                self.append_plain_preview();
            }
            return;
        }

        let parse_threshold = self.next_parse_at.max(1024);
        let structural_line = self.tail.len() <= MAX_UNSTABLE_PARSE_BYTES
            && self.tail.lines().next().is_some_and(|line| {
                let line = line.trim_start();
                line.starts_with('#')
                    || line.starts_with('>')
                    || is_list_marker(line, &mut self.stats.lexical_scanned_bytes)
                    || matches!(line, "---" | "***" | "___")
            });
        let structural_tail = self.tail.len() <= MAX_UNSTABLE_PARSE_BYTES
            && self.tail.lines().next_back().is_some_and(|line| {
                let line = line.trim();
                matches!(line, "---" | "***" | "___")
                    || (line.contains('|') && line.contains("---"))
            });
        if self.tail.len() <= MAX_UNSTABLE_PARSE_BYTES
            && (had_open_fence
                || completed_fence
                || proven_boundary
                || ((structural_line || structural_tail) && !self.tail_semantic_parsed)
                || self.tail.len() >= parse_threshold)
        {
            self.parse_and_commit_stable_tail();
            self.next_parse_at = self
                .tail
                .len()
                .saturating_mul(2)
                .clamp(1024, MAX_UNSTABLE_PARSE_BYTES);
        } else if self.tail.len() <= MAX_UNSTABLE_PARSE_BYTES {
            self.append_plain_preview();
        } else if proven_boundary {
            if let Some(offset) = lexical_stable_offset(
                &self.tail,
                &mut self.lexical_first,
                &mut self.stats.lexical_scanned_bytes,
            ) {
                self.commit_prefix(offset);
                self.parse_and_commit_stable_tail();
            } else {
                self.append_plain_preview();
            }
        } else {
            // An enormous single paragraph/list remains mutable. Display it as
            // safe literal text and parse it only once on completion.
            self.append_plain_preview();
        }
    }

    fn parse_and_commit_stable_tail(&mut self) {
        self.record_parse(self.tail.len());
        let starts = markdown::top_level_block_starts(&self.tail);
        if starts.len() >= 2 {
            if let Some(offset) = starts.last().copied().filter(|offset| *offset > 0) {
                self.commit_prefix(offset);
            }
        }
        self.record_parse(self.tail.len());
        self.preview_epoch += 1;
        self.preview = markdown::parse(&self.tail);
        self.tail_semantic_parsed = true;
    }

    fn commit_prefix(&mut self, offset: usize) {
        if offset == 0 || offset > self.tail.len() || !self.tail.is_char_boundary(offset) {
            return;
        }
        let prefix_len = offset;
        let mut document = markdown::parse(&self.tail[..offset]);
        self.record_parse(prefix_len);
        self.committed.blocks.append(&mut document.blocks);
        self.tail.drain(..offset);
        self.scanner.drain_prefix(offset);
        self.lexical_first = None;
        // A drained tail invalidates any literal preview prefix. Callers either
        // replace it immediately with a semantic render or append afresh.
        self.preview_epoch += 1;
        self.preview = Document::default();
        self.next_parse_at = 1024;
        self.tail_semantic_parsed = false;
        self.committed_revision = self.committed_revision.saturating_add(1);
    }

    fn record_parse(&mut self, bytes: usize) {
        self.stats.parse_passes = self.stats.parse_passes.saturating_add(1);
        self.stats.reparsed_bytes = self.stats.reparsed_bytes.saturating_add(bytes as u64);
    }

    fn append_plain_preview(&mut self) {
        match self.preview.blocks.as_mut_slice() {
            [Block::Plain(text)] if text.len() <= self.tail.len() => {
                // Literal tails change only by append; `commit_prefix` clears
                // the preview before draining. Trust that invariant instead of
                // comparing the complete accumulated paragraph on every token.
                if text.len() < self.tail.len() {
                    self.stats.preview_copied_bytes += (self.tail.len() - text.len()) as u64;
                    text.push_str(&self.tail[text.len()..]);
                }
            }
            _ => {
                self.preview_epoch += 1;
                self.stats.preview_copied_bytes += self.tail.len() as u64;
                self.preview = Document::new(vec![Block::Plain(self.tail.clone())]);
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamingLineUpdate {
    /// Physical rows at the start of the previous render that remain unchanged.
    pub stable_prefix: usize,
    /// Selected styled/plain rows replacing everything after `stable_prefix`.
    pub replacement: Vec<String>,
}

/// Incremental line-layout cache. At a stable width, newly committed blocks
/// and changed tail rows are rendered. Literal/open-code tails retain proven
/// visual rows; semantic promotion, reflow, and finalization may replace them.
#[derive(Clone, Debug, Default)]
pub struct StreamingRenderCache {
    width: Option<u16>,
    options: Option<RenderOptions>,
    theme_revision: u64,
    committed_revision: u64,
    committed_blocks: usize,
    committed_lines: Vec<RenderedLine>,
    /// Exclusive row boundary after each committed semantic segment. Ordinary
    /// Markdown blocks contribute one segment; lists and tables contribute
    /// stable item/row segments so commits can advance through large blocks.
    /// Positions are recomputed on reflow while indices retain their identity.
    committed_block_ends: Vec<usize>,
    tail_revision: u64,
    tail_lines: Vec<RenderedLine>,
    tail_visible: usize,
    append_tail: AppendOnlyTail,
    preview_epoch: u64,
    layout_stats: StreamingLayoutStats,
    selected_styled: Option<bool>,
    /// Pre-built merged result, invalidated when either committed or tail changes.
    merged_lines: Vec<RenderedLine>,
    merged_revision: Option<u64>,
    finished: bool,
}

impl StreamingRenderCache {
    /// Tail layout inputs and encoded-row work, not allocation or parser totals.
    pub const fn stats(&self) -> StreamingLayoutStats {
        self.layout_stats
    }

    /// Rows produced by parser-committed blocks at the current width. These
    /// rows are byte-stable across later token deltas and may safely cross a
    /// native-scrollback commit boundary.
    pub fn committed_rows(&self) -> usize {
        self.committed_lines.len()
    }

    /// Exclusive visual-row ends for parser-committed semantic segments at the
    /// current width. Segment indices survive width reflow; lists and tables
    /// contribute one segment per item or row instead of stalling at the outer
    /// Markdown block boundary.
    pub fn committed_block_ends(&self) -> &[usize] {
        &self.committed_block_ends
    }

    fn append_committed_blocks(&mut self, blocks: &[Block], renderer: &RichRenderer, width: u16) {
        for block in blocks {
            if !self.committed_lines.is_empty() {
                self.committed_lines.push(RenderedLine::default());
            }
            let block_start = self.committed_lines.len();
            let (lines, ends) = renderer.render_block_with_commit_ends(block, width);
            self.committed_lines.extend(lines);
            if ends.is_empty() {
                self.committed_block_ends.push(self.committed_lines.len());
            } else {
                self.committed_block_ends
                    .extend(ends.into_iter().map(|end| block_start.saturating_add(end)));
            }
        }
    }

    fn update(&mut self, stream: &StreamingMarkdown, renderer: &RichRenderer, width: u16) -> usize {
        let prior_committed_rows = self.committed_lines.len();
        let prior_committed_blocks = self.committed_blocks;
        let prior_tail_visible = self.tail_visible;
        let mut stable_tail = prior_tail_visible;
        let full_reflow = self.width != Some(width)
            || self.options != Some(renderer.options())
            || self.theme_revision != renderer.theme().revision()
            || (stream.is_finished() && !self.finished)
            || stream.committed().blocks.len() < self.committed_blocks;

        if full_reflow {
            self.committed_lines.clear();
            self.committed_block_ends.clear();
            self.committed_blocks = 0;
            self.append_committed_blocks(&stream.committed().blocks, renderer, width);
            self.committed_blocks = stream.committed().blocks.len();
        } else if self.committed_revision != stream.committed_revision()
            && self.committed_blocks < stream.committed().blocks.len()
        {
            let new_blocks = &stream.committed().blocks[self.committed_blocks..];
            self.append_committed_blocks(new_blocks, renderer, width);
            self.committed_blocks = stream.committed().blocks.len();
        }

        if full_reflow || self.preview_epoch != stream.preview_epoch {
            self.append_tail = AppendOnlyTail::default();
            stable_tail = 0;
        }
        if full_reflow || self.tail_revision != stream.tail_revision() {
            if stream.is_finished() {
                self.tail_lines.clear();
                self.tail_visible = 0;
                stable_tail = 0;
            } else if let [block] = stream.preview().blocks.as_slice() {
                if let Some((stable, visible)) = self.append_tail.update(
                    block,
                    renderer,
                    width,
                    &mut self.tail_lines,
                    &mut self.layout_stats,
                ) {
                    self.tail_visible = visible;
                    stable_tail = stable.min(prior_tail_visible);
                } else {
                    self.render_general_tail(stream, renderer, width);
                    stable_tail = 0;
                }
            } else {
                self.render_general_tail(stream, renderer, width);
                stable_tail = 0;
            }
        }
        self.preview_epoch = stream.preview_epoch;

        self.width = Some(width);
        self.options = Some(renderer.options());
        self.theme_revision = renderer.theme().revision();
        if full_reflow {
            // Width/theme/options changes rebuild the source caches without
            // changing stream revisions, so the merged view must be rebuilt.
            self.merged_revision = None;
        }
        self.committed_revision = stream.committed_revision();
        self.tail_revision = stream.tail_revision();
        self.finished = stream.is_finished();

        if full_reflow {
            0
        } else if self.committed_blocks > prior_committed_blocks {
            prior_committed_rows
        } else {
            self.committed_lines.len()
                + usize::from(!self.committed_lines.is_empty() && self.tail_visible > 0)
                + stable_tail
        }
    }

    fn render_general_tail(
        &mut self,
        stream: &StreamingMarkdown,
        renderer: &RichRenderer,
        width: u16,
    ) {
        self.layout_stats.full_tail_layouts += 1;
        self.layout_stats.fallback_source_bytes += stream.unstable_source().len() as u64;
        self.tail_lines = renderer.render_unstable_lines(stream.preview(), width);
        self.tail_visible = self.tail_lines.len();
        self.layout_stats.encoded_rows += self.tail_visible as u64;
    }

    fn merged_lines(&mut self) -> &[RenderedLine] {
        let merge_rev = self
            .committed_revision
            .wrapping_mul(2)
            .wrapping_add(self.tail_revision);
        if self.merged_revision != Some(merge_rev) {
            let total = self.committed_lines.len()
                + if self.committed_lines.is_empty() || self.tail_visible == 0 {
                    0
                } else {
                    1
                }
                + self.tail_visible;
            self.merged_lines.clear();
            self.merged_lines.reserve(total);
            self.merged_lines
                .extend(self.committed_lines.iter().cloned());
            if !self.committed_lines.is_empty() && self.tail_visible > 0 {
                self.merged_lines.push(RenderedLine::default());
            }
            self.merged_lines
                .extend(self.tail_lines[..self.tail_visible].iter().cloned());
            self.merged_revision = Some(merge_rev);
        }
        &self.merged_lines
    }

    pub fn render(
        &mut self,
        stream: &StreamingMarkdown,
        renderer: &RichRenderer,
        width: u16,
    ) -> RenderedDocument {
        self.update(stream, renderer, width);
        let lines = self.merged_lines().to_vec();
        RenderedDocument {
            lines,
            copy_text: renderer.sanitize_copy(&stream.copy_text()),
        }
    }

    fn selected_lines_from(&self, start: usize, styled: bool) -> Vec<String> {
        let separator = usize::from(!self.committed_lines.is_empty() && self.tail_visible > 0);
        let total = self.committed_lines.len() + separator + self.tail_visible;
        let start = start.min(total);
        let mut lines = Vec::with_capacity(total.saturating_sub(start));

        if start < self.committed_lines.len() {
            lines.extend(self.committed_lines[start..].iter().map(|line| {
                if styled {
                    line.styled.clone()
                } else {
                    line.plain.clone()
                }
            }));
        }
        if separator > 0 && start <= self.committed_lines.len() {
            lines.push(String::new());
        }
        let tail_start = start.saturating_sub(self.committed_lines.len() + separator);
        lines.extend(
            self.tail_lines[tail_start.min(self.tail_visible)..self.tail_visible]
                .iter()
                .map(|line| {
                    if styled {
                        line.styled.clone()
                    } else {
                        line.plain.clone()
                    }
                }),
        );
        lines
    }

    /// Render only the mutable suffix while reporting the unchanged physical
    /// prefix from the previous call at the same width/theme. Visual stability
    /// within the preview is not a parser commit; use `committed_rows()` when
    /// deciding which rows may cross a native-scrollback commit boundary.
    pub fn render_line_update(
        &mut self,
        stream: &StreamingMarkdown,
        renderer: &RichRenderer,
        width: u16,
        styled: bool,
    ) -> StreamingLineUpdate {
        let mut stable_prefix = self.update(stream, renderer, width);
        if self.selected_styled != Some(styled) {
            stable_prefix = 0;
        }
        self.selected_styled = Some(styled);
        StreamingLineUpdate {
            stable_prefix,
            replacement: self.selected_lines_from(stable_prefix, styled),
        }
    }

    /// Render one terminal representation without constructing the semantic
    /// copy text or cloning the unused styled/plain representation. This is the
    /// hot path for transcript surfaces, which already store copy text in their
    /// source block and only need one display representation.
    pub fn render_lines(
        &mut self,
        stream: &StreamingMarkdown,
        renderer: &RichRenderer,
        width: u16,
        styled: bool,
    ) -> Vec<String> {
        self.update(stream, renderer, width);
        self.selected_styled = Some(styled);
        self.selected_lines_from(0, styled)
    }
}

fn decode_available(buffer: &mut Vec<u8>, finish: bool) -> String {
    let mut output = String::new();
    loop {
        match str::from_utf8(buffer) {
            Ok(valid) => {
                output.push_str(valid);
                buffer.clear();
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    // Safety: UTF-8 validation identified this exact prefix.
                    output.push_str(
                        str::from_utf8(&buffer[..valid]).expect("validated UTF-8 prefix"),
                    );
                    buffer.drain(..valid);
                    continue;
                }
                if let Some(length) = error.error_len() {
                    output.push('\u{fffd}');
                    buffer.drain(..length.min(buffer.len()));
                    continue;
                }
                if finish {
                    output.push('\u{fffd}');
                    buffer.clear();
                }
                break;
            }
        }
    }
    output
}

fn opening_fence(line: &str) -> Option<(char, usize, Option<String>)> {
    let line = line
        .strip_prefix("   ")
        .or_else(|| line.strip_prefix("  "))
        .or_else(|| line.strip_prefix(' '))
        .unwrap_or(line);
    let marker = line.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let count = line
        .chars()
        .take_while(|character| *character == marker)
        .count();
    if count < 3 {
        return None;
    }
    let info = line[count..]
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Some((marker, count, info))
}

fn is_closing_fence(line: &str, marker: char, minimum: usize) -> bool {
    let line = line
        .strip_prefix("   ")
        .or_else(|| line.strip_prefix("  "))
        .or_else(|| line.strip_prefix(' '))
        .unwrap_or(line);
    let count = line
        .chars()
        .take_while(|character| *character == marker)
        .count();
    count >= minimum && line[count..].trim().is_empty()
}

fn likely_complete_inline(source: &str) -> bool {
    let paired = |marker: char| source.matches(marker).nth(1).is_some();
    paired('*')
        || paired('_')
        || paired('`')
        || source.match_indices("~~").nth(1).is_some()
        || source
            .find("](")
            .is_some_and(|start| source[start.saturating_add(2)..].contains(')'))
}

#[derive(Clone, Copy, Debug)]
struct LexicalLinePrefix {
    list: bool,
    quote: bool,
    html: bool,
}

fn lexical_line_prefix(source: &str, scanned_bytes: &mut u64) -> LexicalLinePrefix {
    // Equivalent to lines().next().unwrap_or_default().trim_start() for
    // prefix checks, without finding the end of a potentially enormous line.
    // LF must stop trimming: whitespace on the next line is not indentation.
    let line = source.trim_start_matches(|ch: char| {
        *scanned_bytes += ch.len_utf8() as u64;
        ch != '\n' && ch.is_whitespace()
    });
    let marker = line.as_bytes().first().copied();
    *scanned_bytes += u64::from(marker.is_some());
    LexicalLinePrefix {
        list: is_list_marker(line, scanned_bytes),
        quote: marker == Some(b'>'),
        html: marker == Some(b'<'),
    }
}

fn lexical_stable_offset(
    source: &str,
    first: &mut Option<LexicalLinePrefix>,
    scanned_bytes: &mut u64,
) -> Option<usize> {
    let boundary = source.rfind("\n\n");
    *scanned_bytes += boundary.map_or(source.len(), |start| source.len() - start) as u64;
    let offset = boundary?.saturating_add(2);
    if offset >= source.len() {
        return None;
    }
    // A blank boundary proves the first line is complete. Cache its marker
    // classification until prefix draining changes that line; even unbounded
    // leading whitespace or ordered-list digits are then inspected only once.
    let first = *first.get_or_insert_with(|| lexical_line_prefix(source, scanned_bytes));
    let candidate = lexical_line_prefix(&source[offset..], scanned_bytes);
    // The existing heuristic trims indentation before testing these markers;
    // its old space/tab continuation checks were therefore always false. Keep
    // that lexical interpretation rather than introducing new Markdown rules.
    if (first.quote && candidate.quote) || (first.list && candidate.list) || first.html {
        None
    } else {
        Some(offset)
    }
}

fn is_list_marker(line: &str, scanned_bytes: &mut u64) -> bool {
    let mut bytes = line.bytes().inspect(|_| *scanned_bytes += 1);
    let first = bytes.next();
    if matches!(first, Some(b'-' | b'*' | b'+')) {
        return bytes.next() == Some(b' ');
    }
    let mut marker = first;
    while marker.is_some_and(|byte| byte.is_ascii_digit()) {
        marker = bytes.next();
    }
    // Preserve the previous split_once(". ") behavior, including its empty
    // numeric prefix (". "), but never search an ordinary line's full body.
    marker == Some(b'.') && bytes.next() == Some(b' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich_text::render::RichRenderer;
    use crate::{ColorDepth, TerminalCapabilities, Theme};

    const ADVERSARIAL: &str = "# Heading\n\nA **strong** link to [docs](https://example.com) and `code`.\n\n- first\n  - nested\n- second\n\n```rust\nfn main() {\n    println!(\"界\");\n}\n```\n";

    fn prior_list_marker(line: &str) -> bool {
        line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("+ ")
            || line.split_once(". ").is_some_and(|(number, _)| {
                number.chars().all(|character| character.is_ascii_digit())
            })
    }

    fn prior_lexical_stable_offset(source: &str) -> Option<usize> {
        let offset = source.rfind("\n\n")?.saturating_add(2);
        if offset >= source.len() {
            return None;
        }
        let first = source.lines().next().unwrap_or_default().trim_start();
        let candidate = source[offset..]
            .lines()
            .next()
            .unwrap_or_default()
            .trim_start();
        let same_quote = first.starts_with('>') && candidate.starts_with('>');
        let possible_list_continuation = prior_list_marker(first)
            && (prior_list_marker(candidate)
                || candidate.starts_with("  ")
                || candidate.starts_with('\t'));
        let possible_indented_code = (first.starts_with("    ") || first.starts_with('\t'))
            && (candidate.starts_with("    ") || candidate.starts_with('\t'));
        let possible_html_block = first.starts_with('<');
        if same_quote || possible_list_continuation || possible_indented_code || possible_html_block
        {
            None
        } else {
            Some(offset)
        }
    }

    #[test]
    fn lexical_prefix_checks_preserve_prior_semantics_without_scanning_line_bodies() {
        let cases = [
            "",
            " ",
            "\t\u{2003}",
            "\n- x",
            "\r\n> x",
            "\u{2003}\n<tag>",
            "> quote",
            "<tag>",
            "- item",
            "* item",
            "+ item",
            ". item",
            "1. item",
            "12345678901234567890. item",
            "1.. item",
            "  plain text",
            "\t- item",
            "1.\n item",
        ];
        for first in cases {
            assert_eq!(is_list_marker(first, &mut 0), prior_list_marker(first));
            for candidate in cases {
                let mut source = format!("{first}\n\n{candidate}");
                let mut cache = None;
                for suffix in [
                    "",
                    " more",
                    "\n\n- item",
                    "\n\n. item",
                    "\n\n\t> quote",
                    "\n\n",
                ] {
                    source.push_str(suffix);
                    assert_eq!(
                        lexical_stable_offset(&source, &mut cache, &mut 0),
                        prior_lexical_stable_offset(&source),
                        "{source:?}"
                    );
                }
            }
        }
        let mut seed = 71u64;
        let alphabet = [
            'a', '1', '9', '.', ' ', '\t', '\n', '\r', '\u{2003}', '-', '*', '+', '>', '<', '界',
        ];
        for _ in 0..2_000 {
            let mut source = String::new();
            for _ in 0..40 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                source.push(alphabet[seed as usize % alphabet.len()]);
            }
            assert_eq!(is_list_marker(&source, &mut 0), prior_list_marker(&source));
            source.push_str("\n\n- item");
            assert_eq!(
                lexical_stable_offset(&source, &mut None, &mut 0),
                prior_lexical_stable_offset(&source)
            );
        }
        // A rejected marker must not search a long ordinary line for a later
        // '. '. An empty numeric prefix remains accepted, matching the old rule.
        let ordinary = format!("word{}. item", "a".repeat(100_000));
        let mut scanned = 0;
        assert!(!is_list_marker(&ordinary, &mut scanned));
        assert_eq!(scanned, 1);
        assert!(is_list_marker(". item", &mut 0));
    }

    #[test]
    fn huge_lexical_first_line_is_classified_once_including_whitespace_and_digits() {
        for first in [
            format!("- {}", "a".repeat(100_000)),
            format!("{}- item", "\u{2003}".repeat(40_000)),
            format!("{}. item", "1".repeat(100_000)),
            format!(". {}", "a".repeat(100_000)),
        ] {
            let mut stream = StreamingMarkdown::from_text(&first);
            for step in 0..1_000 {
                let before = stream.stats().lexical_scanned_bytes;
                stream.push_str("\n\n- x");
                let work = stream.stats().lexical_scanned_bytes - before;
                if step > 0 {
                    assert_eq!(work, 9, "step={step}");
                } else {
                    assert!(work <= first.len() as u64 + 16);
                }
            }
            let stats = stream.stats();
            assert!(
                stats.lexical_scanned_bytes <= first.len() as u64 + 9_016,
                "{stats:?}"
            );
            assert!(stream.committed().blocks.is_empty());
            let expected = first + &"\n\n- x".repeat(1_000);
            assert_eq!(stream.raw_bytes(), expected.as_bytes());
            let [Block::Plain(text)] = stream.preview().blocks.as_slice() else {
                panic!("literal preview");
            };
            assert_eq!(text, &expected);
            assert_eq!(stream.copy_text(), expected.clone() + "\n");
            assert_eq!(stream.finish(), &markdown::parse(&expected));
            eprintln!(
                "lexical source={} scan_bytes={}",
                expected.len(),
                stats.lexical_scanned_bytes
            );
        }
    }

    #[test]
    fn lexical_first_line_cache_is_reset_when_the_tail_prefix_commits() {
        let mut stream = StreamingMarkdown::from_text(&format!("- {}", "a".repeat(100_000)));
        stream.push_str("\n\n- x");
        assert!(stream.lexical_first.is_some());
        stream.push_str("\n\nordinary");
        assert!(stream.lexical_first.is_none());
        let committed = stream.committed().blocks.len();
        stream.push_str(&"b".repeat(100_000));
        stream.push_str("\n\n- new list");
        assert!(stream.committed().blocks.len() > committed);
        let expected = markdown::parse(stream.raw_text());
        assert_eq!(stream.finish(), &expected);
    }

    #[test]
    fn fence_search_and_literal_preview_copy_are_append_local() {
        for chunk in ["abcdefgh", "word line\n", "界 e\u{301} "] {
            let mut stream = StreamingMarkdown::new();
            let mut total = 0;
            for _ in 0..30_000 {
                stream.push_str(chunk);
                total += chunk.len();
            }
            let stats = stream.stats();
            assert!(stats.fence_scanned_bytes <= 2 * total as u64, "{stats:?}");
            assert!(stats.preview_copied_bytes <= 3 * total as u64, "{stats:?}");
            assert_eq!(stream.raw_bytes(), chunk.repeat(30_000).as_bytes());
            let [Block::Plain(text)] = stream.preview().blocks.as_slice() else {
                panic!("expected literal preview");
            };
            assert_eq!(text, stream.unstable_source());
            assert_eq!(stream.finish(), &markdown::parse(&chunk.repeat(30_000)));
        }
    }

    fn tail_work(chunks: usize, fence: bool, multiline: bool, wrap: bool) -> StreamingLayoutStats {
        let mut stream = StreamingMarkdown::new();
        let mut renderer = RichRenderer::plain();
        let mut options = renderer.options();
        options.code_overflow = if wrap {
            super::super::render::CodeOverflow::Wrap
        } else {
            super::super::render::CodeOverflow::Clip
        };
        renderer.set_options(options);
        let mut cache = StreamingRenderCache::default();
        if fence {
            stream.push_str("```rust\n");
        }
        let chunk = if multiline {
            "some words and source text\n"
        } else {
            "word xyz "
        };
        let mut frame = Vec::new();
        for _ in 0..chunks {
            stream.push_str(chunk);
            let update = cache.render_line_update(&stream, &renderer, 40, false);
            assert!(update.stable_prefix <= frame.len());
            frame.truncate(update.stable_prefix);
            frame.extend(update.replacement);
        }
        let stats = cache.stats();
        let expected = renderer.render_unstable(stream.preview(), 40).plain_lines();
        assert_eq!(frame, expected);
        assert_eq!(
            stream.raw_text(),
            format!(
                "{}{}",
                if fence { "```rust\n" } else { "" },
                chunk.repeat(chunks)
            )
        );
        let copy = cache.render(&stream, &renderer, 40).copy_text;
        assert_eq!(copy, renderer.sanitize_copy(&stream.copy_text()));
        assert!(copy.contains(chunk.trim()));
        let raw = stream.raw_text().to_owned();
        assert_eq!(stream.finish(), &markdown::parse(&raw));
        let final_update = cache.render_line_update(&stream, &renderer, 40, false);
        assert_eq!(final_update.stable_prefix, 0);
        assert_eq!(
            final_update.replacement,
            renderer.render(stream.committed(), 40).plain_lines()
        );
        stats
    }

    #[test]
    fn long_plain_and_open_code_layout_work_grows_linearly() {
        for fence in [false, true] {
            for multiline in [false, true] {
                for wrap in [false, true] {
                    let small = tail_work(4_000, fence, multiline, wrap);
                    let large = tail_work(8_000, fence, multiline, wrap);
                    let work = |s: StreamingLayoutStats| {
                        s.checked_bytes
                            + s.measured_bytes
                            + s.laid_out_bytes
                            + s.fallback_source_bytes
                    };
                    assert!(
                        work(large) <= work(small) * 5 / 2,
                        "{fence} {multiline} {wrap}: {small:?} -> {large:?}"
                    );
                    assert!(large.encoded_rows < 8_000 * 12, "{large:?}");
                    assert!(large.laid_out_bytes < 8_000 * 256, "{large:?}");
                    eprintln!("tail_work fence={fence} multiline={multiline} wrap={wrap}: {small:?} -> {large:?}");
                }
            }
        }
    }

    #[test]
    fn random_byte_chunks_resize_theme_and_finalization_match_authoritative_rows() {
        use crate::rich_text::render::CodeOverflow;
        let cases = [
            "plain 界 words e\u{301} 👩\u{200d}💻 with wraps and more ordinary text ".repeat(8),
            format!("```rust\n{}", "  let 界 = e\u{301}; // 👩\u{200d}💻\n\n".repeat(12)),
            "# head\n\nplain\ttext\r\n\x1b[31m\u{202e} more\n\n```text\nhello\tworld\r\n\n```\n\nend".to_owned(),
            "```text\nold\n```\n```rust\nnew\n".to_owned(),
        ];
        for source in cases {
            for seed in 1..=4u64 {
                let caps = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
                let mut renderer = RichRenderer::new(
                    Theme::with_capabilities(caps),
                    caps,
                    RenderOptions::default(),
                );
                let mut stream = StreamingMarkdown::new();
                let mut cache = StreamingRenderCache::default();
                let mut frame = Vec::new();
                let mut rng = seed;
                let mut offset = 0;
                let mut step = 0;
                while offset < source.len() {
                    rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let end = (offset + 1 + (rng as usize % 23)).min(source.len());
                    stream.push_bytes(&source.as_bytes()[offset..end]);
                    offset = end;
                    step += 1;
                    let width = [1, 13, 40, 0, 80][step / 11 % 5];
                    if step % 17 == 0 {
                        renderer
                            .theme_mut()
                            .set_accent(crate::Color::Rgb(1, step as u8, 90));
                    }
                    let mut options = renderer.options();
                    options.code_overflow = if step / 13 % 2 == 0 {
                        CodeOverflow::Clip
                    } else {
                        CodeOverflow::Wrap
                    };
                    options.code_borders = step / 19 % 2 == 0;
                    renderer.set_options(options);
                    let styled = step / 7 % 2 == 0;
                    let update = cache.render_line_update(&stream, &renderer, width, styled);
                    assert!(update.stable_prefix <= frame.len());
                    frame.truncate(update.stable_prefix);
                    frame.extend(update.replacement);
                    let mut expected =
                        renderer.render_blocks_only(&stream.committed().blocks, width);
                    let tail = renderer.render_unstable(stream.preview(), width).lines;
                    if !expected.is_empty() && !tail.is_empty() {
                        expected.push(RenderedLine::default());
                    }
                    expected.extend(tail);
                    let expected: Vec<_> = expected
                        .into_iter()
                        .map(|line| if styled { line.styled } else { line.plain })
                        .collect();
                    assert_eq!(frame, expected, "seed={seed} step={step} source={source:?}");
                }
                assert_eq!(stream.raw_bytes(), source.as_bytes());
                assert_eq!(stream.finish(), &markdown::parse(&source));
                let final_update = cache.render_line_update(&stream, &renderer, 40, true);
                assert_eq!(final_update.stable_prefix, 0);
                assert_eq!(
                    final_update.replacement,
                    renderer.render(stream.committed(), 40).styled_lines()
                );
            }
        }
    }

    #[test]
    fn committed_blocks_are_a_monotonic_prefix_of_the_final_document() {
        let expected = markdown::parse(ADVERSARIAL);
        let mut stream = StreamingMarkdown::new();
        for byte in ADVERSARIAL.as_bytes() {
            stream.push_bytes(&[*byte]);
            assert!(expected.blocks.starts_with(&stream.committed().blocks));
        }
    }

    #[test]
    fn every_utf8_chunk_boundary_finishes_like_static_parsing() {
        let expected = markdown::parse(ADVERSARIAL);
        for split in 0..=ADVERSARIAL.len() {
            if !ADVERSARIAL.is_char_boundary(split) {
                continue;
            }
            let mut stream = StreamingMarkdown::new();
            stream.push_str(&ADVERSARIAL[..split]);
            stream.push_str(&ADVERSARIAL[split..]);
            assert_eq!(stream.finish(), &expected, "split at {split}");
            assert_eq!(stream.raw_bytes(), ADVERSARIAL.as_bytes());
        }
    }

    #[test]
    fn arbitrary_byte_chunks_buffer_utf8_and_never_panic() {
        let mut stream = StreamingMarkdown::new();
        for byte in ADVERSARIAL.as_bytes() {
            stream.push_bytes(&[*byte]);
        }
        assert_eq!(stream.finish(), &markdown::parse(ADVERSARIAL));
        assert_eq!(stream.stats().pending_utf8_bytes, 0);
    }

    #[test]
    fn complete_fence_in_one_chunk_is_parsed_without_waiting_for_finish() {
        let mut stream = StreamingMarkdown::new();
        stream.push_str("```rust\nfn main() {}\n```\n");
        assert!(matches!(
            stream.preview().blocks.as_slice(),
            [Block::CodeBlock(_)]
        ));
    }

    #[test]
    fn incomplete_inline_and_block_syntax_is_mutable_not_permanently_wrong() {
        let mut stream = StreamingMarkdown::new();
        stream.push_str("Opening **strong");
        assert!(stream.preview().plain_text().contains("**strong"));
        stream.push_str("**\n\n```ru");
        stream.push_str("st\nfn main()");
        assert!(stream.preview().plain_text().contains("fn main"));
        stream.push_str(" {}\n```\n");
        let final_document = stream.finish().clone();
        assert_eq!(final_document, markdown::parse(stream.raw_text()));
        assert!(!final_document.plain_text().contains("**"));
    }

    #[test]
    fn complete_inline_markdown_becomes_rich_before_a_newline_or_finish() {
        let mut stream = StreamingMarkdown::new();
        stream.push_str("**Plan");
        assert!(stream.preview().plain_text().contains("**Plan"));
        stream.push_str("ning**");
        assert_eq!(stream.preview().plain_text(), "Planning\n");
        assert!(matches!(
            stream.preview().blocks.as_slice(),
            [Block::Paragraph(content)]
                if matches!(content.as_slice(), [crate::rich_text::Inline::Strong(_)])
        ));

        stream.push_str(" and `testing`");
        assert_eq!(stream.preview().plain_text(), "Planning and testing\n");
        let rendered = RichRenderer::plain()
            .render_unstable(stream.preview(), 80)
            .plain_text();
        assert!(!rendered.contains("**"));
        assert!(!rendered.contains('`'));
    }

    #[test]
    fn malformed_utf8_and_escape_sequences_are_recoverable_and_safe() {
        let mut stream = StreamingMarkdown::new();
        stream.push_bytes(b"safe \xf0\x9f");
        assert_eq!(stream.stats().pending_utf8_bytes, 2);
        stream.push_bytes(b"\x92\xa1 \x1b]52;c;bad\x07");
        stream.finish();
        assert_eq!(
            stream.raw_bytes(),
            b"safe \xf0\x9f\x92\xa1 \x1b]52;c;bad\x07"
        );
        let rendered = RichRenderer::plain()
            .render(stream.committed(), 80)
            .styled_text();
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\x07'));
    }

    #[test]
    fn long_single_paragraph_uses_geometric_not_per_line_reparsing() {
        let mut stream = StreamingMarkdown::new();
        for _ in 0..20_000 {
            stream.push_str("word line\n");
        }
        assert!(stream.stats().reparsed_bytes < 4 * MAX_UNSTABLE_PARSE_BYTES as u64);
        stream.finish();
    }

    #[test]
    fn unstable_reparse_is_bounded_for_huge_open_blocks() {
        let mut stream = StreamingMarkdown::new();
        stream.push_str("```text\n");
        for _ in 0..10_000 {
            stream.push_str("a long code line\n");
        }
        let before_finish = stream.stats();
        assert!(before_finish.reparsed_bytes < 2 * MAX_UNSTABLE_PARSE_BYTES as u64);
        assert!(stream.preview().plain_text().contains("a long code line"));
        stream.push_str("```\n");
        stream.finish();
        assert_eq!(stream.committed(), &markdown::parse(stream.raw_text()));
    }

    #[test]
    fn semantic_commit_segments_remap_across_widths() {
        let renderer = RichRenderer::plain();
        for (source, expected_segments) in [
            ("- alpha item\n- beta item\n- gamma item\n", 3),
            (
                "| Name | Value |\n|---|---|\n| alpha | one |\n| beta | two |\n| gamma | three |\n",
                3,
            ),
        ] {
            let mut stream = StreamingMarkdown::from_text(source);
            stream.finish();
            let mut cache = StreamingRenderCache::default();

            cache.render(&stream, &renderer, 80);
            let wide = cache.committed_block_ends().to_vec();
            assert_eq!(wide.len(), expected_segments);
            assert_eq!(wide.last().copied(), Some(cache.committed_rows()));
            assert!(wide.windows(2).all(|ends| ends[0] < ends[1]));

            cache.render(&stream, &renderer, 12);
            let narrow = cache.committed_block_ends().to_vec();
            assert_eq!(narrow.len(), expected_segments);
            assert_eq!(narrow.last().copied(), Some(cache.committed_rows()));
            assert!(narrow.windows(2).all(|ends| ends[0] < ends[1]));
        }
    }

    #[test]
    fn line_update_reuses_committed_rows_and_replaces_only_the_mutable_tail() {
        let renderer = RichRenderer::plain();
        let mut stream = StreamingMarkdown::new();
        let mut cache = StreamingRenderCache::default();

        stream.push_str("# Stable heading\n\nmutable");
        let first = cache.render_line_update(&stream, &renderer, 40, false);
        assert_eq!(first.stable_prefix, 0);
        let mut frame = first.replacement;

        stream.push_str(" tail");
        let next = cache.render_line_update(&stream, &renderer, 40, false);
        assert!(next.stable_prefix > 0, "{next:?}");
        frame.truncate(next.stable_prefix);
        frame.extend(next.replacement);

        let mut full_cache = StreamingRenderCache::default();
        assert_eq!(
            frame,
            full_cache.render_lines(&stream, &renderer, 40, false)
        );

        let resized = cache.render_line_update(&stream, &renderer, 20, false);
        assert_eq!(resized.stable_prefix, 0);
    }

    #[test]
    fn full_lines_then_incremental_update_retains_the_selected_prefix() {
        let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
        let renderer = RichRenderer::new(
            Theme::with_capabilities(capabilities),
            capabilities,
            RenderOptions::default(),
        );
        for styled in [false, true] {
            let mut stream = StreamingMarkdown::from_text("# Stable heading\n\nmutable");
            let mut cache = StreamingRenderCache::default();
            let mut frame = cache.render_lines(&stream, &renderer, 40, styled);
            stream.push_str(" tail");
            let update = cache.render_line_update(&stream, &renderer, 40, styled);
            assert!(update.stable_prefix > 0, "{update:?}");
            frame.truncate(update.stable_prefix);
            frame.extend(update.replacement);
            let mut reference = StreamingRenderCache::default();
            assert_eq!(
                frame,
                reference.render_lines(&stream, &renderer, 40, styled)
            );

            // A full render also establishes which representation the next
            // delta must preserve. Switching it still invalidates every row.
            cache.render_lines(&stream, &renderer, 40, !styled);
            stream.push_str(" again");
            let changed = cache.render_line_update(&stream, &renderer, 40, styled);
            assert_eq!(changed.stable_prefix, 0);
            assert_eq!(
                changed.replacement,
                reference.render_lines(&stream, &renderer, 40, styled)
            );
        }
    }

    #[test]
    fn lines_only_render_matches_the_selected_document_lines() {
        let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
        let renderer = RichRenderer::new(
            Theme::with_capabilities(capabilities),
            capabilities,
            RenderOptions::default(),
        );
        let mut stream = StreamingMarkdown::new();
        stream.push_str("**committed**\n\nmutable");
        let mut document_cache = StreamingRenderCache::default();
        let mut lines_cache = StreamingRenderCache::default();

        let document = document_cache.render(&stream, &renderer, 80);
        let plain = lines_cache.render_lines(&stream, &renderer, 80, false);
        assert_eq!(
            plain,
            document
                .lines
                .iter()
                .map(|line| line.plain.clone())
                .collect::<Vec<_>>()
        );

        let styled = lines_cache.render_lines(&stream, &renderer, 80, true);
        assert_eq!(
            styled,
            document
                .lines
                .iter()
                .map(|line| line.styled.clone())
                .collect::<Vec<_>>()
        );

        stream.push_str(" tail");
        let next_document = document_cache.render(&stream, &renderer, 80);
        let next_plain = lines_cache.render_lines(&stream, &renderer, 80, false);
        assert_eq!(
            next_plain,
            next_document
                .lines
                .iter()
                .map(|line| line.plain.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn render_cache_reflows_on_resize_and_tracks_latest_tail() {
        let mut stream = StreamingMarkdown::new();
        let renderer = RichRenderer::plain();
        let mut cache = StreamingRenderCache::default();
        stream.push_str("first paragraph\n\nsecond");
        let wide = cache.render(&stream, &renderer, 80).plain_text();
        assert!(wide.contains("second"));
        let narrow = cache.render(&stream, &renderer, 10).plain_text();
        assert!(narrow.lines().all(|line| line.len() <= 10));
    }
}
