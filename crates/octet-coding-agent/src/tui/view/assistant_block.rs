//! Streaming assistant and reasoning block state with cached rich-text rendering.

use super::renderer_model::SharedText;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sexy_tui_rs::{
    parse_markdown, Block, Color, DiffRenderOptions, Inline, RichRenderer, StreamingLineUpdate,
    StreamingMarkdown, StreamingRenderCache, UnifiedDiff,
};

use super::terminal_text::sanitize_for_terminal;
use super::tool_render::looks_like_diff;
use crate::tui::theme::OctetTheme;

fn reasoning_markdown_projection(source: &str) -> String {
    // OpenAI-style reasoning summaries can concatenate independently bolded
    // sections without whitespace: `**Plan****Verify**`. CommonMark treats the
    // middle four asterisks as literal text inside one strong span. Insert a
    // display-only block boundary while retaining `AssistantBlock::text` as the
    // exact provider/session source.
    source
        .replace("****", "**\n\n**")
        .replace("____", "__\n\n__")
}

fn append_reasoning_inline_text(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Code(text) | Inline::Raw(text) => output.push_str(text),
            Inline::Styled(span) => append_reasoning_inline_text(&span.content, output),
            Inline::Role { content, .. }
            | Inline::Status { content, .. }
            | Inline::Emphasis(content)
            | Inline::Strong(content)
            | Inline::Strikethrough(content) => append_reasoning_inline_text(content, output),
            Inline::Link { label, .. } => append_reasoning_inline_text(label, output),
            Inline::SoftBreak | Inline::HardBreak => output.push(' '),
        }
    }
}

fn normalized_reasoning_heading(inlines: &[Inline]) -> Option<String> {
    let mut heading = String::new();
    append_reasoning_inline_text(inlines, &mut heading);
    let heading = sanitize_for_terminal(&heading);
    let heading = heading.split_whitespace().collect::<Vec<_>>().join(" ");
    (!heading.is_empty()).then_some(heading)
}

pub(super) fn reasoning_heading_from_block(block: &Block) -> Option<String> {
    match block {
        Block::Heading { content, .. } => normalized_reasoning_heading(content),
        Block::Paragraph(content) => {
            let mut meaningful = content.iter().filter(|inline| {
                !matches!(inline, Inline::Text(text) | Inline::Raw(text) if text.trim().is_empty())
            });
            let Inline::Strong(heading) = meaningful.next()? else {
                return None;
            };
            meaningful
                .next()
                .is_none()
                .then(|| normalized_reasoning_heading(heading))
                .flatten()
        }
        _ => None,
    }
}

fn reasoning_delimiter_crosses_chunk_boundary(previous: &str, next: &str) -> bool {
    ['*', '_'].into_iter().any(|marker| {
        let trailing = previous
            .chars()
            .rev()
            .take_while(|character| *character == marker)
            .take(3)
            .count();
        let leading = next
            .chars()
            .take_while(|character| *character == marker)
            .take(3)
            .count();
        trailing > 0 && leading > 0 && trailing + leading >= 4
    })
}

/// Presentation-only backoff; never retained as conversation or a raw cause.
#[derive(Clone, Debug)]
pub(super) struct RetryActivity {
    pub(super) operation: Option<octet_agent::ProviderOperation>,
    pub(super) attempt: usize,
    pub(super) max_attempts: Option<usize>,
    pub(super) delay: std::time::Duration,
    pub(super) observed_at: Instant,
}

