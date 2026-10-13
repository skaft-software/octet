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
use super::{Block, CodeBlock, Document, Inline};

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
    /// Newly accepted bytes classified for provisional structural-line visibility.
    pub preview_scanned_bytes: u64,
    /// Bytes classified while deciding whether a literal preview can be promoted
    /// to canonical prose.
    pub preview_classified_bytes: u64,
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
    math: Option<(usize, &'static str)>,
    pending: PendingPresentation,
    completed_table_header: bool,
    completed_fence_end: Option<usize>,
}

/// Incremental classification of just the unfinished source line. Once it is
/// ordinary payload, later bytes cannot turn its prefix into a block marker.
#[derive(Clone, Debug, Default)]
struct PendingPresentation {
    start: usize,
    checked: usize,
    kind: PendingLineKind,
}

#[derive(Clone, Debug, Default)]
enum PendingLineKind {
    #[default]
    Start,
    Indent(usize),
    Marker(char, usize),
    FenceInfo(char),
    ClosingSpace,
    Visible,
}

impl PendingPresentation {
    fn end(
        &mut self,
        source: &str,
        start: usize,
        open: Option<&FenceState>,
        scanned: &mut u64,
    ) -> usize {
        if self.start != start {
            *self = Self {
                start,
                checked: start,
                ..Self::default()
            };
        }
        for ch in source[self.checked..].chars() {
            if matches!(
                self.kind,
                PendingLineKind::Visible | PendingLineKind::FenceInfo('~')
            ) {
                break;
            }
            *scanned += ch.len_utf8() as u64;
            self.kind = match self.kind {
                PendingLineKind::Start | PendingLineKind::Indent(_) => {
                    let indent = match self.kind {
                        PendingLineKind::Indent(n) => n,
                        _ => 0,
                    };
                    if ch == ' ' && indent < 3 {
                        PendingLineKind::Indent(indent + 1)
                    } else if open.map_or(matches!(ch, '`' | '~' | '-' | '=' | '*' | '_'), |f| {
                        ch == f.marker
                    }) {
                        PendingLineKind::Marker(ch, 1)
                    } else {
                        PendingLineKind::Visible
                    }
                }
                PendingLineKind::Marker(marker, count) if ch == marker => {
                    PendingLineKind::Marker(marker, count + 1)
                }
                PendingLineKind::Marker(marker, count) => {
                    if let Some(open) = open {
                        if count >= open.count && ch.is_whitespace() {
                            PendingLineKind::ClosingSpace
                        } else {
                            PendingLineKind::Visible
                        }
                    } else if matches!(marker, '`' | '~') && count >= 3 {
                        PendingLineKind::FenceInfo(marker)
                    } else {
                        PendingLineKind::Visible
                    }
                }
                PendingLineKind::FenceInfo('`') if ch != '`' => PendingLineKind::FenceInfo('`'),
                PendingLineKind::ClosingSpace if ch.is_whitespace() => {
                    PendingLineKind::ClosingSpace
                }
                _ => PendingLineKind::Visible,
            };
        }
        self.checked = source.len();
        if matches!(self.kind, PendingLineKind::Visible) {
            source.len()
        } else {
            start
        }
    }
}

