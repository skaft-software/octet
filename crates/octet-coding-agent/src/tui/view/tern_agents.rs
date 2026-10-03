//! Native telemetry, never inferred from tool output or model text.

use octet_tern::wire::{Kind, Node, Props};
use serde_json::json;

use super::{terminal_text::sanitize_for_terminal as safe, ShellState, SubagentStateGroup};

pub(super) fn node(shell: &ShellState) -> Option<Node> {
    let view = shell.subagent_activity.as_ref()?;
    let children: Vec<_> = if !view.telemetry.is_empty() {
        view.telemetry
            .iter()
            .map(|child| {
                let status = status(&child.state);
                let mut stats = json!({"tools":child.tool_use_count,"tokens":child.total_tokens,
                if matches!(status,"pending"|"running") {"age"} else {"took"}:child.elapsed_ms});
                if let Some(cost) = child.cost_microdollars {
                    stats["cost"] = json!(cost as f64 / 1_000_000.0);
                }
                let mut props = Props::new()
                    .set("name", safe(&child.task_name))
                    .set("status", status)
                    .set("model", safe(&child.model))
                    .set("collapsible", true)
                    .set("collapsed", true)
                    .set("stats", stats);
                if let Some(profile) = &child.profile {
                    props = props.set("agent", safe(profile));
                }
                if let Some(tool) = &child.current_tool {
                    props = props.set("tool", json!({"name":safe(tool)}));
                }
                let detail = child
                    .failure_reason
                    .as_ref()
                    .map(|reason| {
                        Node::new(
                            format!("subagent.{}.failure", child.child_id),
                            Kind::Text,
                            Props::new().set("text", safe(reason)).set("tone", "error"),
                        )
                    })
                    .into_iter()
                    .collect();
                Node::with_children(
                    format!("subagent.{}", child.child_id),
                    Kind::Agent,
                    props,
                    detail,
                )
            })
            .collect()
    } else {
        view.activities
            .iter()
            .map(|activity| {
                let state = super::subagent_activity_state_label(activity.state);
                let mut props = Props::new()
                    .set("name", safe(&activity.summary))
                    .set("status", status(state))
                    .set("agent", safe(&activity.kind))
                    .set("collapsible", false);
                if let Some(metrics) = activity.metrics {
                    let tokens = metrics
                        .input_tokens
                        .saturating_add(metrics.cache_read_tokens)
                        .saturating_add(metrics.cache_write_tokens)
                        .saturating_add(metrics.output_tokens);
                    let mut stats = json!({"tools":metrics.tool_calls,"tokens":tokens});
                    if let Some(cost) = metrics.cost_microdollars {
                        stats["cost"] = json!(cost as f64 / 1_000_000.0);
                    }
                    props = props.set("stats", stats);
                }
                Node::new(format!("subagent.{}", activity.id), Kind::Agent, props)
            })
            .collect()
    };
    if children.is_empty() {
        return None;
    }
    Some(Node::with_children(
        "subagents",
        Kind::Section,
        Props::new()
            .set("head", safe(&view.status_label))
            .set("collapsible", true)
            .set("collapsed", true),
        children,
    ))
}

fn status(state: &str) -> &'static str {
    match SubagentStateGroup::of_declared_state(state) {
        SubagentStateGroup::Running if state == "pending" => "pending",
        SubagentStateGroup::Running => "running",
        SubagentStateGroup::Completed => "done",
        SubagentStateGroup::Failed => "failed",
        SubagentStateGroup::Stopped => "aborted",
    }
}
