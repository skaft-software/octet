//! Immutable semantic publication and renderer-private materialization.
//!
//! The semantic owner retains the latest root, not a queue of deltas. Unchanged
//! branches and source segments are shared; layout/parser caches never cross the
//! ownership boundary. A slow consumer may skip roots, never accepted bytes.

use super::{ShellState, TranscriptBlock};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
enum Node<T> {
    Branch(Option<Arc<Node<T>>>, Option<Arc<Node<T>>>),
    Leaf(T),
}

#[derive(Debug)]
pub(super) struct SharedSequence<T> {
    root: Option<Arc<Node<T>>>,
    capacity: usize,
    len: usize,
}

impl<T> Clone for SharedSequence<T> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            capacity: self.capacity,
            len: self.len,
        }
    }
}
impl<T> Default for SharedSequence<T> {
    fn default() -> Self {
        Self {
            root: None,
            capacity: 1,
            len: 0,
        }
    }
}
impl<T> SharedSequence<T> {
    pub(super) fn len(&self) -> usize {
        self.len
    }
    pub(super) fn push(&mut self, value: T) {
        if self.len == self.capacity {
            self.root = Some(Arc::new(Node::Branch(self.root.take(), None)));
            self.capacity *= 2;
        }
        self.set(self.len, value);
        self.len += 1;
    }
    fn truncate(&mut self, len: usize) {
        fn prune<T>(
            node: Option<&Arc<Node<T>>>,
            size: usize,
            offset: usize,
            len: usize,
        ) -> Option<Arc<Node<T>>> {
            if offset >= len {
                return None;
            }
            if offset + size <= len {
                return node.cloned();
            }
            match node.map(Arc::as_ref) {
                Some(Node::Branch(left, right)) => Some(Arc::new(Node::Branch(
                    prune(left.as_ref(), size / 2, offset, len),
                    prune(right.as_ref(), size / 2, offset + size / 2, len),
                ))),
                _ => node.cloned(),
            }
        }
        if len >= self.len {
            return;
        }
        self.root = prune(self.root.as_ref(), self.capacity, 0, len);
        self.len = len;
    }
    fn set(&mut self, index: usize, value: T) {
        fn replace<T>(
            node: Option<&Arc<Node<T>>>,
            size: usize,
            index: usize,
            value: T,
        ) -> Arc<Node<T>> {
            if size == 1 {
                return Arc::new(Node::Leaf(value));
            }
            let (left, right) = match node.map(Arc::as_ref) {
                Some(Node::Branch(left, right)) => (left.clone(), right.clone()),
                _ => (None, None),
            };
            Arc::new(if index < size / 2 {
                Node::Branch(Some(replace(left.as_ref(), size / 2, index, value)), right)
            } else {
                Node::Branch(
                    left,
                    Some(replace(right.as_ref(), size / 2, index - size / 2, value)),
                )
            })
        }
        self.root = Some(replace(self.root.as_ref(), self.capacity, index, value));
    }
    pub(super) fn visit_from(&self, start: usize, mut visit: impl FnMut(usize, &T)) {
        fn walk<T>(
            node: Option<&Arc<Node<T>>>,
            size: usize,
            offset: usize,
            start: usize,
            visit: &mut impl FnMut(usize, &T),
        ) {
            if offset + size <= start {
                return;
            }
            match node.map(Arc::as_ref) {
                Some(Node::Leaf(value)) => visit(offset, value),
                Some(Node::Branch(left, right)) => {
                    walk(left.as_ref(), size / 2, offset, start, visit);
                    walk(right.as_ref(), size / 2, offset + size / 2, start, visit);
                }
                None => {}
            }
        }
        walk(self.root.as_ref(), self.capacity, 0, start, &mut visit);
    }
    fn changed(&self, previous: &Self, mut visit: impl FnMut(usize, &T)) {
        fn walk<T>(
            new: Option<&Arc<Node<T>>>,
            old: Option<&Arc<Node<T>>>,
            size: usize,
            offset: usize,
            visit: &mut impl FnMut(usize, &T),
        ) {
            if let (Some(new), Some(old)) = (new, old) {
                if Arc::ptr_eq(new, old) {
                    return;
                }
            }
            match new.map(Arc::as_ref) {
                Some(Node::Leaf(value)) => visit(offset, value),
                Some(Node::Branch(left, right)) => {
                    let (old_left, old_right) = match old.map(Arc::as_ref) {
                        Some(Node::Branch(left, right)) => (left.as_ref(), right.as_ref()),
                        _ => (None, None),
                    };
                    walk(left.as_ref(), old_left, size / 2, offset, visit);
                    walk(
                        right.as_ref(),
                        old_right,
                        size / 2,
                        offset + size / 2,
                        visit,
                    );
                }
                None => {}
            }
        }
        let mut old = previous.clone();
        while old.capacity < self.capacity {
            old.root = Some(Arc::new(Node::Branch(old.root.take(), None)));
            old.capacity *= 2;
        }
        walk(
            self.root.as_ref(),
            old.root.as_ref(),
            self.capacity,
            0,
            &mut visit,
        );
    }
}

