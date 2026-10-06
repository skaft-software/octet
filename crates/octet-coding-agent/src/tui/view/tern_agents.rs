//! One native orchestration transcript block; detailed telemetry stays in the inspector.

use octet_tern::wire::{Effect, Kind, Node, Props, Span};

use super::{terminal_text::sanitize_ordinary_surface_cell, SubagentTranscript};

/// Project the accepted conversation summary, not the inspector roster. Live
/// token lines are default activity, so verbose disclosure does not change them.
/// Commit-scoped ids keep successive waves separate even when task names repeat.
pub(super) fn transcript(identity: u64, view: &SubagentTranscript, _verbose: bool) -> Node {
    let id = |suffix: &str| format!("t{identity}.subagents{suffix}");
    let live = !view.hydrated && view.active_count() > 0;
    let mut marker = Span::styled("·", if live { "accent" } else { view.settled_role() });
    if live {
        marker = marker.effect(Effect::Pulse);
    }
    let label = view.label();
    // A native card head shares the other transcript surfaces' typography.
    // Bare chrome avoids another box or an invented tool lifecycle for history.
    let mut heading = vec![
        marker,
        Span::new(" "),
        Span::styled("Subagents", "strong toolTitle"),
        Span::styled(label.strip_prefix("Subagents").unwrap(), "muted"),
    ];
    if live {
        heading.push(Span::styled(" · stop: /subagents stop all", "muted"));
    }
    let mut body = Vec::new();
    if live {
        for (index, worker) in view.live_workers.iter().take(4).enumerate() {
            let estimate = if worker.output_estimated { "~" } else { "" };
            body.push(Node::with_children(
                id(&format!(".worker{index}")),
                Kind::Row,
                Props::new()
                    .set("gap", "xs")
                    .set("align", "start")
                    .set("wrap", false),
                vec![
                    Node::new(
                        id(&format!(".worker{index}.elbow")),
                        Kind::Text,
                        Props::new().text("spans", vec![Span::styled("└", "muted")]),
                    ),
                    Node::new(
                        id(&format!(".worker{index}.name")),
                        Kind::Text,
                        Props::new()
                            .text(
                                "spans",
                                vec![Span::styled(
                                    sanitize_ordinary_surface_cell(&worker.name, true),
                                    "text",
                                )],
                            )
                            .set("wrap", "none")
                            .set("truncate", "end"),
                    ),
                    Node::new(
                        id(&format!(".worker{index}.tokens")),
                        Kind::Text,
                        Props::new()
                            .text(
                                "spans",
                                vec![Span::styled(
                                    format!(
                                        "· ↑{} ↓{estimate}{}",
                                        compact_tokens(worker.input_tokens),
                                        compact_tokens(worker.output_tokens)
                                    ),
                                    "muted",
                                )],
                            )
                            .set("wrap", "none"),
                    ),
                ],
            ));
        }
        let shown = view.live_workers.len().min(4);
        let hidden = view.active_count().saturating_sub(shown);
        // No telemetry means no fabricated per-worker lines or token metrics.
        if hidden > 0 && shown > 0 {
            body.push(Node::new(
                id(".overflow"),
                Kind::Text,
                Props::new()
                    .text(
                        "spans",
                        vec![Span::styled(format!("└ +{hidden} more"), "muted")],
                    )
                    .set("wrap", "none"),
            ));
        }
    }
    Node::with_children(
        id(""),
        Kind::Card,
        Props::new()
            .role("octet.subagents")
            .set("variant", "bare")
            .text("head", heading),
        vec![Node::with_children(
            id(".body"),
            Kind::Col,
            Props::new().set("gap", "xs"),
            body,
        )],
    )
}

