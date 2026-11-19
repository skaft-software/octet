//! Native queued-input chrome; queue ownership and recall remain with the host.

use octet_tern::wire::{Kind, Node, Props, Span};

use super::input_overlays::{pending_previews, PendingPreview};
use super::{tern_controls, ShellState};

pub(super) fn node(shell: &ShellState) -> Option<Node> {
    let count =
        shell.steering_queue.len() + shell.follow_up_queue.len() + shell.pending_controls.len();
    if count == 0 {
        return None;
    }
    let label = if !shell.pending_controls.is_empty() {
        "Controls"
    } else if shell.follow_up_queue.is_empty() {
        "Steering"
    } else if shell.steering_queue.is_empty() {
        "Follow-up"
    } else {
        "Input"
    };
    let editable = !shell.follow_up_queue.is_empty()
        || shell.steering_queue.iter().any(|entry| {
            entry
                .recall
                .as_ref()
                .is_some_and(|receipt| receipt.is_pending())
        });
    let mut heading = vec![
        Node::new(
            "pending.label",
            Kind::Text,
            Props::new().text("spans", vec![Span::styled(label, "strong")]),
        ),
        Node::new(
            "pending.state",
            Kind::Badge,
            Props::new()
                .set(
                    "text",
                    if count == 1 {
                        "queued".into()
                    } else {
                        format!("{count} queued")
                    },
                )
                .set("tone", "muted"),
        ),
    ];
    // No pointer action: showing a hint does not introduce another input owner.
    if editable {
        if let Some(key) = tern_controls::key(shell, "app.message.dequeue") {
            heading.push(Node::new(
                "pending.key",
                Kind::Kbd,
                Props::new().set("keys", [key]),
            ));
            heading.push(Node::new(
                "pending.edit",
                Kind::Text,
                Props::new().text("spans", vec![Span::styled("edit queued input", "muted")]),
            ));
        }
    }
    let previews = pending_previews(shell, usize::from(shell.size.0))
        .into_iter()
        .enumerate()
        .map(|(index, preview)| {
            let (text, props) = match preview {
                PendingPreview::Steering(text) => (
                    text,
                    Props::new().set("wrap", "none").set("truncate", "end"),
                ),
                PendingPreview::Wrapped(text) => (text, Props::new()),
            };
            Node::new(
                format!("pending.preview.{index}"),
                Kind::Text,
                props.text("spans", vec![Span::styled(text, "muted")]),
            )
        })
        .collect();
    Some(Node::with_children(
        "pending",
        Kind::Card,
        Props::new().role("octet.pending").set("frame", "card"),
        vec![
            Node::with_children(
                "pending.heading",
                Kind::Row,
                Props::new()
                    .set("gap", "xs")
                    .set("wrap", true)
                    .set("align", "center"),
                heading,
            ),
            Node::with_children("pending.preview", Kind::Col, Props::new(), previews),
        ],
    ))
}