impl FenceScanner {
    fn scan(&mut self, source: &str) -> bool {
        let mut completed_fence = false;
        self.completed_table_header = false;
        self.completed_fence_end = None;
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
                    self.completed_fence_end = Some(end);
                }
            } else if let Some((_, closing)) = self.math {
                if line.trim_end().ends_with(closing) {
                    self.math = None;
                }
            } else if let Some((closing, body)) = display_math_opening(line) {
                if !body.trim_end().ends_with(closing) {
                    self.math = Some((self.offset, closing));
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
            if self.open.is_none()
                && self.math.is_none()
                && line.trim_start().starts_with(['|', '-', ':'])
            {
                self.scanned_bytes += line.len() as u64;
                self.completed_table_header |= line.contains('|')
                    && line.contains("---")
                    && line
                        .chars()
                        .all(|ch| matches!(ch, '|' | '-' | ':' | ' ' | '\t' | '\r'));
            }
            self.offset = end;
        }
        completed_fence
    }

    fn drain_prefix(&mut self, bytes: usize) {
        if let Some((start, _)) = &mut self.math {
            *start = start.saturating_sub(bytes);
        }
        self.offset = self.offset.saturating_sub(bytes);
        self.searched = self.searched.saturating_sub(bytes);
        self.pending.start = self.pending.start.saturating_sub(bytes);
        self.pending.checked = self.pending.checked.saturating_sub(bytes);
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
    // Raw tail bytes represented by the preview; any withheld suffix remains
    // immediately available to raw and semantic-copy consumers.
    preview_source_len: usize,
    // Whether the represented source already contains a newline. This keeps
    // append-local preview classification from rescanning an immutable prefix.
    preview_source_has_newline: bool,
    // Ordinary prose is previewed with Markdown's soft-break geometry from its
    // first visible line. The source itself remains in `tail`/`raw`; these
    // fields only track the normalized display projection between parser
    // commits.
    preview_prose: bool,
    prose_pending_soft_break: bool,
    prose_at_boundary: bool,
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

    /// Construct an already complete document with one canonical parse.
    ///
    /// Unlike [`Self::from_text`] followed by [`Self::finish`], this skips
    /// provisional fence scanning, suffix copies and preview parsing. Exact
    /// UTF-8 source and final semantics are identical; further pushes are ignored.
    pub fn from_finalized_text(text: &str) -> Self {
        Self {
            raw: text.as_bytes().to_vec(),
            decoded: text.to_owned(),
            committed: markdown::parse(text),
            finished: true,
            committed_revision: 1,
            tail_revision: 1,
            stats: StreamingStats {
                parse_passes: 1,
                reparsed_bytes: text.len() as u64,
                ..StreamingStats::default()
            },
            ..Self::default()
        }
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
        let completed_fence = self.scanner.scan(&self.tail);
        let before = (
            self.preview_source_len,
            self.preview_epoch,
            self.committed_revision,
        );
        self.stabilize(
            had_open_fence,
            decoded_chunk.contains('\n'),
            completed_fence,
            proven_boundary,
        );
        if before
            != (
                self.preview_source_len,
                self.preview_epoch,
                self.committed_revision,
            )
        {
            self.tail_revision = self.tail_revision.saturating_add(1);
        }
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

    /// Current semantic copy text, including accepted source withheld from the
    /// live preview while a structural line is incomplete.
    pub fn copy_text(&self) -> String {
        let mut output = self.committed.plain_text();
        let tail = self.preview.plain_text();
        if !output.is_empty() && !output.ends_with('\n') && !tail.is_empty() {
            output.push('\n');
        }
        output.push_str(&tail);
        if !self.finished {
            output.push_str(&self.tail[self.preview_source_len..]);
        }
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
        self.preview_source_len = 0;
        self.preview_source_has_newline = false;
        self.preview_prose = false;
        self.prose_pending_soft_break = false;
        self.prose_at_boundary = false;
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
                self.preview = Document::new(vec![Block::CodeBlock(CodeBlock {
                    language: open.language.clone(),
                    code: String::new(),
                })]);
                self.preview_source_len = open.code_start;
                self.preview_source_has_newline = self.tail[..open.code_start].contains('\n');
                self.tail_semantic_parsed = true;
            }
            let end = self.presentation_end();
            if let [Block::CodeBlock(code)] = self.preview.blocks.as_mut_slice() {
                let start = self.preview_source_len;
                code.code.push_str(&self.tail[start..end]);
                self.stats.preview_copied_bytes += (end - start) as u64;
                self.preview_source_has_newline |= self.tail[start..end].contains('\n');
                self.preview_source_len = end;
            }
            return;
        }

        if completed_fence && self.tail.len() > MAX_UNSTABLE_PARSE_BYTES {
            // The closed fence is a proven boundary, not an oversized mutable
            // parse opportunity. Commit once instead of showing closing syntax
            // as literal payload after the preview budget has been exhausted.
            if let Some(end) = self.scanner.completed_fence_end {
                self.commit_prefix(end);
            }
        }
        let table = matches!(self.preview.blocks.first(), Some(Block::Table(_)));
        if table && !saw_newline {
            // A partial cell must not repeatedly reshape a previously painted
            // table. Raw/copy ingestion remains immediate.
            return;
        }
        if (table && self.tail.len() <= MAX_UNSTABLE_PARSE_BYTES)
            || (self.tail.len() <= MAX_LIVE_INLINE_PREVIEW_BYTES
                && (self.tail_semantic_parsed || likely_complete_inline(&self.tail)))
        {
            // Scheduling a newline is not a reason to demote an already-rich
            // preview to raw Markdown. Only publish a new semantic preview.
            self.parse_and_commit_stable_tail();
            return;
        }
        if !saw_newline {
            self.append_preview();
            return;
        }

        if self.tail_semantic_parsed
            && matches!(self.preview.blocks.as_slice(), [Block::Paragraph(_)])
            && !proven_boundary
            && !completed_fence
            && !had_open_fence
        {
            // An ordinary prose preview already uses Markdown's soft-break
            // geometry. Keep appending its literal source projection until a
            // proven structure or parser promotion requires replacement.
            self.append_preview();
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
                || self.scanner.completed_table_header
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
            self.append_preview();
        } else if proven_boundary {
            if let Some(offset) = lexical_stable_offset(
                &self.tail,
                &mut self.lexical_first,
                &mut self.stats.lexical_scanned_bytes,
            ) {
                if self.scanner.math.is_none_or(|(start, _)| offset <= start) {
                    self.commit_prefix(offset);
                    self.parse_and_commit_stable_tail();
                } else {
                    self.append_preview();
                }
            } else {
                self.append_preview();
            }
        } else {
            // An enormous single paragraph/list remains mutable. Display it as
            // safe literal text and parse it only once on completion.
            self.append_preview();
        }
    }

    fn parse_and_commit_stable_tail(&mut self) {
        let end = self.presentation_end();
        self.record_parse(end);
        let starts = markdown::top_level_block_starts(&self.tail[..end]);
        if starts.len() >= 2 {
            if let Some(offset) = starts.last().copied().filter(|offset| *offset > 0) {
                let offset = self
                    .scanner
                    .math
                    .map_or(offset, |(start, _)| offset.min(start));
                self.commit_prefix(offset);
            }
        }
        let mut end = self.presentation_end();
        self.record_parse(end);
        let mut preview = markdown::parse(&self.tail[..end]);
        if matches!(preview.blocks.last(), Some(Block::Table(_))) && end > self.scanner.offset {
            // Interpret only complete table rows; the next cell is provisional.
            end = self.scanner.offset;
            self.record_parse(end);
            preview = markdown::parse(&self.tail[..end]);
        }
        // Ordinary prose previews already use the same soft-break geometry as
        // canonical Markdown. Other parser-only structure is published only
        // when its semantic interpretation is safe to show.
        let rich = preview.blocks.iter().any(|block| match block {
            Block::Paragraph(content) => content.iter().any(|inline| {
                !matches!(
                    inline,
                    Inline::Text(_) | Inline::SoftBreak | Inline::HardBreak
                )
            }),
            Block::Plain(_) => true,
            _ => true,
        });
        if rich {
            self.preview_prose = false;
            self.prose_pending_soft_break = false;
            self.prose_at_boundary = false;
            let preview_source_has_newline = if self.preview_source_len <= end {
                self.preview_source_has_newline
                    || self.tail[self.preview_source_len..end].contains('\n')
            } else {
                self.tail[..end].contains('\n')
            };
            self.preview_epoch += 1;
            self.preview = preview;
            self.preview_source_len = end;
            self.preview_source_has_newline = preview_source_has_newline;
            self.tail_semantic_parsed = true;
        } else {
            self.append_preview();
        }
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
        self.preview_source_len = 0;
        self.preview_source_has_newline = false;
        self.preview_prose = false;
        self.prose_pending_soft_break = false;
        self.prose_at_boundary = false;
        self.committed_revision = self.committed_revision.saturating_add(1);
    }

    fn record_parse(&mut self, bytes: usize) {
        self.stats.parse_passes = self.stats.parse_passes.saturating_add(1);
        self.stats.reparsed_bytes = self.stats.reparsed_bytes.saturating_add(bytes as u64);
    }

    fn presentation_end(&mut self) -> usize {
        if self.scanner.math.is_some() {
            return self.tail.len();
        }
        self.scanner.pending.end(
            &self.tail,
            self.scanner.offset,
            self.scanner.open.as_ref(),
            &mut self.stats.preview_scanned_bytes,
        )
    }

    fn append_preview(&mut self) {
        let mut end = self.presentation_end();
        if matches!(self.preview.blocks.first(), Some(Block::Table(_))) {
            end = end.min(self.scanner.offset);
        }
        if end <= self.preview_source_len {
            return;
        }
        let start = self.preview_source_len;
        let suffix_len = end - start;
        let suffix_has_newline = self.tail[start..end].contains('\n');
        if self.preview_prose {
            if prose_suffix_requires_literal_preview(
                &self.tail[..start],
                &self.tail[start..end],
                &mut self.stats.preview_classified_bytes,
            ) {
                let source = self.tail[..end].to_owned();
                self.preview = Document::new(vec![Block::Plain(source)]);
                self.preview_prose = false;
                self.prose_pending_soft_break = false;
                self.prose_at_boundary = false;
                self.preview_epoch += 1;
                self.stats.preview_copied_bytes += suffix_len as u64;
            } else {
                self.stats.preview_copied_bytes += suffix_len as u64;
                append_prose_suffix(
                    &self.tail[start..end],
                    &mut self.preview,
                    &mut self.prose_pending_soft_break,
                    &mut self.prose_at_boundary,
                    &mut self.preview_epoch,
                );
            }
        } else {
            // A literal preview cannot be promoted to canonical prose until a
            // newline proves the current source line. Defer classification while
            // the accepted suffix has no newline; otherwise every append would
            // rescan the complete growing candidate.
            if !suffix_has_newline {
                self.append_literal_preview(start, end);
                self.preview_source_len = end;
                return;
            }

            // A parsed structural prefix (for example, a heading) may still
            // share the mutable tail with an ordinary paragraph. Classify the
            // suffix after that proven block boundary, rather than allowing the
            // prefix's syntax to force the paragraph back to physical source
            // rows.
            let structural_prefix = self.preview_source_len > 0
                && self.tail[..start].ends_with("\n\n")
                && matches!(
                    self.preview.blocks.last(),
                    Some(block) if !matches!(block, Block::Paragraph(_) | Block::Plain(_))
                );
            let candidate_start = if structural_prefix { start } else { 0 };
            let candidate = &self.tail[candidate_start..end];
            // A rich paragraph cannot become a plain-prose preview. Do not
            // repeatedly classify its growing source before discovering that.
            let promotable = self.preview.is_empty()
                || matches!(self.preview.blocks.as_slice(), [Block::Plain(_)])
                || structural_prefix;
            if promotable
                && ordinary_preview_start(candidate, &mut self.stats.preview_classified_bytes)
                && !prose_requires_literal_preview(
                    candidate,
                    &mut self.stats.preview_classified_bytes,
                )
            {
                let source_has_newline = self.preview_source_has_newline || suffix_has_newline;
                let replay_plain = !self.preview.is_empty()
                    && source_has_newline
                    && matches!(self.preview.blocks.as_slice(), [Block::Plain(_)]);
                if (self.preview.is_empty() && source_has_newline)
                    || replay_plain
                    || (structural_prefix && source_has_newline)
                {
                    let source_start = if replay_plain { 0 } else { start };
                    if replay_plain {
                        // A source line can be classified as ordinary only after
                        // it receives its first newline. Rebuild that short
                        // literal prefix as canonical prose once, before it can
                        // be committed as a different geometry.
                        self.preview = Document::default();
                        self.preview_source_len = 0;
                    }
                    self.preview_prose = true;
                    self.prose_pending_soft_break = false;
                    self.prose_at_boundary = false;
                    self.preview_epoch += 1;
                    self.stats.preview_copied_bytes += (end - source_start) as u64;
                    append_prose_suffix(
                        &self.tail[source_start..end],
                        &mut self.preview,
                        &mut self.prose_pending_soft_break,
                        &mut self.prose_at_boundary,
                        &mut self.preview_epoch,
                    );
                } else {
                    self.append_literal_preview(start, end);
                }
            } else {
                self.append_literal_preview(start, end);
            }
        }
        self.preview_source_has_newline |= suffix_has_newline;
        self.preview_source_len = end;
    }

    fn append_literal_preview(&mut self, start: usize, end: usize) {
        let suffix = &self.tail[start..end];
        self.stats.preview_copied_bytes += suffix.len() as u64;
        match self.preview.blocks.last_mut() {
            Some(Block::Plain(text)) => text.push_str(suffix),
            Some(Block::Paragraph(content)) => {
                // Exhausting an inline parse budget must not restore Markdown
                // delimiters in the entire paragraph. Keep interpreted spans
                // and append a literal continuation until the next parse.
                if let Some(Inline::Raw(text)) = content.last_mut() {
                    text.push_str(suffix);
                } else {
                    content.push(Inline::Raw(suffix.to_owned()));
                    self.preview_epoch += 1;
                }
            }
            _ => {
                self.preview_epoch += 1;
                self.preview.blocks.push(Block::Plain(suffix.to_owned()));
            }
        }
    }
}

