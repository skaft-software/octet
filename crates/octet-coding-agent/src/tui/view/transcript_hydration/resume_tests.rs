use super::*;
use octet_ai::ToolCallId;

fn call(id: &str, name: &str) -> TranscriptItem {
    TranscriptItem::ToolCall {
        id: ToolCallId(id.into()),
        name: name.into(),
        args: serde_json::json!({"path": "public-path", "prompt": "SECRET-ARGUMENT"}),
    }
}

fn result(id: &str, text: &str, is_error: bool) -> TranscriptItem {
    TranscriptItem::ToolResult {
        id: ToolCallId(id.into()),
        text: text.into(),
        is_error,
        duration_ms: None,
        diff: None,
        images: Vec::new(),
    }
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        text: text.into(),
        model_lab: None,
        prompt_color: None,
    }
}

fn summary(running: usize, hydrated: bool) -> SubagentTranscript {
    SubagentTranscript {
        queued: 0,
        running,
        succeeded: usize::from(running == 0),
        failed: 0,
        stopped: 0,
        hydrated,
        live_workers: Vec::new(),
        worker_ids: Vec::new(),
    }
}

#[test]
fn subagent_hydration_inspects_history_once_then_only_the_live_tail() {
    const PAIRS: usize = 64;
    for history in [1_000, 10_000, 100_000] {
        let mut state = ShellState::default();
        for _ in 0..history {
            state.push_block(TranscriptBlock::Subagents(summary(0, true)));
        }
        let items = (0..PAIRS).flat_map(|index| {
            let id = format!("call-{index}");
            [
                call(&id, "subagent_spawn"),
                call(&id, "subagent_continue"),
                result(&id, "SECRET-RESULT", false),
                user("next prompt"),
            ]
        });
        HYDRATION_BLOCK_VISITS.with(|visits| visits.set(0));
        append_hydrated_items(&mut state, items);
        assert_eq!(
            HYDRATION_BLOCK_VISITS.with(|visits| visits.get()),
            history + 2 * PAIRS,
            "per-call lookup revisited settled history",
        );
        assert_eq!(state.transcript.len(), history + 2 * PAIRS);
        for block in &state.transcript[history..] {
            if let TranscriptBlock::Subagents(summary) = block {
                assert!(summary.hydrated);
                assert_eq!(
                    (summary.running, summary.succeeded, summary.failed),
                    (0, 1, 0)
                );
            }
        }
        assert!(state.hydrated_pending_subagent_calls.is_empty());
        assert!(state.transcript_cache.borrow().dirty_blocks.is_empty());
    }
}

fn state() -> ShellState {
    ShellState {
        theme: crate::tui::theme::test_theme_from_source(
            "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
        ),
        size: (100, 24),
        follow_tail: true,
        ..Default::default()
    }
}

fn projection(state: &ShellState) -> Vec<(String, Option<SubagentTranscript>)> {
    state
        .transcript
        .iter()
        .map(|block| {
            let summary = match block {
                TranscriptBlock::Subagents(summary) => Some(summary.clone()),
                _ => None,
            };
            (super::super::block_copy_text(block), summary)
        })
        .collect()
}