pub(super) type SharedText = SharedSequence<Arc<str>>;

/// The mutex only makes legacy RefCell-bearing value types shareable. There is
/// no mutation API, and this lock is never held by the semantic owner while a
/// renderer parses or lays out a block. Assistant payloads contain metadata only.
struct PublishedBlock {
    id: u64,
    revision: u64,
    value: Mutex<TranscriptBlock>,
    source: Option<SharedText>,
}
impl PublishedBlock {
    fn capture(state: &ShellState, index: usize) -> Self {
        let (value, source) = match &state.transcript[index] {
            TranscriptBlock::Assistant(block) => (
                TranscriptBlock::Assistant(Box::new(block.render_metadata())),
                Some(block.render_source.clone()),
            ),
            TranscriptBlock::Reasoning(block) => (
                TranscriptBlock::Reasoning(Box::new(block.render_metadata())),
                Some(block.render_source.clone()),
            ),
            TranscriptBlock::Tool(panel) => {
                // Tool-local diff/disclosure caches are renderer implementation
                // details, not part of immutable accepted tool meaning.
                let panel = super::ToolPanel {
                    id: panel.id.clone(),
                    name: panel.name.clone(),
                    args: panel.args.clone(),
                    display: panel.display.clone(),
                    output: panel.output.clone(),
                    images: panel.images.clone(),
                    image_rendering: panel.image_rendering,
                    finished: panel.finished,
                    is_error: panel.is_error,
                    duration: panel.duration,
                    failure_reason: panel.failure_reason.clone(),
                    extension_render_segments: panel.extension_render_segments.clone(),
                    progress_decoration: panel.progress_decoration.clone(),
                    subagent_activity: panel.subagent_activity.clone(),
                    model_lab: panel.model_lab,
                    cached_diff: Default::default(),
                    cached_disclosure_sensitive: Default::default(),
                };
                (TranscriptBlock::Tool(Box::new(panel)), None)
            }
            block => (block.clone(), None),
        };
        Self {
            id: state.transcript_commit_ids[index],
            revision: state.block_revisions[index],
            value: Mutex::new(value),
            source,
        }
    }
    fn materialize(&self, previous: Option<&mut TranscriptBlock>) -> TranscriptBlock {
        let mut next = self.value.lock().expect("immutable block poisoned").clone();
        if let Some(source) = &self.source {
            let reasoning = matches!(next, TranscriptBlock::Reasoning(_));
            let next_block = match &mut next {
                TranscriptBlock::Assistant(block) | TranscriptBlock::Reasoning(block) => block,
                _ => unreachable!(),
            };
            let old_block = previous.and_then(|block| match block {
                TranscriptBlock::Assistant(block) | TranscriptBlock::Reasoning(block) => {
                    Some(block)
                }
                _ => None,
            });
            next_block.materialize_source(source, old_block.map(Box::as_mut), reasoning);
        }
        next
    }
}

