//! Durable transcript-item projection into the interactive shell model.

use std::collections::{hash_map::Entry, HashMap};
use std::time::Duration;

use super::{
    is_subagent_tool, sanitize_for_terminal, AssistantBlock, CompactionBlock, ShellState, ToolPanel,
    TranscriptBlock,
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
            TranscriptItem::Assistant(text) => state.push_block(TranscriptBlock::Assistant(
                Box::new(AssistantBlock::finalized(text)),
            )),
            TranscriptItem::Reasoning(text) => state.push_block(TranscriptBlock::Reasoning(
                Box::new(AssistantBlock::finalized_reasoning(text)),
            )),
            TranscriptItem::ToolCall { id, name, args } => {
                if is_subagent_tool(&name) {
                    state.hidden_hydrated_subagent_calls.insert(id, name);
                    continue;
                }
                state.hidden_hydrated_subagent_calls.remove(&id);
                let index = state.transcript.len();
                let display =
                    summarize_tool_with_workspace(&name, &args, state.workspace.as_deref());
                let model_lab = state.model_lab;
                state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
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
                if state.hidden_hydrated_subagent_calls.contains_key(&id) {
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
                    let index = state.transcript.len();
                    match state.tool_panels.entry(id.clone()) {
                        Entry::Occupied(_) => {
                            let registered_images = state.register_tool_images(images);
                            if let Some(panel) = state.tool_output_mut(&id) {
                                apply_hydrated_tool_result(panel, &text, is_error);
                                panel.images = registered_images;
                                panel.duration = duration_ms.map(Duration::from_millis);
                            }
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(index);
                            let model_lab = state.model_lab;
                            let registered_images = state.register_tool_images(images);
                            let mut panel = ToolPanel::new(
                                id,
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
                            state.push_block(TranscriptBlock::Tool(Box::new(panel)));
                        }
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
