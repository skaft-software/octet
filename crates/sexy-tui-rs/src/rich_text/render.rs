//! Width-aware terminal and plain-text rendering for semantic documents.
//!
//! [`RichRenderer`] is the whole public surface: it holds a theme, the terminal
//! capabilities, the layout options and a small syntax cache, and it turns a
//! [`Document`] into rows plus escape-free copy text. What it delegates to
//! lives in a sibling module, one per layout concern:
//!
//! - `lines` — the styled-row primitives every block layout is built from.
//! - `blocks` — the block dispatch and the prose, list, quote, detail and
//!   table row builders.
//! - `code` — code-block geometry: borders, headers, gutters and the
//!   `CodeLayout` the streaming tail also measures against.
//! - `wrap` — inline flattening, wrapping, clipping, tab expansion and the
//!   terminal-boundary sanitizer.
//! - `diffs` — promoting fenced `diff`/`patch` code into the semantic diff
//!   pipeline, plus the shell-command scoping a hunk header needs.
//! - `append_tail` — the incremental tail used while a block is still
//!   growing, which must agree with the static layouts above.
//!
//! What stays here is the part that is genuinely the renderer: the options and
//! output types, the syntax cache, and the entry points that choose a layout.

mod append_tail;
mod blocks;
mod code;
mod diffs;
mod lines;
mod wrap;

use std::cell::RefCell;
#[cfg(feature = "syntax-highlighting")]
use std::collections::{hash_map::DefaultHasher, HashMap, VecDeque};
#[cfg(feature = "syntax-highlighting")]
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use self::diffs::{diff_language_hint, restyle_ranges, shell_program_ranges};
use self::lines::{RichLine, RichRun};

// Re-exported rather than re-homed: `rich_text::stream` already reaches the
// streaming tail as `render::AppendOnlyTail`.
pub(in crate::rich_text) use self::append_tail::AppendOnlyTail;

use crate::capabilities::TerminalCapabilities;
use crate::rich_text::diff::{DiffLineKind, DiffRenderOptions, UnifiedDiff};
use crate::rich_text::{Block, CodeBlock, Document};
use crate::style::{Color, TextRole, TextStyle};
use crate::theme::Theme;
use crate::width::WidthPolicy;

/// Long-line policy for code blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeOverflow {
    /// Preserve source rows and clip visually. The semantic copy text remains
    /// complete.
    #[default]
    Clip,
    /// Wrap at grapheme boundaries with a hanging code indent.
    Wrap,
}

/// Marker used for unordered rich-text lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnorderedListMarker {
    #[default]
    Bullet,
    Dash,
}

impl UnorderedListMarker {
    const fn glyph(self, capabilities: TerminalCapabilities) -> &'static str {
        match (self, capabilities.unicode && !capabilities.plain) {
            (Self::Bullet, true) => "•",
            (Self::Bullet, false) => "*",
            (Self::Dash, true) => "—",
            (Self::Dash, false) => "-",
        }
    }
}

/// Rich-rendering options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderOptions {
    pub width: WidthPolicy,
    pub code_overflow: CodeOverflow,
    pub code_borders: bool,
    pub syntax_highlighting: bool,
    pub tables: bool,
    /// Size code surfaces and table columns from the viewport, not growing
    /// payloads. Earlier rows then keep their geometry as a stream appends.
    pub stable_block_geometry: bool,
    /// Optional reading lane for prose; code, diffs, diagrams and tables keep
    /// the viewport width. Indentation is included in this measure.
    pub prose_width: Option<u16>,
    pub unordered_list_marker: UnorderedListMarker,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: WidthPolicy::default(),
            code_overflow: CodeOverflow::Clip,
            code_borders: false,
            syntax_highlighting: cfg!(feature = "syntax-highlighting"),
            tables: true,
            stable_block_geometry: false,
            prose_width: None,
            unordered_list_marker: UnorderedListMarker::Bullet,
        }
    }
}

/// One rendered terminal row with a copyable escape-free equivalent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedLine {
    pub styled: String,
    pub plain: String,
}