impl RetryActivity {
    pub(super) fn label_at(&self, now: Instant) -> String {
        let remaining = self
            .delay
            .saturating_sub(now.saturating_duration_since(self.observed_at));
        let activity = match self.max_attempts {
            Some(limit) => format!("Retrying {}/{}", self.attempt, limit),
            None => format!("Waiting for network · attempt {}", self.attempt),
        };
        let activity = match self.operation {
            Some(octet_agent::ProviderOperation::LocalCompaction) => {
                format!("Local compaction · {activity}")
            }
            Some(octet_agent::ProviderOperation::NativeCompaction) => {
                format!("Native compaction · {activity}")
            }
            Some(octet_agent::ProviderOperation::TerminalGate) => {
                format!("Final-answer check · {activity}")
            }
            Some(octet_agent::ProviderOperation::BranchSummary) => {
                format!("Branch summary · {activity}")
            }
            None => activity,
        };
        if remaining.is_zero() {
            activity
        } else {
            format!(
                "{activity} in {}s",
                remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0)
            )
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct AssistantBlock {
    pub(super) text: String,
    /// Immutable accepted source segments; publication never copies the prefix.
    pub(super) render_source: SharedText,
    pub(super) markdown: StreamingMarkdown,
    pub(super) layout: RefCell<StreamingRenderCache>,
    /// Canonical copy projection, computed only when selection requests it.
    copy_text: RefCell<Option<String>>,
    /// Model that generated this block, for stable accent colour across
    /// model switches mid-session.
    pub(super) model_lab: Option<crate::tui::theme::ModelLab>,
    pub(super) finished: bool,
    /// Reasoning is retained verbatim but stays out of the mutable native
    /// scrollback tail until the user explicitly asks to inspect it.
    pub(super) reasoning_expanded: bool,
    /// First streamed reasoning delta, used to freeze elapsed timing when the
    /// block closes.
    pub(super) reasoning_started_at: Option<Instant>,
    /// Frozen reasoning duration after the block closes.
    pub(super) reasoning_elapsed: Option<Duration>,
    pub(super) retry_activity: Option<RetryActivity>,
    /// Start of the owning root run. Unlike reasoning timing, this survives
    /// steering, provider turns, and status-row replacement.
    pub(super) activity_started_at: Option<Instant>,
    /// Latest explicit ATX or standalone-bold heading emitted by the model.
    pub(super) reasoning_heading: Option<String>,
    /// Committed semantic blocks already inspected for reasoning headings.
    pub(super) reasoning_heading_committed_blocks: usize,
    /// Only the newest reasoning block advertises the global disclosure key.
    /// Older repeated hints become noise once a newer thinking event exists.
    pub(super) show_reasoning_hint: bool,
}

impl AssistantBlock {
    /// Copy only finite presentation metadata. Parser, source, copy, and row
    /// caches remain with their owner and are never cloned for publication.
    pub(super) fn render_metadata(&self) -> Self {
        let mut metadata = Self::streaming("");
        metadata.model_lab = self.model_lab.clone();
        metadata.finished = self.finished.clone();
        metadata.reasoning_expanded = self.reasoning_expanded.clone();
        metadata.reasoning_started_at = self.reasoning_started_at.clone();
        metadata.reasoning_elapsed = self.reasoning_elapsed.clone();
        metadata.retry_activity = self.retry_activity.clone();
        metadata.activity_started_at = self.activity_started_at.clone();
        metadata.reasoning_heading = self.reasoning_heading.clone();
        metadata.reasoning_heading_committed_blocks =
            self.reasoning_heading_committed_blocks.clone();
        metadata.show_reasoning_hint = self.show_reasoning_hint.clone();
        metadata
    }

    pub(super) fn materialize_source(
        &mut self,
        source: &SharedText,
        previous: Option<&mut Self>,
        reasoning: bool,
    ) {
        let finished = self.finished;
        let mut rendered = previous
            .map(|old| std::mem::replace(old, Self::streaming("")))
            .unwrap_or_else(|| {
                if reasoning {
                    Self::streaming_reasoning("")
                } else {
                    Self::streaming("")
                }
            });
        let start = rendered.render_source.len();
        source.visit_from(start, |_, segment| {
            if reasoning {
                rendered.append_reasoning(segment);
            } else {
                rendered.append(segment);
            }
        });
        if finished && !rendered.finished {
            if reasoning {
                rendered.finish_reasoning();
            } else {
                rendered.finish();
            }
        }
        self.text = rendered.text;
        self.markdown = rendered.markdown;
        self.layout = rendered.layout;
        self.copy_text = rendered.copy_text;
        self.render_source = source.clone();
    }

    pub(super) fn streaming(text: &str) -> Self {
        let mut markdown = StreamingMarkdown::new();
        markdown.push_str(text);
        let mut render_source = SharedText::default();
        if !text.is_empty() {
            render_source.push(Arc::from(text));
        }
        Self {
            text: text.to_owned(),
            render_source,
            markdown,
            layout: RefCell::new(StreamingRenderCache::default()),
            copy_text: RefCell::new(None),
            model_lab: None,
            finished: false,
            reasoning_expanded: false,
            reasoning_started_at: None,
            reasoning_elapsed: None,
            retry_activity: None,
            activity_started_at: None,
            reasoning_heading: None,
            reasoning_heading_committed_blocks: 0,
            show_reasoning_hint: true,
        }
    }

    pub(super) fn finalized(text: String) -> Self {
        let mut block = Self::streaming(&text);
        block.finish();
        block.text = text;
        block
    }

    pub(super) fn streaming_reasoning(text: &str) -> Self {
        let projection = reasoning_markdown_projection(text);
        let mut block = Self::streaming(&projection);
        block.text = text.to_owned();
        block.render_source = SharedText::default();
        if !text.is_empty() {
            block.render_source.push(Arc::from(text));
        }
        block.reasoning_started_at = Some(Instant::now());
        block.refresh_reasoning_heading();
        block
    }

    pub(super) fn finalized_reasoning(text: String) -> Self {
        let mut block = Self::streaming_reasoning(&text);
        // Hydrated sessions preserve reasoning text but do not currently store
        // provider-phase timing, so do not invent a duration on replay.
        block.reasoning_started_at = None;
        block.finish_reasoning();
        block
    }

    pub(super) fn with_model_lab(mut self, lab: Option<crate::tui::theme::ModelLab>) -> Self {
        self.model_lab = lab;
        self
    }

    pub(super) fn with_activity_started_at(mut self, started_at: Option<Instant>) -> Self {
        self.activity_started_at = started_at;
        self
    }

    pub(super) fn is_working_activity(&self) -> bool {
        !self.finished
            && self.text.is_empty()
            && !self.show_reasoning_hint
            && self.reasoning_heading.as_deref() == Some("Working")
    }

    pub(super) fn copy_text(&self) -> String {
        self.copy_text
            .borrow_mut()
            .get_or_insert_with(|| parse_markdown(self.markdown.raw_text()).plain_text())
            .clone()
    }

    pub(super) fn append(&mut self, text: &str) {
        if !text.is_empty() {
            self.render_source.push(Arc::from(text));
        }
        *self.copy_text.get_mut() = None;
        self.text.push_str(text);
        self.markdown.push_str(text);
    }

    pub(super) fn append_reasoning(&mut self, text: &str) {
        if !text.is_empty() {
            self.render_source.push(Arc::from(text));
        }
        *self.copy_text.get_mut() = None;
        let repairs_boundary = reasoning_delimiter_crosses_chunk_boundary(&self.text, text);
        self.text.push_str(text);
        if repairs_boundary {
            // This is rare (normally one boundary per provider summary
            // heading), so repair the cross-delta delimiter only when needed.
            self.markdown =
                StreamingMarkdown::from_text(&reasoning_markdown_projection(&self.text));
            self.reasoning_heading_committed_blocks = 0;
            self.invalidate_layout();
        } else {
            // Preserve the parser's committed prefix for ordinary token deltas.
            // Rebuilding here made verbose reasoning quadratic. Most deltas do
            // not contain the provider-specific adjacency at all, so avoid an
            // allocation on that hot path too.
            if text.contains("****") || text.contains("____") {
                self.markdown.push_str(&reasoning_markdown_projection(text));
            } else {
                self.markdown.push_str(text);
            }
        }
        self.refresh_reasoning_heading();
    }

    fn refresh_reasoning_heading(&mut self) {
        let (committed_blocks, heading) = {
            let committed = &self.markdown.committed().blocks;
            let start = self.reasoning_heading_committed_blocks.min(committed.len());
            let mut heading = committed[start..]
                .iter()
                .filter_map(reasoning_heading_from_block)
                .next_back();
            if let Some(preview_heading) = self
                .markdown
                .preview()
                .blocks
                .iter()
                .filter_map(reasoning_heading_from_block)
                .next_back()
            {
                heading = Some(preview_heading);
            }
            (committed.len(), heading)
        };
        self.reasoning_heading_committed_blocks = committed_blocks;
        if let Some(heading) = heading {
            self.reasoning_heading = Some(heading);
        }
    }

    pub(super) fn finish_reasoning(&mut self) {
        // A four-character emphasis boundary can straddle provider deltas. Fix
        // that rare boundary once at completion rather than reparsing the full
        // trace after every delta.
        let projection = reasoning_markdown_projection(&self.text);
        if self.markdown.raw_text() != projection {
            *self.copy_text.get_mut() = None;
            self.markdown = StreamingMarkdown::from_text(&projection);
            self.reasoning_heading_committed_blocks = 0;
            self.invalidate_layout();
        }
        if self.reasoning_elapsed.is_none() {
            self.reasoning_elapsed = self.reasoning_started_at.map(|started| started.elapsed());
        }
        self.finish();
        self.refresh_reasoning_heading();
    }

    pub(super) fn finish(&mut self) {
        self.markdown.finish();
        self.finished = true;
    }

    pub(super) fn invalidate_layout(&self) {
        *self.layout.borrow_mut() = StreamingRenderCache::default();
    }

    #[cfg(test)]
    pub(super) fn render(
        &self,
        renderer: &RichRenderer,
        theme: &OctetTheme,
        width: u16,
    ) -> Vec<String> {
        self.render_on_surface(renderer, theme, width, None)
    }

    pub(super) fn render_update(
        &self,
        renderer: &RichRenderer,
        theme: &OctetTheme,
        width: u16,
    ) -> Option<StreamingLineUpdate> {
        if self.finished || looks_like_diff(&self.text) {
            return None;
        }
        let use_plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
        Some(self.layout.borrow_mut().render_line_update(
            &self.markdown,
            renderer,
            width,
            !use_plain,
        ))
    }

    pub(super) fn render_on_surface(
        &self,
        renderer: &RichRenderer,
        theme: &OctetTheme,
        width: u16,
        background: Option<Color>,
    ) -> Vec<String> {
        // Blocks are rendered at the caller's exact content width. Every
        // transcript block shares the same outer baseline; semantic styling
        // supplies hierarchy without changing horizontal geometry.
        let use_plain = theme.capabilities().color == crate::tui::terminal::ColorDepth::None;
        if looks_like_diff(&self.text) {
            return renderer
                .render_diff(
                    &UnifiedDiff::parse(&self.text),
                    width,
                    DiffRenderOptions {
                        line_numbers: width >= 70,
                        wrap: true,
                    },
                )
                .lines
                .into_iter()
                .map(|line| if use_plain { line.plain } else { line.styled })
                .collect();
        }
        if self.finished && background.is_some_and(|background| background != Color::Default) {
            return renderer
                .render_on_background(
                    &parse_markdown(self.markdown.raw_text()),
                    width,
                    background.expect("checked above"),
                )
                .lines
                .into_iter()
                .map(|line| if use_plain { line.plain } else { line.styled })
                .collect();
        }
        self.layout
            .borrow_mut()
            .render_lines(&self.markdown, renderer, width, !use_plain)
    }
}

#[cfg(test)]
mod retry_activity_tests {
    use super::*;

    #[test]
    fn semantic_copy_cache_is_lazy_and_invalidated_by_source_changes() {
        let mut block = AssistantBlock::streaming("**First**");
        assert!(block.copy_text.borrow().is_none());
        assert_eq!(block.copy_text(), "First\n");
        assert_eq!(block.copy_text.borrow().as_deref(), Some("First\n"));
        assert_eq!(block.copy_text(), "First\n");
        block.append(" second");
        assert!(block.copy_text.borrow().is_none());
        assert_eq!(block.copy_text(), "First second\n");
        block.finish();
        assert_eq!(block.copy_text(), "First second\n");

        let mut reasoning = AssistantBlock::streaming_reasoning("**Plan**");
        reasoning.copy_text();
        reasoning.append_reasoning("**Verify**");
        assert!(reasoning.copy_text.borrow().is_none());
        let expected = parse_markdown(reasoning.markdown.raw_text()).plain_text();
        assert_eq!(reasoning.copy_text(), expected);
        reasoning.finish_reasoning();
        assert_eq!(reasoning.copy_text(), expected);
    }

    #[test]
    fn branch_summary_retry_is_named_separately_from_compaction() {
        let now = Instant::now();
        let activity = RetryActivity {
            operation: Some(octet_agent::ProviderOperation::BranchSummary),
            attempt: 2,
            max_attempts: Some(3),
            delay: Duration::from_secs(1),
            observed_at: now,
        };
        assert_eq!(
            activity.label_at(now),
            "Branch summary · Retrying 2/3 in 1s"
        );
        assert_eq!(
            activity.label_at(now + Duration::from_secs(2)),
            "Branch summary · Retrying 2/3"
        );
    }
}