fn ordinary_preview_start(source: &str, classified: &mut u64) -> bool {
    *classified = classified.saturating_add(source.len() as u64);
    let line = source.split('\n').next().unwrap_or_default();
    !line.is_empty() && !line.trim().is_empty() && !literal_preview_line(line)
}

fn prose_requires_literal_preview(source: &str, classified: &mut u64) -> bool {
    *classified = classified.saturating_add(source.len() as u64);
    source.contains(['\t', '\r'])
        || source
            .chars()
            .any(|character| character.is_control() && character != '\n')
        || source
            .split('\n')
            .any(|line| literal_preview_line(line) || line_has_hard_break(line))
}

fn prose_suffix_requires_literal_preview(
    previous: &str,
    suffix: &str,
    classified: &mut u64,
) -> bool {
    *classified = classified.saturating_add(suffix.len() as u64);
    suffix.contains(['\t', '\r'])
        || suffix
            .chars()
            .any(|character| character.is_control() && character != '\n')
        || suffix
            .split('\n')
            .any(|line| literal_preview_line(line) || line_has_hard_break(line))
        || (previous.ends_with(' ') && suffix.starts_with(" \n"))
        || (previous.ends_with("  ") && suffix.starts_with('\n'))
        || (previous.ends_with('\\') && suffix.starts_with('\n'))
}

