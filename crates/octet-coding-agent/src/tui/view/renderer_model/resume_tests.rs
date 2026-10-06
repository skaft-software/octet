use super::*;

fn state() -> ShellState {
    ShellState {
        size: (80, 24),
        follow_tail: true,
        ..ShellState::with_theme(crate::tui::theme::test_theme())
    }
}

#[test]
fn resolved_shell_initialization_and_publication_have_exact_theme_work() {
    use crate::tui::theme::{take_theme_work, test_theme};

    let theme = test_theme();
    let renderer_theme = theme.clone();
    take_theme_work();
    let mut semantic = ShellState::with_theme(theme);
    assert_eq!(
        take_theme_work(),
        (0, 0),
        "shell discarded a resolved theme"
    );
    let mut renderer = RenderOwner::new(renderer_theme);
    assert_eq!(take_theme_work(), (0, 0), "renderer rediscovered its theme");

    for revision in 0..32 {
        semantic.render_revision = revision;
        semantic.editor.set_text(format!("draft {revision}"));
        let model = RenderModel::capture(&mut semantic);
        assert_eq!(
            take_theme_work(),
            (0, 1),
            "capture must clone only its retained theme"
        );
        renderer.accept(model);
        assert_eq!(
            take_theme_work(),
            (0, 0),
            "accept must move the published theme"
        );
        assert_eq!(renderer.revision, revision);
        assert_eq!(renderer.state.editor.text(), semantic.editor.text());
    }
}

#[test]
fn shell_default_keeps_resolved_theme_and_nontrivial_field_defaults() {
    use crate::tui::theme::take_theme_work;

    take_theme_work();
    let shell = ShellState::default();
    assert_eq!(take_theme_work(), (1, 0));
    assert!(shell.theme.is_compiled_default());
    assert_eq!(shell.next_transcript_commit_id.0, 1_u64 << 63);
    assert!(shell.editor.text().is_empty());
    assert!(shell.transcript.is_empty());
    assert!(shell.render_geometry.is_none());
    assert!(!shell.follow_tail);
    assert!(!shell.image_rendering.enabled);
    assert!(shell.transcript_cache.borrow().width.is_none());
    assert_eq!(shell.render_publication.root.capacity, 1);
}

#[test]
fn initialized_test_shell_does_not_construct_a_discarded_theme() {
    use crate::tui::theme::{take_theme_work, test_theme};

    let theme = test_theme();
    take_theme_work();
    let shell = super::super::InteractiveShell::test_shell_with_theme(theme);
    // The first frame may clone styling for its own caches, but it must not
    // rediscover terminal capabilities or resolve another theme.
    assert_eq!(take_theme_work().0, 0);
    assert_eq!(shell.state.borrow().size, (120, 40));
    assert!(shell.state.borrow().follow_tail);
}

#[test]
fn resolved_theme_publication_preserves_encoded_frames_and_theme_swaps() {
    use super::super::{native_scrollback::render_shell_at, OutputChannel};

    let mut semantic = state();
    semantic.append_text_block(OutputChannel::Text, "**styled** `code`\n\nplain β");
    let mut renderer = RenderOwner::new(semantic.theme.clone());
    let now = std::time::Instant::now();
    for replacement in [false, true] {
        if replacement {
            semantic.theme.override_token("accent", "#b347cd");
            semantic.theme.override_token("text", "#589a78");
            semantic.theme_epoch += 1;
            semantic.invalidate_rich_text();
        }
        for size in [(46, 8), (80, 24), (120, 40)] {
            semantic.size = size;
            semantic.editor.set_text("editable β draft");
            renderer.accept(RenderModel::capture(&mut semantic));
            assert_eq!(
                render_shell_at(&semantic, size.0, now),
                render_shell_at(&renderer.state, size.0, now),
                "encoded frame changed at {size:?}, replacement={replacement}",
            );
        }
    }
}

fn values(sequence: &SharedSequence<usize>) -> Vec<usize> {
    let mut values = Vec::new();
    sequence.visit_from(0, |_, value| values.push(*value));
    values
}