#[derive(Default)]
pub(super) struct Publication {
    root: SharedSequence<Arc<PublishedBlock>>,
    dirty: std::collections::HashSet<usize>,
    reset: bool,
}
impl Publication {
    pub(super) fn touch(&mut self, index: usize) {
        self.dirty.insert(index);
    }
    pub(super) fn reset(&mut self) {
        self.reset = true;
    }
    pub(super) fn remove(&mut self, index: usize, count: usize) {
        if index + 1 == count {
            self.root.truncate(index);
            self.dirty.retain(|dirty| *dirty < index);
        } else {
            self.reset();
        }
    }
}

pub(super) struct RenderModel {
    chrome: ShellState,
    geometry_fence: super::renderer_geometry::GeometryFence,
    root: SharedSequence<Arc<PublishedBlock>>,
    pub(super) revision: u64,
    reset: bool,
}

impl RenderModel {
    pub(super) fn capture(state: &mut ShellState) -> Self {
        let mut publication = std::mem::take(&mut state.render_publication);
        let reset = publication.reset || publication.root.len() > state.transcript.len();
        if reset {
            publication.root = SharedSequence::default();
        }
        let appended_from = publication.root.len();
        for index in appended_from..state.transcript.len() {
            publication
                .root
                .push(Arc::new(PublishedBlock::capture(state, index)));
        }
        for index in publication.dirty.drain() {
            if index < appended_from {
                publication
                    .root
                    .set(index, Arc::new(PublishedBlock::capture(state, index)));
            }
        }
        publication.reset = false;
        let root = publication.root.clone();
        state.render_publication = publication;
        let mut chrome = ShellState::default();
        copy_presentation(state, &mut chrome);
        Self {
            chrome,
            geometry_fence: super::renderer_geometry::GeometryFence::capture(state),
            root,
            revision: state.render_revision,
            reset,
        }
    }
}

#[derive(Default)]
pub(super) struct RenderOwner {
    pub(super) state: ShellState,
    pub(super) geometry_fence: Option<super::renderer_geometry::GeometryFence>,
    root: SharedSequence<Arc<PublishedBlock>>,
    pub(super) revision: u64,
}
impl RenderOwner {
    pub(super) fn accept(&mut self, model: RenderModel) {
        let state = &mut self.state;
        let structural = model.reset || state.transcript_epoch != model.chrome.transcript_epoch;
        let theme_changed = state.theme_epoch != model.chrome.theme_epoch;
        let size_changed = state.size != model.chrome.size;
        copy_presentation(&model.chrome, state);
        if structural {
            state.transcript.clear();
            state.transcript_commit_ids.clear();
            state.block_revisions.clear();
            *state.transcript_cache.get_mut() = Default::default();
            self.root = SharedSequence::default();
        }
        for count in (model.root.len() + 1..=state.transcript.len()).rev() {
            if !state
                .transcript_cache
                .get_mut()
                .truncate_tail_block(count - 1, count)
            {
                state.invalidate_transcript_layout();
            }
        }
        state.transcript.truncate(model.root.len());
        state.transcript_commit_ids.truncate(model.root.len());
        state.block_revisions.truncate(model.root.len());
        model.root.changed(&self.root, |index, block| {
            if index == state.transcript.len() {
                state.transcript.push(block.materialize(None));
                state.transcript_commit_ids.push(block.id);
                state.block_revisions.push(block.revision);
            } else {
                let same_identity = state.transcript_commit_ids[index] == block.id;
                if !same_identity
                    && !state
                        .transcript_cache
                        .get_mut()
                        .truncate_tail_block(index, state.transcript.len())
                {
                    state.invalidate_transcript_layout();
                }
                let previous = same_identity.then(|| &mut state.transcript[index]);
                let next = block.materialize(previous);
                state.transcript[index] = next;
                state.transcript_commit_ids[index] = block.id;
                state.block_revisions[index] = block.revision;
                state.transcript_cache.get_mut().dirty_blocks.push(index);
            }
            state.transcript_cache.get_mut().dirty = true;
        });
        // Welcome/overlay/chrome changes may affect the bounded prefix even
        // when every transcript node is shared with the previous publication.
        state.transcript_cache.get_mut().dirty = true;
        if theme_changed {
            state.invalidate_rich_text();
        }
        if size_changed {
            state.invalidate_transcript_layout();
        }
        self.root = model.root;
        self.revision = model.revision;
        self.geometry_fence = Some(model.geometry_fence);
        if let Some(top) = state.pending_panel_document_top {
            let rows =
                super::shell_chrome::shell_chrome(state, state.size.0, std::time::Instant::now())
                    .panel;
            if let Some(receipt) =
                super::renderer_geometry::PanelRenderReceipt::capture(state, &rows)
            {
                let maximum = receipt
                    .document_rows
                    .saturating_sub(receipt.document_body_rows);
                if let Some(super::Panel::ReadOnlyDocument {
                    scroll_from_bottom, ..
                }) = state.panel.as_mut()
                {
                    *scroll_from_bottom = maximum.saturating_sub(top);
                }
            }
        }
    }
}