fn literal_preview_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let indent = line.len().saturating_sub(trimmed.len());
    let mut scanned = 0;
    indent >= 4
        || line.starts_with('\t')
        || trimmed.starts_with(['#', '>', '<', '|'])
        || is_list_marker(trimmed, &mut scanned)
        || opening_fence(trimmed).is_some()
        || matches!(trimmed, "---" | "***" | "___")
}

fn line_has_hard_break(line: &str) -> bool {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let without_spaces = line.trim_end_matches(' ');
    line.len().saturating_sub(without_spaces.len()) >= 2 || without_spaces.ends_with('\\')
}

fn append_prose_suffix(
    suffix: &str,
    preview: &mut Document,
    pending_soft_break: &mut bool,
    at_boundary: &mut bool,
    preview_epoch: &mut u64,
) {
    for piece in suffix.split_inclusive('\n') {
        let has_newline = piece.ends_with('\n');
        let line = piece.strip_suffix('\n').unwrap_or(piece);
        if has_newline && line.chars().all(char::is_whitespace) {
            *pending_soft_break = false;
            *at_boundary = true;
            continue;
        }
        if *at_boundary {
            preview.blocks.push(Block::Paragraph(Vec::new()));
            *preview_epoch = (*preview_epoch).saturating_add(1);
            *at_boundary = false;
        }
        if *pending_soft_break {
            append_prose_text(preview, " ");
            *pending_soft_break = false;
        }
        if !line.is_empty() {
            append_prose_text(preview, line);
        }
        if has_newline {
            *pending_soft_break = true;
        }
    }
}

