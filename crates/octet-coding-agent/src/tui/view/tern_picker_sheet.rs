//! Compact RAIL sheets for ordinary host-owned catalogues, never consent.
use super::*;

pub(super) fn node(
    shell: &ShellState,
    surface: &super::super::OrdinarySurfaceMetadata,
    items: &[String],
    descriptions: &[Option<String>],
    selected: usize,
    filter: &str,
    action_kind: &PanelAction,
) -> Node {
    let panel = id(shell);
    let safe = |text: &str| sanitize_ordinary_surface_cell(text, shell.theme.unicode());
    let order = filtered_indices_for_action(items, descriptions, action_kind, filter);
    let selected = order.get(selected).copied();
    let mut body = vec![Node::new(
        format!("{panel}.title"),
        Kind::Text,
        Props::new()
            .text("spans", vec![Span::styled(safe(&surface.title), "strong")])
            .set("wrap", "word"),
    )];
    if let Some(purpose) = &surface.purpose {
        body.push(Node::new(
            format!("{panel}.purpose"),
            Kind::Text,
            Props::new()
                .text("spans", vec![Span::styled(safe(purpose), "muted")])
                .set("wrap", "word"),
        ));
    }
    let (_, message) = surface.lifecycle.native_state(std::time::Instant::now());
    if let Some(message) = message {
        body.push(Node::new(
            format!("{panel}.status"),
            Kind::Text,
            Props::new()
                .text("spans", vec![Span::styled(safe(message), "muted")])
                .set("wrap", "word"),
        ));
    }
    body.push(Node::new(
        format!("{panel}.filter"),
        Kind::Editor,
        Props::new()
            .set("text", filter)
            .set("maxLines", 1)
            .set("placeholder", "Filter options…"),
    ));
    let item_id = |index| format!("{panel}.item.{index}");
    body.push(Node::with_children(
        &panel,
        Kind::List,
        Props::new()
            .set("selected", selected.map(item_id))
            .set("max", json!({"lines":8})),
        order
            .iter()
            .map(|index| {
                Node::new(
                    item_id(*index),
                    Kind::Item,
                    Props::new().text("label", safe(&items[*index])).text(
                        "detail",
                        descriptions
                            .get(*index)
                            .and_then(Option::as_deref)
                            .map(safe)
                            .unwrap_or_default(),
                    ),
                )
            })
            .collect(),
    ));
    body.push(Node::new(
        format!("{panel}.count"),
        Kind::Text,
        Props::new().text(
            "spans",
            vec![Span::styled(
                format!("{} of {} options", order.len(), items.len()),
                "muted",
            )],
        ),
    ));
    if order.is_empty() {
        body.push(Node::new(
            format!("{panel}.empty"),
            Kind::Text,
            Props::new().set("text", "No matches"),
        ));
    }
    if let Some(index) = selected {
        // Intrinsic Item labels can truncate; retain the full selected source.
        body.push(Node::new(
            format!("{panel}.selected"),
            Kind::Text,
            Props::new()
                .text("spans", vec![Span::styled(safe(&items[index]), "strong")])
                .set("wrap", "word"),
        ));
        if let Some(detail) = descriptions.get(index).and_then(Option::as_deref) {
            body.push(Node::new(
                format!("{panel}.detail"),
                Kind::Text,
                Props::new()
                    .text("spans", vec![Span::styled(safe(detail), "muted")])
                    .set("wrap", "word"),
            ));
        }
    }
    let controls = vec![
        Node::new(
            format!("{panel}.hint"),
            Kind::Text,
            Props::new().text(
                "spans",
                vec![Span::styled(
                    super::super::tern_controls::navigation(shell),
                    "muted",
                )],
            ),
        ),
        super::super::tern_controls::action(shell, &panel, "cancel", "Back", "tui.select.cancel"),
        super::super::tern_controls::action(
            shell,
            &panel,
            "confirm",
            "Select",
            "tui.select.confirm",
        ),
    ];
    body.push(Node::with_children(
        format!("{panel}.actions"),
        Kind::Row,
        Props::new().set("gap", "sm").set("wrap", true),
        controls,
    ));
    Node::with_children(
        format!("{panel}.sheet"),
        Kind::Overlay,
        Props::new()
            .role("octet.picker")
            .text("head", "octet")
            .set("modal", true)
            .set("size", "md"),
        body,
    )
}