/// Render output plus the original semantic copy text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedDocument {
    pub lines: Vec<RenderedLine>,
    pub copy_text: String,
}

impl RenderedDocument {
    pub fn styled_lines(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.styled.clone()).collect()
    }

    pub fn plain_lines(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.plain.clone()).collect()
    }

    pub fn styled_text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.styled.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn plain_text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.plain.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Observable cache counters used by benchmarks and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyntaxCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub bytes: usize,
}

/// Stateless layout plus a small renderer-local syntax cache. There is no
/// global render lock; a renderer is intended to live with one UI surface.
pub struct RichRenderer {
    theme: Theme,
    capabilities: TerminalCapabilities,
    options: RenderOptions,
    syntax_cache: RefCell<SyntaxCache>,
}

impl RichRenderer {
    pub fn new(
        mut theme: Theme,
        capabilities: TerminalCapabilities,
        mut options: RenderOptions,
    ) -> Self {
        theme.set_capabilities(capabilities);
        if capabilities.plain {
            options.syntax_highlighting = false;
        }
        Self {
            theme,
            capabilities,
            options,
            syntax_cache: RefCell::new(SyntaxCache::default()),
        }
    }

    pub fn plain() -> Self {
        let capabilities = TerminalCapabilities::plain();
        Self::new(
            Theme::with_capabilities(capabilities),
            capabilities,
            RenderOptions::default(),
        )
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn theme_mut(&mut self) -> &mut Theme {
        &mut self.theme
    }

    pub const fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }

    pub const fn options(&self) -> RenderOptions {
        self.options
    }

    pub fn set_options(&mut self, mut options: RenderOptions) {
        if self.capabilities.plain {
            options.syntax_highlighting = false;
        }
        self.options = options;
    }

    pub fn syntax_cache_stats(&self) -> SyntaxCacheStats {
        self.syntax_cache.borrow().stats()
    }

    pub fn render(&self, document: &Document, width: u16) -> RenderedDocument {
        self.render_document(document, width, self.options.syntax_highlighting)
    }

    /// Render source code with syntax foregrounds but without code-block
    /// chrome, padding, or background. This is useful for command transcripts,
    /// where the source should read as code while adjacent output stays neutral.
    pub fn render_inline_syntax(
        &self,
        source: &str,
        language: &str,
        width: u16,
    ) -> RenderedDocument {
        self.render_inline_syntax_with_wrap(source, language, width, false)
    }

    /// Syntax-preserving, whitespace-preferred preview wrapping. The original
    /// sanitized source remains the semantic copy, including wrap whitespace.
    pub fn render_inline_syntax_wrapped(
        &self,
        source: &str,
        language: &str,
        width: u16,
    ) -> RenderedDocument {
        self.render_inline_syntax_with_wrap(source, language, width, true)
    }

    fn render_inline_syntax_with_wrap(
        &self,
        source: &str,
        language: &str,
        width: u16,
        soft_wrap: bool,
    ) -> RenderedDocument {
        let source = self.sanitize(source);
        let code = CodeBlock::with_language(language, source.clone());
        let base_style = self.theme.style(TextRole::Code);
        let mut runs = self.highlighted(&code).map_or_else(
            || vec![RichRun::new(source.clone(), base_style, None)],
            |highlighted| {
                let mut runs = Vec::new();
                for (line_index, line) in highlighted.iter().enumerate() {
                    if line_index > 0 {
                        runs.push(RichRun::new("\n".to_owned(), base_style, None));
                    }
                    for region in line {
                        runs.push(RichRun::new(
                            self.sanitize(&region.text),
                            region.role.map_or(base_style, |role| {
                                base_style.merge(self.theme.style(role))
                            }),
                            None,
                        ));
                    }
                }
                runs
            },
        );
        if matches!(
            language.trim().to_ascii_lowercase().as_str(),
            "bash" | "sh" | "shell" | "zsh"
        ) {
            let program_style = base_style.merge(self.theme.style(TextRole::SyntaxFunction));
            runs = restyle_ranges(runs, &shell_program_ranges(&source), program_style);
        }
        let width = usize::from(width).max(1);
        let lines = (if soft_wrap {
            self.wrap_runs(&runs, width)
        } else {
            self.hard_wrap_runs(&runs, width)
        })
        .into_iter()
        .map(|line| self.encode_line(line, width))
        .collect();
        RenderedDocument {
            lines,
            copy_text: source,
        }
    }