fn copy_presentation(source: &ShellState, target: &mut ShellState) {
    // Queue roots and their immutable entries are Arc-shared: these clones
    // never duplicate ComposedInput parts or attachment/paste/media payloads.
    // TerminalImageStore likewise clones one Arc; deferred history contains
    // only a path, head ID and boundary ID, never the saved-session contents.
    macro_rules! fields { ($($field:ident),* $(,)?) => { $(target.$field = source.$field.clone();)* }; }
    fields!(
        available_update,
        late_update_notice,
        panel,
        panel_epoch,
        pending_panel_document_top,
        theme,
        safe_mode,
        image_rendering,
        theme_epoch,
        transcript_epoch,
        model_lab,
        prompt_color,
        event_dot_visible,
        event_spinner_frame,
        status_shimmer_frame,
        active_event_blocks,
        native_animation_viewport_top,
        history_prepended,
        steering_queue,
        follow_up_queue,
        input_modalities,
        workspace,
        file_index,
        terminal_images,
        deferred_session_history,
        path_selection,
        tool_input_revision,
        tool_input_prompt,
        prompt_templates,
        skill_commands,
        extension_commands,
        subagent_activity,
        subagent_activity_block,
        slash_selection,
        slash_scroll,
        slash_popup_dismissed,
        extension_ui,
        extension_autocomplete,
        status_detail,
        error,
        overlay,
        active_text,
        active_reasoning,
        application_viewport_requested,
        scroll_from_bottom,
        viewport_anchor,
        follow_tail,
        new_output_count,
        transcript_selection,
        context_estimate,
        last_turn_usage,
        last_turn_tokens_per_second,
        last_turn_generation_elapsed,
        last_turn_generated_tokens,
        turn_generation_started_at,
        turn_requested_at,
        last_turn_first_token,
        last_turn_provider_elapsed,
        turn_streamed_output_bytes,
        turn_output_tokens_before_generation,
        session_cost_microdollars,
        usage_uncertain,
        max_session_cost_microdollars,
        cache_hit_rate_basis_points,
        run_cost_microdollars,
        run_cost_available,
        run,
        session_work_elapsed,
        provider,
        model,
        model_display,
        model_compact_names,
        run_model,
        run_model_lab,
        run_prompt_color,
        run_model_display,
        run_model_compact_names,
        run_reasoning,
        run_price_display,
        run_context_estimate,
        telemetry_model,
        price_display,
        reasoning,
        run_label,
        verbose_tools,
        size,
        startup_pending,
        startup_card_started_at,
    );
    if target.editor.text() != source.editor.text() {
        target.editor.set_text(source.editor.text());
    }
    target.editor.set_cursor(source.editor.cursor());
    *target.transcript_navigation.get_mut() =
        source.transcript_navigation.borrow().render_snapshot();
}