fn assert_same_tree(left: Option<&Node<usize>>, right: Option<&Node<usize>>) {
    match (left, right) {
        (None, None) => {}
        (Some(Node::Leaf(left)), Some(Node::Leaf(right))) => assert_eq!(left, right),
        (Some(Node::Branch(ll, lr)), Some(Node::Branch(rl, rr))) => {
            assert_same_tree(ll.as_deref(), rl.as_deref());
            assert_same_tree(lr.as_deref(), rr.as_deref());
        }
        _ => panic!("bulk construction changed the persistent tree shape"),
    }
}

#[test]
fn bulk_sequence_matches_push_shape_and_preserves_persistent_updates() {
    for count in 0..130 {
        let bulk = SharedSequence::from_exact_iter(0..count);
        let mut pushed = SharedSequence::default();
        for value in 0..count {
            pushed.push(value);
        }
        assert_eq!(bulk.len, pushed.len);
        assert_eq!(bulk.capacity, pushed.capacity);
        assert_same_tree(bulk.root.as_deref(), pushed.root.as_deref());
        assert_eq!(values(&bulk), (0..count).collect::<Vec<_>>());
        if count == 0 {
            continue;
        }

        let mut next = bulk.clone();
        let changed_index = count / 2;
        SHARED_SEQUENCE_NODES.with(|nodes| nodes.set(0));
        next.set(changed_index, usize::MAX);
        assert_eq!(
            SHARED_SEQUENCE_NODES.with(|nodes| nodes.get()),
            bulk.capacity.trailing_zeros() as usize + 1,
        );
        let mut changed = Vec::new();
        next.changed(&bulk, |index, _| changed.push(index));
        assert_eq!(changed, [changed_index]);
        assert_eq!(values(&bulk), (0..count).collect::<Vec<_>>());

        let before_truncation = next.clone();
        next.truncate(count / 2);
        changed.clear();
        next.changed(&before_truncation, |index, _| changed.push(index));
        assert!(changed.is_empty(), "truncation replaced surviving leaves");
        assert_eq!(next.capacity, bulk.capacity);
        assert_eq!(values(&next), (0..count / 2).collect::<Vec<_>>());
        let truncated = next.clone();
        next.push(usize::MAX - 1);
        next.changed(&truncated, |index, _| changed.push(index));
        assert_eq!(changed, [count / 2]);
        let mut suffix = Vec::new();
        next.visit_from(count / 2, |index, value| suffix.push((index, *value)));
        assert_eq!(suffix, [(count / 2, usize::MAX - 1)]);
    }
}

fn assert_notice_publication(model: &RenderModel, semantic: &ShellState) {
    let mut visited = 0;
    model.root.visit_from(0, |index, block| {
        assert_eq!(block.id, semantic.transcript_commit_ids[index]);
        assert_eq!(block.revision, semantic.block_revisions[index]);
        let value = block.value.lock().unwrap();
        let (TranscriptBlock::Notice(actual), TranscriptBlock::Notice(expected)) =
            (&*value, &semantic.transcript[index])
        else {
            panic!("expected a notice publication");
        };
        assert_eq!(actual, expected);
        visited += 1;
    });
    assert_eq!(visited, semantic.transcript.len());
}

#[test]
fn initial_and_reset_publications_allocate_linear_nodes_with_exact_blocks() {
    for count in [1_000, 10_000, 100_000] {
        let mut semantic = state();
        for index in 0..count {
            semantic.push_block(TranscriptBlock::Notice(format!("accepted {index}")));
            semantic.touch_block(index);
        }
        SHARED_SEQUENCE_NODES.with(|nodes| nodes.set(0));
        let initial = RenderModel::capture(&mut semantic);
        assert!(SHARED_SEQUENCE_NODES.with(|nodes| nodes.get()) <= 3 * count);
        assert_notice_publication(&initial, &semantic);

        semantic.insert_block(7, TranscriptBlock::Notice("inserted prefix".into()));
        SHARED_SEQUENCE_NODES.with(|nodes| nodes.set(0));
        let reset = RenderModel::capture(&mut semantic);
        assert!(reset.reset);
        assert!(SHARED_SEQUENCE_NODES.with(|nodes| nodes.get()) <= 3 * (count + 1));
        assert_notice_publication(&reset, &semantic);
        // Retaining the old root does not make the new bulk build alter it.
        assert_eq!(initial.root.len(), count);
        let mut old_prefix = Vec::new();
        initial.root.visit_from(7, |index, block| {
            if index == 7 {
                let value = block.value.lock().unwrap();
                let TranscriptBlock::Notice(text) = &*value else {
                    unreachable!()
                };
                old_prefix.push(text.clone());
            }
        });
        assert_eq!(old_prefix, ["accepted 7"]);
    }
}

