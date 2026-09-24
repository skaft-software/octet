//! Durable transcript-item projection into the interactive shell model.

use std::collections::HashMap;
use std::time::Duration;

use super::{
    is_subagent_tool, sanitize_for_terminal, AssistantBlock, CompactionBlock, ShellState,
    SubagentTranscript, ToolPanel, TranscriptBlock,
};
use crate::hydrate::TranscriptItem;
use crate::presentation::{
    summarize_tool, summarize_tool_with_workspace, tool_failure_reason, tool_result_is_failure,
};

fn apply_hydrated_tool_result(panel: &mut ToolPanel, text: &str, is_error: bool) {
    panel.finished = true;
    let replayed = Ok(octet_agent::ToolOutput::new(text.to_owned()));
    panel.is_error = is_error || tool_result_is_failure(&panel.name, &replayed);
    if !panel.is_error {
        panel.display.mark_media_read_from_result(text);
    }
    panel.failure_reason = if is_error {
        tool_failure_reason(
            &panel.name,
            &Err(octet_agent::ToolError::new(text.to_owned())),
        )
    } else {
        tool_failure_reason(&panel.name, &replayed)
    };
    panel.output.clear();
    panel.output.push_str(text);
}

pub(super) fn append_hydrated_items(
    state: &mut ShellState,
    items: impl IntoIterator<Item = TranscriptItem>,
) {
    state.render_publication.reset();
    // Index open duplicates once per hydration batch, not once per result.
    // The ordinary tool_panels index continues to identify the newest card for
    // repeated results; this temporary index also retains older open duplicates.
    let mut pending_by_id: HashMap<octet_ai::ToolCallId, Vec<usize>> = HashMap::new();
    for (index, block) in state.transcript.iter().enumerate() {
        if let TranscriptBlock::Tool(panel) = block {
            if !panel.finished {
                pending_by_id
                    .entry(panel.id.clone())
                    .or_default()
                    .push(index);
            }
        }
    }
    for item in items {
        match item {
            TranscriptItem::User {
                text,
                model_lab,
                prompt_color,
            } => {
                state.push_block(TranscriptBlock::User {
                    text,
                    model_lab,
                    prompt_color,
                    persisted: true,
                });
            }
            TranscriptItem::Assistant(text) => {
                state.push_block(TranscriptBlock::Assistant(Box::new(
                    AssistantBlock::finalized(text),
                )));
            }
            TranscriptItem::Reasoning(text) => {
                state.push_block(TranscriptBlock::Reasoning(Box::new(
                    AssistantBlock::finalized_reasoning(text),
                )));
            }
            TranscriptItem::ToolCall { id, name, args } => {
                if is_subagent_tool(&name) {
                    // Discovery/status calls remain hidden. Actual orchestration
                    // calls restore one bounded lifecycle row from durable
                    // call/result pairs without ever rendering their arguments.
                    if matches!(
                        name.as_str(),
                        "subagent_spawn" | "subagent_continue" | "subagent_wait" | "subagent_stop"
                    ) && state.hydrated_pending_subagent_calls.insert(id.clone())
                    {
                        if let Some(index) = state.transcript.iter().rposition(|block| {
                            matches!(block, TranscriptBlock::Subagents(summary) if summary.hydrated && summary.running > 0)
                        }) {
                            if let TranscriptBlock::Subagents(summary) = &mut state.transcript[index] {
                                summary.running += 1;
                            }
                            state.touch_block(index);
                        } else {
                            state.push_block(TranscriptBlock::Subagents(SubagentTranscript {
                                queued: 0,
                                running: 1,
                                succeeded: 0,
                                failed: 0,
                                stopped: 0,
                                hydrated: true,
                                live_workers: Vec::new(),
                                worker_ids: Vec::new(),
                            }));
                        }
                    }
                    state.hidden_hydrated_subagent_calls.insert(id, name);
                    continue;
                }
                state.hidden_hydrated_subagent_calls.remove(&id);
                state.hydrated_pending_subagent_calls.remove(&id);
                let display =
                    summarize_tool_with_workspace(&name, &args, state.workspace.as_deref());
                let model_lab = state.model_lab;
                let index = state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
                    id.clone(),
                    name,
                    args.to_string(),
                    display,
                    String::new(),
                    false,
                    false,
                    None,
                    model_lab,
                ))));
                pending_by_id.entry(id.clone()).or_default().push(index);
                state.tool_panels.insert(id, index);
            }
            TranscriptItem::ToolResult {
                id,
                text,
                is_error,
                duration_ms,
                images,
            } => {
                if let Some(name) = state.hidden_hydrated_subagent_calls.get(&id) {
                    if state.hydrated_pending_subagent_calls.remove(&id) {
                        let failed = is_error
                            || tool_result_is_failure(
                                name,
                                &Ok(octet_agent::ToolOutput::new(text.clone())),
                            );
                        if let Some(index) = state.transcript.iter().rposition(|block| {
                            matches!(block, TranscriptBlock::Subagents(summary) if summary.hydrated && summary.running > 0)
                        }) {
                            if let TranscriptBlock::Subagents(summary) = &mut state.transcript[index] {
                                summary.running -= 1;
                                if failed { summary.failed += 1; } else { summary.succeeded += 1; }
                            }
                            state.touch_block(index);
                        }
                    }
                    continue;
                }
                // Malformed provider output can reuse one call ID within the
                // same assistant turn. The durable protocol cannot identify
                // which duplicate a result belongs to, so conservatively close
                // every still-open matching card. Leaving an older duplicate
                // active would revive a spinner for work that cannot still be
                // running after process restart.
                if let Some(pending) = pending_by_id.remove(&id) {
                    for index in pending {
                        let registered_images = state.register_tool_images(images.clone());
                        if let Some(TranscriptBlock::Tool(panel)) = state.transcript.get_mut(index)
                        {
                            apply_hydrated_tool_result(panel, &text, is_error);
                            panel.images = registered_images;
                            panel.duration = duration_ms.map(Duration::from_millis);
                        }
                    }
                } else {
                    if state.tool_panels.contains_key(&id) {
                        let registered_images = state.register_tool_images(images);
                        if let Some(panel) = state.tool_output_mut(&id) {
                            apply_hydrated_tool_result(panel, &text, is_error);
                            panel.images = registered_images;
                            panel.duration = duration_ms.map(Duration::from_millis);
                        }
                    } else {
                        let model_lab = state.model_lab;
                        let registered_images = state.register_tool_images(images);
                        let mut panel = ToolPanel::new(
                            id.clone(),
                            "tool result".into(),
                            String::new(),
                            summarize_tool("tool result", &serde_json::Value::Null),
                            sanitize_for_terminal(&text),
                            true,
                            is_error,
                            is_error.then(|| {
                                tool_failure_reason(
                                    "tool result",
                                    &Err(octet_agent::ToolError::new(text.clone())),
                                )
                                .unwrap_or_else(|| "tool failed".into())
                            }),
                            model_lab,
                        );
                        panel.images = registered_images;
                        let index = state.push_block(TranscriptBlock::Tool(Box::new(panel)));
                        state.tool_panels.insert(id, index);
                    }
                }
            }
            TranscriptItem::CompactionMarker { summary } => {
                state.push_block(TranscriptBlock::Compaction(Box::new(CompactionBlock {
                    label: "Context compacted".into(),
                    summary,
                    expanded: false,
                })));
            }
            TranscriptItem::NativeCompactionMarker => {
                state.push_block(TranscriptBlock::Notice(
                    "Context compacted natively · opaque Responses state retained".into(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octet_ai::ToolCallId;

    #[test]
    fn restored_orchestration_settles_without_exposing_arguments_or_worker_output() {
        let mut state = ShellState::default();
        let spawn = ToolCallId("spawn".into());
        let wait = ToolCallId("wait".into());
        let call = |id: &ToolCallId, name: &str| TranscriptItem::ToolCall {
            id: id.clone(),
            name: name.into(),
            args: serde_json::json!({"prompt": "SECRET-ARGUMENT"}),
        };
        append_hydrated_items(&mut state, [call(&spawn, "subagent_spawn")]);
        append_hydrated_items(
            &mut state,
            [
                TranscriptItem::Assistant("parent progress".into()),
                call(&wait, "subagent_wait"),
            ],
        );
        assert!(
            matches!(state.transcript.last(), Some(TranscriptBlock::Subagents(summary)) if summary.running == 2)
        );
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolResult {
                id: spawn,
                text: "SECRET-WORKER-OUTPUT".into(),
                is_error: false,
                duration_ms: None,
                images: Vec::new(),
            }],
        );
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolResult {
                id: wait,
                text: "SECRET-FAILURE".into(),
                is_error: true,
                duration_ms: None,
                images: Vec::new(),
            }],
        );
        assert!(
            matches!(state.transcript.last(), Some(TranscriptBlock::Subagents(summary))
            if summary.running == 0 && summary.succeeded == 1 && summary.failed == 1
                && summary.hydrated && summary.settled_role() == "warning")
        );
        let restored = state.rendered_transcript(120).join("\n");
        assert!(
            restored.contains("parent progress")
                && restored
                    .contains("Subagents · activity recorded · orchestration failed · /subagents"),
            "{restored}"
        );
        assert!(!restored.contains("SECRET"), "{restored}");
        let copy = state
            .transcript
            .iter()
            .map(super::super::block_copy_text)
            .collect::<String>();
        assert!(
            !copy.contains("SECRET")
                && copy.contains("Subagents · activity recorded · orchestration failed"),
            "{copy}"
        );
        // Replayed status and discovery calls never become fabricated work.
        append_hydrated_items(
            &mut state,
            [call(&ToolCallId("status".into()), "subagent_status")],
        );
        assert_eq!(
            state
                .transcript
                .iter()
                .filter(|block| matches!(block, TranscriptBlock::Subagents(_)))
                .count(),
            1
        );
    }

    #[test]
    fn restored_successful_spawn_does_not_claim_the_child_completed() {
        let mut state = ShellState::default();
        let id = ToolCallId("spawn".into());
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolCall {
                id: id.clone(),
                name: "subagent_spawn".into(),
                args: serde_json::json!({"prompt": "SECRET-PENDING-CHILD"}),
            }],
        );
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolResult {
                id,
                text: "{\"child_id\":\"pending-child\"}".into(),
                is_error: false,
                duration_ms: None,
                images: Vec::new(),
            }],
        );
        let summary = match state.transcript.last() {
            Some(TranscriptBlock::Subagents(summary)) => summary,
            _ => panic!("expected restored orchestration row"),
        };
        assert!(summary.hydrated && summary.running == 0 && summary.settled_role() == "muted");
        let text = state.rendered_transcript(120).join("\n");
        assert!(
            text.contains("Subagents · activity recorded · /subagents"),
            "{text}"
        );
        assert!(
            !text.contains("completed")
                && !text.contains("pending-child")
                && !text.contains("SECRET"),
            "{text}"
        );
    }

    #[test]
    fn hydrated_ordinary_tool_after_active_orchestration_uses_inserted_index() {
        let mut state = ShellState::default();
        let spawn = ToolCallId("spawn".into());
        let read = ToolCallId("read".into());
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolCall {
                id: spawn,
                name: "subagent_spawn".into(),
                args: serde_json::json!({"prompt": "SECRET"}),
            }],
        );
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolResult {
                id: read.clone(),
                text: "ordinary output".into(),
                is_error: false,
                duration_ms: None,
                images: Vec::new(),
            }],
        );
        let index = state.tool_panels[&read];
        assert!(
            matches!(&state.transcript[index], TranscriptBlock::Tool(panel) if panel.output == "ordinary output")
        );
        assert!(
            matches!(state.transcript.last(), Some(TranscriptBlock::Subagents(summary)) if summary.running == 1)
        );
        append_hydrated_items(
            &mut state,
            [TranscriptItem::ToolResult {
                id: read,
                text: "updated ordinary output".into(),
                is_error: false,
                duration_ms: None,
                images: Vec::new(),
            }],
        );
        assert!(
            matches!(&state.transcript[index], TranscriptBlock::Tool(panel) if panel.output == "updated ordinary output")
        );
    }

    #[test]
    fn hydrated_result_index_preserves_duplicates_across_batches_and_repeated_results() {
        let mut state = ShellState::default();
        let id = ToolCallId("duplicate".into());
        append_hydrated_items(
            &mut state,
            ["first", "second"].map(|path| TranscriptItem::ToolCall {
                id: id.clone(),
                name: "read".into(),
                args: serde_json::json!({"path": path}),
            }),
        );
        let result = |text: &str| TranscriptItem::ToolResult {
            id: id.clone(),
            text: text.into(),
            is_error: false,
            duration_ms: Some(7),
            images: Vec::new(),
        };
        append_hydrated_items(
            &mut state,
            [result("first result"), result("repeated result")],
        );
        assert_eq!(state.transcript.len(), 2);
        for (index, block) in state.transcript.iter().enumerate() {
            let TranscriptBlock::Tool(panel) = block else {
                unreachable!()
            };
            assert!(panel.finished);
            assert_eq!(panel.duration, Some(Duration::from_millis(7)));
            assert_eq!(
                panel.output,
                if index == 0 {
                    "first result"
                } else {
                    "repeated result"
                }
            );
        }
        assert_eq!(state.tool_panels[&id], 1);
    }
}