#[cfg(test)]
mod tests {
    use super::super::{AssistantBlock, OutputChannel};
    use super::*;
    use std::sync::Weak;

    fn state() -> ShellState {
        ShellState {
            theme: crate::tui::theme::test_theme(),
            size: (80, 24),
            follow_tail: true,
            ..Default::default()
        }
    }

    #[test]
    fn fixed_tail_publication_shares_history_at_all_workload_sizes() {
        for history in [1_000, 10_000, 100_000] {
            let mut semantic = state();
            for _ in 0..history {
                semantic.push_block(TranscriptBlock::Notice("settled".into()));
            }
            semantic.push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::streaming("prefix"),
            )));
            let first = RenderModel::capture(&mut semantic);
            let TranscriptBlock::Assistant(assistant) = &mut semantic.transcript[history] else {
                unreachable!()
            };
            assistant.append(" suffix");
            semantic.touch_block(history);
            let next = RenderModel::capture(&mut semantic);
            let mut changed = Vec::new();
            next.root
                .changed(&first.root, |index, _| changed.push(index));
            assert_eq!(changed, [history]);
            let mut visited = 0;
            next.root.visit_from(history, |_, block| {
                let frozen = block.value.lock().unwrap();
                let TranscriptBlock::Assistant(metadata) = &*frozen else {
                    unreachable!()
                };
                assert!(
                    metadata.text.is_empty(),
                    "publication copied assistant prefix"
                );
                assert!(metadata.markdown.raw_text().is_empty());
                visited += 1;
            });
            assert_eq!(visited, 1);
        }
    }

    #[test]
    fn transient_tail_removal_shares_prefix_and_keeps_renderer_geometry() {
        let mut semantic = state();
        for index in 0..1_000 {
            semantic.push_block(TranscriptBlock::Notice(format!("settled {index}")));
        }
        semantic.push_block(TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::streaming(""),
        )));
        let first = RenderModel::capture(&mut semantic);
        let root = first.root.clone();
        let mut renderer = RenderOwner::default();
        renderer.accept(first);
        renderer.state.rendered_transcript(80);
        let prefix_length = renderer.state.transcript_cache.borrow().block_starts[1_000];
        semantic.remove_transient_activity_block(1_000);
        let next = RenderModel::capture(&mut semantic);
        assert!(!next.reset);
        let mut visited = 0;
        next.root.changed(&root, |_, _| visited += 1);
        assert_eq!(
            visited, 0,
            "tail deletion recopied historical block metadata"
        );
        renderer.accept(next);
        assert_eq!(
            renderer.state.transcript_cache.borrow().lines.len(),
            prefix_length
        );
        assert_eq!(renderer.state.transcript_cache.borrow().width, Some(80));
        // A new event in the same slot has a fresh identity even if its local
        // revision is zero, and cannot accidentally reuse the old status rows.
        semantic.push_block(TranscriptBlock::Notice("replacement outcome".into()));
        renderer.accept(RenderModel::capture(&mut semantic));
        assert!(renderer
            .state
            .rendered_transcript(80)
            .iter()
            .any(|line| line.contains("replacement outcome")));
    }

    #[test]
    fn skipped_publications_recover_exact_source_and_canonical_layout() {
        let mut semantic = state();
        semantic.append_text_block(OutputChannel::Text, "**prefix**\n\n");
        let mut renderer = RenderOwner::default();
        renderer.accept(RenderModel::capture(&mut semantic));
        renderer.state.rendered_transcript(80);
        for _ in 0..512 {
            semantic.append_text_block(OutputChannel::Text, "accepted β ");
        }
        // Coalescing skips every intermediate presentation, not source bytes.
        renderer.accept(RenderModel::capture(&mut semantic));
        let source_index = semantic.active_text.unwrap();
        let TranscriptBlock::Assistant(source) = &mut semantic.transcript[source_index] else {
            unreachable!()
        };
        source.finish();
        semantic.touch_block(source_index);
        renderer.accept(RenderModel::capture(&mut semantic));
        let TranscriptBlock::Assistant(source) = &semantic.transcript[source_index] else {
            unreachable!()
        };
        let TranscriptBlock::Assistant(rendered) = &renderer.state.transcript[source_index] else {
            unreachable!()
        };
        assert_eq!(source.text, rendered.text);
        assert_eq!(source.copy_text(), rendered.copy_text());
        assert_eq!(source.markdown.committed(), rendered.markdown.committed());
        assert_eq!(
            *semantic.rendered_transcript(80),
            *renderer.state.rendered_transcript(80)
        );
    }

    #[test]
    fn queued_payloads_are_shared_across_publication_and_queue_mutation() {
        use super::super::{composer, ComposedInput, QueuedSteering};
        let mut semantic = state();
        let payload = "large retained paste\n".repeat(32_768);
        let mut composed = ComposedInput::from_text(payload.clone());
        composed.attachments.push(composer::Attachment {
            id: 1,
            chip: "[paste #1]".into(),
            payload: composer::AttachmentPayload::PastedText(payload),
        });
        Arc::make_mut(&mut semantic.steering_queue).push(Arc::new(QueuedSteering {
            display: composed.transcript_text.clone(),
            editor_display: composed.display_text.clone(),
            attachments: composed.attachments.clone(),
        }));
        Arc::make_mut(&mut semantic.follow_up_queue).push_back(Arc::new(composed));
        let first = RenderModel::capture(&mut semantic);
        for _ in 0..32 {
            let next = RenderModel::capture(&mut semantic);
            assert!(Arc::ptr_eq(
                &semantic.steering_queue,
                &next.chrome.steering_queue
            ));
            assert!(Arc::ptr_eq(
                &semantic.follow_up_queue,
                &next.chrome.follow_up_queue
            ));
            assert!(Arc::ptr_eq(
                &first.chrome.steering_queue,
                &next.chrome.steering_queue
            ));
            assert!(Arc::ptr_eq(
                &first.chrome.follow_up_queue,
                &next.chrome.follow_up_queue
            ));
        }
        // Keeping a renderer revision alive does not make the next admission
        // duplicate any previously accepted paste, media, or editor payload.
        Arc::make_mut(&mut semantic.follow_up_queue)
            .push_back(Arc::new(ComposedInput::from_text("next".into())));
        assert!(!Arc::ptr_eq(
            &semantic.follow_up_queue,
            &first.chrome.follow_up_queue
        ));
        assert!(Arc::ptr_eq(
            &semantic.follow_up_queue[0],
            &first.chrome.follow_up_queue[0]
        ));
        Arc::make_mut(&mut semantic.steering_queue).push(Arc::new(QueuedSteering {
            display: "next".into(),
            editor_display: "next".into(),
            attachments: Vec::new(),
        }));
        assert!(Arc::ptr_eq(
            &semantic.steering_queue[0],
            &first.chrome.steering_queue[0]
        ));
    }

    #[test]
    fn replaced_snapshot_roots_are_not_retained_as_an_event_fifo() {
        let mut semantic = state();
        semantic.push_block(TranscriptBlock::Notice("one semantic block".into()));
        let mut renderer = RenderOwner::default();
        let mut old: Vec<Weak<Node<Arc<PublishedBlock>>>> = Vec::new();
        for _ in 0..1_000 {
            semantic.touch_block(0);
            let model = RenderModel::capture(&mut semantic);
            old.push(Arc::downgrade(model.root.root.as_ref().unwrap()));
            renderer.accept(model);
        }
        assert_eq!(old.iter().filter(|root| root.strong_count() > 0).count(), 1);
        assert_eq!(
            Arc::strong_count(renderer.root.root.as_ref().unwrap()),
            2,
            "only semantic publication and active renderer may retain latest root"
        );
    }
}