#[test]
fn summary_cursor_preserves_tail_moves_duplicates_failures_and_batch_boundaries() {
    let items = vec![
        call("duplicate", "subagent_spawn"),
        call("duplicate", "subagent_continue"),
        TranscriptItem::Reasoning("parent reasoning".into()),
        TranscriptItem::Assistant("parent progress".into()),
        call("wait", "subagent_wait"),
        call("read", "read"),
        call("read", "read"),
        result("orphan", "ordinary orphan output", false),
        TranscriptItem::NativeCompactionMarker,
        TranscriptItem::CompactionMarker {
            summary: "checkpoint".into(),
        },
        result("duplicate", "SECRET-FAILURE", true),
        result("duplicate", "SECRET-REPEATED-RESULT", false),
        result("read", "ordinary output", false),
        result("read", "updated ordinary output", false),
        call("status", "subagent_status"),
        result("status", "SECRET-DISCOVERY-FAILURE", true),
        result("wait", "SECRET-WORKER-OUTPUT", false),
        user("next wave"),
        call("duplicate", "subagent_stop"),
        TranscriptItem::Assistant("later parent progress".into()),
        result("duplicate", "SECRET-STOP-RESULT", false),
        call("duplicate", "read"),
        result("duplicate", "ordinary reused ID", false),
        user("finished"),
    ];
    let mut batch = state();
    append_hydrated_items(&mut batch, items.clone());
    let summaries: Vec<_> = batch
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Subagents(summary) => Some(summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(summaries.len(), 2);
    assert_eq!(
        (
            summaries[0].running,
            summaries[0].succeeded,
            summaries[0].failed
        ),
        (0, 1, 1)
    );
    assert_eq!(
        (
            summaries[1].running,
            summaries[1].succeeded,
            summaries[1].failed
        ),
        (0, 1, 0)
    );
    assert!(batch.hydrated_pending_subagent_calls.is_empty());
    let read_panels: Vec<_> = batch
        .transcript
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Tool(panel) if panel.id == ToolCallId("read".into()) => Some(panel),
            _ => None,
        })
        .collect();
    assert_eq!(read_panels.len(), 2);
    assert!(read_panels.iter().all(|panel| panel.finished));
    assert_eq!(read_panels[0].output, "ordinary output");
    assert_eq!(read_panels[1].output, "updated ordinary output");
    assert!(!batch
        .hidden_hydrated_subagent_calls
        .contains_key(&ToolCallId("duplicate".into())));
    assert!(!projection(&batch)
        .iter()
        .any(|(text, _)| text.contains("SECRET")));

    for chunk_size in [1, 2, 3, 5, items.len()] {
        let mut chunked = state();
        for chunk in items.chunks(chunk_size) {
            append_hydrated_items(&mut chunked, chunk.iter().cloned());
        }
        assert_eq!(
            projection(&chunked),
            projection(&batch),
            "batch size {chunk_size}"
        );
        assert_eq!(chunked.transcript_commit_ids, batch.transcript_commit_ids);
        assert_eq!(chunked.block_revisions, batch.block_revisions);
        assert_eq!(chunked.tool_panels, batch.tool_panels);
        assert_eq!(
            chunked.hidden_hydrated_subagent_calls,
            batch.hidden_hydrated_subagent_calls
        );
        assert_eq!(
            chunked.hydrated_pending_subagent_calls,
            batch.hydrated_pending_subagent_calls
        );
        assert_eq!(
            *chunked.rendered_transcript(100),
            *batch.rendered_transcript(100)
        );
    }
}

#[test]
fn a_hydrated_summary_below_a_live_roster_keeps_its_own_cursor() {
    let mut state = ShellState::default();
    append_hydrated_items(&mut state, [call("spawn", "subagent_spawn")]);
    let hydrated_id = state.transcript_commit_ids[0];
    state.push_block(TranscriptBlock::Subagents(summary(2, false)));
    append_hydrated_items(
        &mut state,
        [
            TranscriptItem::Assistant("new parent output".into()),
            call("wait", "subagent_wait"),
            result("spawn", "SECRET-RESULT", false),
            result("wait", "SECRET-FAILURE", true),
        ],
    );
    assert_eq!(state.transcript_commit_ids[0], hydrated_id);
    assert!(
        matches!(&state.transcript[0], TranscriptBlock::Subagents(summary)
        if summary.hydrated && summary.running == 0 && summary.succeeded == 1 && summary.failed == 1)
    );
    assert!(
        matches!(state.transcript.last(), Some(TranscriptBlock::Subagents(summary))
        if !summary.hydrated && summary.running == 2 && summary.succeeded == 0 && summary.failed == 0)
    );
}