fn append_prose_text(preview: &mut Document, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Block::Paragraph(content)) = preview.blocks.last_mut() {
        if let Some(Inline::Raw(raw)) = content.last_mut() {
            raw.push_str(text);
        } else {
            content.push(Inline::Raw(text.to_owned()));
        }
    } else {
        preview
            .blocks
            .push(Block::Paragraph(vec![Inline::Raw(text.to_owned())]));
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
/// and changed tail rows are rendered. Ordinary prose uses its canonical
/// soft-break geometry from the first preview; literal/open-code tails retain
/// proven visual rows; semantic promotion, reflow, and finalization may replace them.
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
    if count < 3 || (marker == '`' && line[count..].contains('`')) {
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

fn display_math_opening(line: &str) -> Option<(&'static str, &str)> {
    let mut trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    while let Some(rest) = trimmed.strip_prefix('>') {
        trimmed = rest.trim_start_matches(' ');
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            trimmed = rest.trim_start_matches(' ');
            break;
        }
    }
    if let Some(body) = trimmed.strip_prefix("$$") {
        Some(("$$", body))
    } else {
        trimmed.strip_prefix("\\[").map(|body| ("\\]", body))
    }
}

fn likely_complete_inline(source: &str) -> bool {
    let paired = |marker: char| source.matches(marker).nth(1).is_some();
    source.contains('$')
        || source.contains("\\(")
        || source.contains("\\[")
        || paired('*')
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
mod tests;