    /// Render a document on a fixed row surface. The background is composed
    /// into every semantic run before ANSI encoding, so syntax foregrounds and
    /// inline resets cannot punch holes in the surface. Rows are padded to the
    /// requested width.
    pub fn render_on_background(
        &self,
        document: &Document,
        width: u16,
        background: Color,
    ) -> RenderedDocument {
        let width = usize::from(width);
        let mut rich_lines = self.render_blocks(
            &document.blocks,
            width,
            false,
            self.options.syntax_highlighting,
        );
        for line in &mut rich_lines {
            for run in &mut line.runs {
                run.style.background = background;
            }
            let current_width = self.line_width(line);
            if current_width < width {
                line.push(
                    " ".repeat(width - current_width),
                    TextStyle::plain().background(background),
                    None,
                );
            }
        }
        RenderedDocument {
            lines: rich_lines
                .into_iter()
                .map(|line| self.encode_line(line, width))
                .collect(),
            copy_text: self.sanitize(&document.plain_text()),
        }
    }

    /// Render an unstable streaming suffix without syntax work that would be
    /// immediately invalidated by the next token.
    pub fn render_unstable(&self, document: &Document, width: u16) -> RenderedDocument {
        self.render_document(document, width, false)
    }

    pub(super) fn render_unstable_lines(
        &self,
        document: &Document,
        width: u16,
    ) -> Vec<RenderedLine> {
        let width = usize::from(width);
        self.render_blocks(&document.blocks, width, false, false)
            .into_iter()
            .map(|line| self.encode_line(line, width))
            .collect()
    }

    fn render_document(
        &self,
        document: &Document,
        width: u16,
        syntax_highlighting: bool,
    ) -> RenderedDocument {
        let width = usize::from(width);
        let rich_lines = self.render_blocks(&document.blocks, width, false, syntax_highlighting);
        RenderedDocument {
            lines: rich_lines
                .into_iter()
                .map(|line| self.encode_line(line, width))
                .collect(),
            copy_text: self.sanitize(&document.plain_text()),
        }
    }

    /// Render a block slice. Streaming caches use this to append newly
    /// committed blocks without cloning the complete document.
    pub fn render_blocks_only(&self, blocks: &[Block], width: u16) -> Vec<RenderedLine> {
        let width = usize::from(width);
        self.render_blocks(blocks, width, false, self.options.syntax_highlighting)
            .into_iter()
            .map(|line| self.encode_line(line, width))
            .collect()
    }

    /// Render one parser-committed block and return exclusive row boundaries
    /// for stable semantic units within it. Lists expose item boundaries and
    /// tables expose completed row boundaries; other blocks commit atomically.
    pub fn render_block_with_commit_ends(
        &self,
        block: &Block,
        width: u16,
    ) -> (Vec<RenderedLine>, Vec<usize>) {
        let width = usize::from(width);
        let (mut rich_lines, mut ends) = match block {
            Block::List(list) => {
                self.render_list_with_commit_ends(list, width, self.options.syntax_highlighting)
            }
            Block::Table(table) if self.options.tables => {
                self.render_table_with_commit_ends(table, width)
            }
            Block::Table(table) => self.render_table_fallback_with_commit_ends(table, width),
            _ => {
                let lines = self.render_block(block, width, self.options.syntax_highlighting);
                let end = (!lines.is_empty())
                    .then_some(lines.len())
                    .into_iter()
                    .collect();
                (lines, end)
            }
        };
        while rich_lines.last().is_some_and(RichLine::is_empty) {
            rich_lines.pop();
        }
        if rich_lines.is_empty() {
            rich_lines.push(RichLine::default());
        }
        for end in &mut ends {
            *end = (*end).min(rich_lines.len());
        }
        ends.dedup();
        if ends.last().copied() != Some(rich_lines.len()) {
            ends.push(rich_lines.len());
        }
        let lines = rich_lines
            .into_iter()
            .map(|line| self.encode_line(line, width))
            .collect();
        (lines, ends)
    }