/// Display-only rounding matches the ordinary transcript; accounting is untouched.
fn compact_tokens(tokens: u64) -> String {
    let tokens = u128::from(tokens);
    let units = [
        (1_000, "K"),
        (1_000_000, "M"),
        (1_000_000_000, "B"),
        (1_000_000_000_000, "T"),
    ];
    let Some(mut index) = units.iter().rposition(|(scale, _)| tokens >= *scale) else {
        return tokens.to_string();
    };
    loop {
        let (scale, suffix) = units[index];
        let precision = if tokens < 100 * scale { 10 } else { 1 };
        let rounded = (tokens * precision + scale / 2) / scale;
        if rounded >= 1_000 * precision && index + 1 < units.len() {
            index += 1;
            continue;
        }
        return if precision == 10 && rounded % 10 != 0 {
            format!("{}.{}{suffix}", rounded / 10, rounded % 10)
        } else {
            format!("{}{suffix}", rounded / precision)
        };
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::tui::view::SubagentWorkerLine;

    fn summary() -> SubagentTranscript {
        SubagentTranscript {
            queued: 1,
            running: 1,
            succeeded: 0,
            failed: 0,
            stopped: 0,
            live_workers: vec![
                SubagentWorkerLine {
                    name: "Inspect".into(),
                    input_tokens: 1_250,
                    output_tokens: 3_900,
                    output_estimated: true,
                },
                SubagentWorkerLine {
                    name: "Audit".into(),
                    input_tokens: 70,
                    output_tokens: 0,
                    output_estimated: false,
                },
            ],
            hydrated: false,
            worker_ids: vec!["reused-worker".into()],
        }
    }

    fn nodes(root: &Node) -> Vec<&Node> {
        std::iter::once(root)
            .chain(root.c.as_deref().unwrap_or_default().iter().flat_map(nodes))
            .collect()
    }

    fn find<'a>(root: &'a Node, suffix: &str) -> &'a Node {
        nodes(root)
            .into_iter()
            .find(|node| node.id.ends_with(suffix))
            .unwrap()
    }

    fn text(root: &Node) -> String {
        nodes(root)
            .iter()
            .filter_map(|node| {
                let props = node.p.as_ref()?.as_map();
                props.get("spans").or_else(|| props.get("head"))?.as_array()
            })
            .flat_map(|spans| spans.iter().map(|span| span["t"].as_str().unwrap()))
            .collect()
    }

    fn marker(root: &Node) -> serde_json::Value {
        root.p.as_ref().unwrap().as_map()["head"][0].clone()
    }

    fn label(root: &Node) -> String {
        root.p.as_ref().unwrap().as_map()["head"]
            .as_array()
            .unwrap()[2..4]
            .iter()
            .map(|span| span["t"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn live_lines_and_stop_hint_are_default_activity_not_verbose_diagnostics() {
        let view = summary();
        let root = transcript(9, &view, false);
        assert_eq!(root, transcript(9, &view, true));
        assert_eq!(root.k, Kind::Card);
        assert_eq!(root.p.as_ref().unwrap().as_map()["variant"], "bare");
        assert_eq!(
            root.p.as_ref().unwrap().as_map()["head"][2]["s"],
            "strong toolTitle"
        );
        assert_eq!(label(&root), view.label());
        assert!(text(&root).contains("1 queued · 1 running"));
        assert!(text(&root).contains("stop: /subagents stop all"));
        assert_eq!(text(find(&root, ".worker0.tokens")), "· ↑1.3K ↓~3.9K");
        assert_eq!(text(find(&root, ".worker1.tokens")), "· ↑70 ↓0");
        assert_eq!(marker(&root)["fx"], "pulse");
        assert!(!text(&root).contains("more"));
        for node in nodes(&root) {
            assert!(matches!(
                node.k,
                Kind::Card | Kind::Row | Kind::Col | Kind::Text
            ));
            let props = node.p.as_ref().unwrap().as_map();
            if node.k == Kind::Row {
                assert_eq!(props["wrap"], false);
            }
            for forbidden in ["model", "cost", "stats", "prompt", "tool", "age", "took"] {
                assert!(!props.contains_key(forbidden), "unexpected {forbidden}");
            }
        }
    }

    #[test]
    fn commit_scoped_ids_are_unique_across_repeated_worker_waves() {
        let view = summary();
        let a = transcript(9, &view, false);
        let b = transcript(10, &view, false);
        let all = nodes(&a).into_iter().chain(nodes(&b)).collect::<Vec<_>>();
        let ids = all.iter().map(|node| &node.id).collect::<HashSet<_>>();
        assert_eq!(ids.len(), all.len());
        assert!(nodes(&a)
            .iter()
            .all(|node| node.id.starts_with("t9.subagents")));
        let mut updated = view;
        updated.live_workers[0].output_tokens += 10;
        let next = transcript(9, &updated, false);
        assert_eq!(
            nodes(&a).iter().map(|node| &node.id).collect::<Vec<_>>(),
            nodes(&next).iter().map(|node| &node.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn settlement_keeps_the_block_and_explicit_outcomes_but_removes_child_rows() {
        for (succeeded, failed, stopped, role, state) in [
            (2, 0, 0, "success", "2 completed"),
            (0, 2, 0, "error", "2 failed"),
            (1, 1, 0, "warning", "1 completed · 1 failed"),
            (0, 0, 2, "error", "2 stopped"),
            (1, 0, 1, "warning", "1 completed · 1 stopped"),
        ] {
            let mut view = summary();
            view.queued = 0;
            view.running = 0;
            view.succeeded = succeeded;
            view.failed = failed;
            view.stopped = stopped;
            view.live_workers.clear();
            let root = transcript(9, &view, false);
            assert_eq!(root.id, "t9.subagents");
            assert_eq!(label(&root), view.label());
            assert!(text(&root).contains(state));
            assert_eq!(marker(&root)["s"], role);
            assert!(marker(&root).get("fx").is_none());
            assert!(!nodes(&root).iter().any(|node| node.id.contains(".worker")
                || node.id.ends_with(".overflow")
                || node.id.ends_with(".stop")));
        }
    }

    #[test]
    fn hydrated_rows_are_neutral_evidence_not_completed_children() {
        for (running, failed, expected, role) in [
            (0, 0, "activity recorded", "muted"),
            (1, 0, "activity in progress", "muted"),
            (0, 1, "orchestration failed", "error"),
        ] {
            let mut view = summary();
            view.queued = 0;
            view.running = running;
            view.failed = failed;
            view.hydrated = true;
            view.live_workers.clear();
            let root = transcript(9, &view, false);
            assert_eq!(label(&root), view.label());
            assert!(text(&root).contains(expected));
            assert_eq!(marker(&root)["s"], role);
            assert!(marker(&root).get("fx").is_none());
            assert!(!text(&root).contains("completed"));
            assert!(!nodes(&root)
                .iter()
                .any(|node| node.id.contains(".worker") || node.id.ends_with(".stop")));
        }
    }

    #[test]
    fn four_live_workers_and_overflow_share_the_heading_content_column() {
        let mut view = summary();
        view.queued = 2;
        view.running = 5;
        view.live_workers = vec![view.live_workers[0].clone(); 4];
        let root = transcript(9, &view, false);
        let body = find(&root, ".body");
        assert_eq!(root.c.as_ref().unwrap()[0].id, body.id);
        assert_eq!(body.c.as_ref().unwrap().len(), 5); // four lines, overflow
        for index in 0..4 {
            assert_eq!(text(find(&root, &format!(".worker{index}.elbow"))), "└");
            assert_eq!(
                find(&root, &format!(".worker{index}.name"))
                    .p
                    .as_ref()
                    .unwrap()
                    .as_map()["truncate"],
                "end"
            );
        }
        assert_eq!(text(find(&root, ".overflow")), "└ +3 more");
        // Worker token metrics follow the name, not a distant right-hand HUD.
        assert!(!find(&root, ".worker0.name")
            .p
            .as_ref()
            .unwrap()
            .as_map()
            .contains_key("grow"));
        assert!(text(&root).contains("stop: /subagents stop all"));
    }

    #[test]
    fn missing_worker_telemetry_does_not_fabricate_token_lines_or_overflow() {
        let mut view = summary();
        view.live_workers.clear();
        let root = transcript(9, &view, false);
        assert!(text(&root).contains("1 queued · 1 running"));
        assert!(!text(&root).contains('↑'));
        assert!(!text(&root).contains("more"));
        assert!(find(&root, ".body").c.is_none());
    }

    #[test]
    fn unicode_names_remain_intact_and_cannot_inject_control_rows() {
        let mut view = summary();
        view.live_workers[0].name = "\x1b[31m調査 👩🏽‍💻 e\u{301}\x1b[0m\nnext\x07".into();
        let root = transcript(9, &view, false);
        let name = text(find(&root, ".worker0.name"));
        assert_eq!(name, "調査 👩🏽‍💻 e\u{301} next␇");
        assert!(!name.chars().any(char::is_control));
        assert!(serde_json::to_string(&root).unwrap().contains("調査"));
    }

    #[test]
    fn token_rounding_matches_the_conversation_units_without_integer_overflow() {
        for (tokens, expected) in [
            (999, "999"),
            (1_000, "1K"),
            (1_250, "1.3K"),
            (999_999, "1M"),
            (1_234_567, "1.2M"),
            (1_234_567_890, "1.2B"),
            (1_234_567_890_123, "1.2T"),
            (u64::MAX, "18446744T"),
        ] {
            assert_eq!(compact_tokens(tokens), expected);
        }
    }
}
