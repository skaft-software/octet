use super::*;

fn state() -> ShellState {
    ShellState {
        theme: crate::tui::theme::test_theme(),
        size: (80, 24),
        follow_tail: true,
        ..Default::default()
    }
}

#[test]
fn unpainted_touches_keep_dirty_replacement_work_empty_at_all_history_sizes() {
    for count in [1_000, 10_000, 100_000] {
        let mut state = state();
        for _ in 0..count {
            let index = state.push_block(TranscriptBlock::Notice("accepted".into()));
            state.touch_block(index);
            state.touch_block(index);
            assert!(state.transcript_cache.borrow().dirty_blocks.is_empty());
        }
        let cache = state.transcript_cache.borrow();
        assert!(cache.dirty);
        assert!(cache.block_revisions.is_empty());
        assert_eq!(state.transcript_semantic_revision, 3 * count as u64);
        assert_eq!(state.block_revisions, vec![2; count]);
    }
}

#[test]
fn only_cached_touches_queue_replacements_and_new_rows_use_latest_revisions() {
    let mut state = state();
    for _ in 0..4 {
        state.push_block(TranscriptBlock::Notice("OLD-CACHED-SENTINEL".into()));
    }
    state.rendered_transcript(80);
    state.transcript[0] = TranscriptBlock::Notice("UPDATED-CACHED-SENTINEL".into());
    state.touch_block(0);
    state.touch_block(0);
    for _ in 0..1_000 {
        let index = state.push_block(TranscriptBlock::Notice("OLD-APPENDED-SENTINEL".into()));
        state.transcript[index] = TranscriptBlock::Notice("UPDATED-APPENDED-SENTINEL".into());
        state.touch_block(index);
        assert_eq!(state.transcript_cache.borrow().dirty_blocks, [0]);
    }
    let rendered = state.rendered_transcript(80).join("\n");
    assert!(rendered.contains("UPDATED-CACHED-SENTINEL"));
    assert!(rendered.contains("UPDATED-APPENDED-SENTINEL"));
    assert!(!rendered.contains("OLD-APPENDED-SENTINEL"));
    let cache = state.transcript_cache.borrow();
    assert_eq!(cache.block_revisions, state.block_revisions);
    assert!(cache.dirty_blocks.is_empty());
}

#[test]
fn invalidated_width_does_not_collect_replacements_for_a_mandatory_rebuild() {
    let mut state = state();
    for _ in 0..1_000 {
        state.push_block(TranscriptBlock::Notice("OLD-WIDTH-SENTINEL".into()));
    }
    state.rendered_transcript(80);
    state.invalidate_transcript_layout();
    for index in 0..state.transcript.len() {
        state.transcript[index] = TranscriptBlock::Notice("NEW-WIDTH-SENTINEL".into());
        state.touch_block(index);
        assert!(state.transcript_cache.borrow().dirty_blocks.is_empty());
    }
    assert!(state.transcript_cache.borrow().dirty);
    let rendered = state.rendered_transcript(80).join("\n");
    assert!(rendered.contains("NEW-WIDTH-SENTINEL"));
    assert!(!rendered.contains("OLD-WIDTH-SENTINEL"));
    assert_eq!(
        state.transcript_cache.borrow().block_revisions,
        state.block_revisions
    );
}