    pub fn render_diff(
        &self,
        diff: &UnifiedDiff,
        width: u16,
        options: DiffRenderOptions,
    ) -> RenderedDocument {
        let width = usize::from(width);
        let mut lines = Vec::new();
        let number_width = if options.line_numbers {
            diff.lines
                .iter()
                .flat_map(|line| [line.old_number, line.new_number])
                .flatten()
                .max()
                .map_or(1, |number| number.to_string().len())
        } else {
            0
        };
        let mut language: Option<String> = None;
        for line in &diff.lines {
            if line.kind == DiffLineKind::FileHeader {
                if let Some(hint) = diff_language_hint(&line.text) {
                    language = Some(hint);
                }
            }
            let role = match line.kind {
                DiffLineKind::Addition => TextRole::DiffAdd,
                DiffLineKind::Removal => TextRole::DiffRemove,
                DiffLineKind::Context | DiffLineKind::Metadata | DiffLineKind::Binary => {
                    TextRole::DiffContext
                }
                DiffLineKind::HunkHeader => TextRole::DiffHunk,
                DiffLineKind::FileHeader => TextRole::DiffHeader,
            };
            let style = self.theme.style(role);
            let mut gutter_style = self.theme.style(TextRole::Subtle);
            gutter_style.background = style.background;
            let mut prefix = RichLine::default();
            if options.line_numbers {
                // A unified diff already marks additions/removals in the text
                // column. One location gutter is enough: prefer the resulting
                // (new) line number, falling back to the old number for a
                // deletion-only row.
                let number = line.new_number.or(line.old_number).map_or_else(
                    || " ".repeat(number_width),
                    |number| format!("{number:>number_width$}"),
                );
                prefix.push(format!("{number} | "), gutter_style, None);
            }
            let text = self.sanitize(&line.text);
            let content = RichLine {
                runs: self
                    .diff_code_runs(&text, line.kind, language.as_deref(), style)
                    .unwrap_or_else(|| vec![RichRun::new(text, style, None)]),
            };
            let prefix_width = self.line_width(&prefix);
            let available = width.saturating_sub(prefix_width);
            let rows = if options.wrap {
                self.wrap_runs(&content.runs, available)
            } else {
                vec![self.clip_runs(&content.runs, available)]
            };
            for (index, row) in rows.into_iter().enumerate() {
                let mut rendered = if index == 0 {
                    prefix.clone()
                } else {
                    let mut continuation = RichLine::default();
                    continuation.push(" ".repeat(prefix_width), gutter_style, None);
                    continuation
                };
                rendered.extend(row);
                // Pad to full width so diff backgrounds span the line.
                let current_width = self.line_width(&rendered);
                if current_width < width {
                    let pad_style = rendered
                        .runs
                        .last()
                        .map(|run| run.style)
                        .unwrap_or(self.theme.style(TextRole::DiffContext));
                    rendered.push(" ".repeat(width - current_width), pad_style, None);
                }
                lines.push(self.encode_line(rendered, width));
            }
        }
        RenderedDocument {
            lines,
            copy_text: self.sanitize(&diff.plain_text()),
        }
    }

    #[cfg(feature = "syntax-highlighting")]
    fn highlighted(&self, code: &CodeBlock) -> Option<Arc<Vec<super::highlight::HighlightedLine>>> {
        if !self.options.syntax_highlighting {
            return None;
        }
        let language = code.language.as_deref()?;
        self.syntax_cache
            .borrow_mut()
            .get_or_insert(language, &code.code)
    }