#[test]
fn an_emptied_publication_retains_capacity_for_later_append_and_materialization() {
    let mut semantic = state();
    for index in 0..9 {
        semantic.push_block(TranscriptBlock::Notice(format!("old {index}")));
    }
    let mut renderer = RenderOwner::new(semantic.theme.clone());
    let first = RenderModel::capture(&mut semantic);
    let capacity = first.root.capacity;
    renderer.accept(first);
    for index in (0..9).rev() {
        semantic.remove_transient_activity_block(index);
    }
    let empty = RenderModel::capture(&mut semantic);
    assert!(!empty.reset);
    assert_eq!(empty.root.len(), 0);
    assert_eq!(empty.root.capacity, capacity);
    renderer.accept(empty);
    semantic.push_block(TranscriptBlock::Notice("new tail".into()));
    let appended = RenderModel::capture(&mut semantic);
    assert!(!appended.reset);
    assert_eq!(appended.root.capacity, capacity);
    renderer.accept(appended);
    assert_eq!(renderer.state.transcript.len(), 1);
    assert!(
        matches!(&renderer.state.transcript[0], TranscriptBlock::Notice(text) if text == "new tail")
    );
}

#[test]
fn renderer_replacements_only_dirty_valid_cached_positions() {
    let mut semantic = state();
    for _ in 0..4 {
        semantic.push_block(TranscriptBlock::Notice("initial".into()));
    }
    let mut renderer = RenderOwner::new(semantic.theme.clone());
    renderer.accept(RenderModel::capture(&mut semantic));
    for round in 0..16 {
        for index in 0..4 {
            semantic.transcript[index] = TranscriptBlock::Notice(format!("accepted {round}"));
            semantic.touch_block(index);
        }
        renderer.accept(RenderModel::capture(&mut semantic));
        assert!(renderer
            .state
            .transcript_cache
            .borrow()
            .dirty_blocks
            .is_empty());
    }
    renderer.state.rendered_transcript(80);
    semantic.transcript[0] = TranscriptBlock::Notice("cached replacement".into());
    semantic.touch_block(0);
    for _ in 0..1_000 {
        let index = semantic.push_block(TranscriptBlock::Notice("new block".into()));
        semantic.touch_block(index);
    }
    renderer.accept(RenderModel::capture(&mut semantic));
    assert_eq!(renderer.state.transcript_cache.borrow().dirty_blocks, [0]);
    for index in 4..semantic.transcript.len() {
        semantic.transcript[index] = TranscriptBlock::Notice("latest new block".into());
        semantic.touch_block(index);
    }
    renderer.accept(RenderModel::capture(&mut semantic));
    assert_eq!(renderer.state.transcript_cache.borrow().dirty_blocks, [0]);
    assert_eq!(renderer.state.block_revisions, semantic.block_revisions);
    assert_eq!(
        *renderer.state.rendered_transcript(80),
        *semantic.rendered_transcript(80)
    );

    semantic.render_publication.reset();
    renderer.accept(RenderModel::capture(&mut semantic));
    assert!(renderer.state.transcript_cache.borrow().width.is_none());
    for index in 0..semantic.transcript.len() {
        semantic.transcript[index] = TranscriptBlock::Notice("reset replacement".into());
        semantic.touch_block(index);
    }
    renderer.accept(RenderModel::capture(&mut semantic));
    assert!(renderer
        .state
        .transcript_cache
        .borrow()
        .dirty_blocks
        .is_empty());
    assert_eq!(
        *renderer.state.rendered_transcript(80),
        *semantic.rendered_transcript(80)
    );
}