    #[cfg(not(feature = "syntax-highlighting"))]
    fn highlighted(&self, _code: &CodeBlock) -> Option<Arc<Vec<NeverHighlightedLine>>> {
        None
    }
}

#[cfg(not(feature = "syntax-highlighting"))]
type NeverHighlightedLine = Vec<NeverHighlightedRegion>;
#[cfg(not(feature = "syntax-highlighting"))]
struct NeverHighlightedRegion {
    text: String,
    role: Option<TextRole>,
}

/// Actual active-tail work, independent of CommonMark parser counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamingLayoutStats {
    /// Source bytes supplied to append eligibility/newline scans, including reflows.
    pub checked_bytes: u64,
    /// Source bytes inspected to measure content-fitting code geometry.
    pub measured_bytes: u64,
    /// Source bytes submitted to wrapping/clipping (including mutable-row replay).
    pub laid_out_bytes: u64,
    /// Text/link bytes materialized by the append cache for its prefix, layout
    /// inputs, and encoded output. Renderer-internal temporary copies and caller
    /// snapshots are not allocation totals and are not included here.
    pub copied_bytes: u64,
    /// Immutable rich inline prefixes flattened, once per semantic/reflow epoch.
    pub rich_prefix_layouts: u64,
    /// Append caches rejected because literal tabs/CR/controls need source maps.
    pub literal_transform_fallbacks: u64,
    /// Rows encoded, including mutable rows subsequently replaced.
    pub encoded_rows: u64,
    /// Tail renders using the general semantic/sanitizing layout path.
    pub full_tail_layouts: u64,
    /// Raw unstable-source bytes supplied to general (nonincremental) tail renders.
    pub fallback_source_bytes: u64,
}

#[cfg(feature = "syntax-highlighting")]
type CachedHighlightedLine = super::highlight::HighlightedLine;

#[derive(Default)]
struct SyntaxCache {
    #[cfg(feature = "syntax-highlighting")]
    entries: HashMap<SyntaxKey, Arc<Vec<CachedHighlightedLine>>>,
    #[cfg(feature = "syntax-highlighting")]
    order: VecDeque<SyntaxKey>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

#[cfg(feature = "syntax-highlighting")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SyntaxKey {
    language: String,
    hash: u64,
    length: usize,
}

impl SyntaxCache {
    #[cfg(feature = "syntax-highlighting")]
    fn get_or_insert(
        &mut self,
        language: &str,
        code: &str,
    ) -> Option<Arc<Vec<super::highlight::HighlightedLine>>> {
        // Unified diffs cache one highlighted source row at a time. Keep the
        // byte budget authoritative while allowing large histories of short
        // rows to survive a width reflow without cycling the cache.
        const MAX_ENTRIES: usize = 65_536;
        const MAX_BYTES: usize = 4 * 1024 * 1024;
        let mut hasher = DefaultHasher::new();
        code.hash(&mut hasher);
        let key = SyntaxKey {
            language: language.to_ascii_lowercase(),
            hash: hasher.finish(),
            length: code.len(),
        };
        if let Some(lines) = self.entries.get(&key) {
            self.hits = self.hits.saturating_add(1);
            return Some(lines.clone());
        }
        self.misses = self.misses.saturating_add(1);
        let lines = Arc::new(super::highlight_code(code, language)?);
        self.bytes = self.bytes.saturating_add(code.len());
        self.order.push_back(key.clone());
        self.entries.insert(key, lines.clone());
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                self.bytes = self.bytes.saturating_sub(oldest.length);
            }
        }
        Some(lines)
    }

    fn stats(&self) -> SyntaxCacheStats {
        SyntaxCacheStats {
            hits: self.hits,
            misses: self.misses,
            entries: {
                #[cfg(feature = "syntax-highlighting")]
                {
                    self.entries.len()
                }
                #[cfg(not(feature = "syntax-highlighting"))]
                {
                    0
                }
            },
            bytes: self.bytes,
        }
    }
}

#[cfg(test)]
mod tests;
