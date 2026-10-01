//! Data-first native pickers over the same host catalogues and filtering policy.

use octet_tern::wire::{Kind, Node, Props};
use serde_json::{json, Value};

use super::{
    filtered_indices_for_action, sanitize_ordinary_surface_cell, session_picker_ordering, Panel,
    PanelAction, ShellState,
};

pub(super) fn id(shell: &ShellState) -> String {
    format!("panel.{}", shell.panel_epoch)
}

pub(super) fn interactive(panel: &Panel) -> bool {
    match panel {
        Panel::SelectList { action, .. } => !matches!(action, PanelAction::Confirmation),
        Panel::SessionPicker { picker } => picker.rename.is_none() && !picker.confirming_delete,
        Panel::MessagePicker { .. } => true,
        Panel::ReadOnlyDocument { .. } => false,
    }
}

pub(super) fn node(shell: &ShellState) -> Option<Node> {
    let panel = shell.panel.as_ref()?;
    let safe = |value: &str| sanitize_ordinary_surface_cell(value, shell.theme.unicode());
    if let Panel::ReadOnlyDocument {
        title,
        text,
        styled,
        ..
    } = panel
    {
        return Some(Node::with_children(
            id(shell),
            Kind::Overlay,
            Props::new()
                .role("octet.document")
                .text("head", safe(title))
                .set("modal", true)
                .set("size", "lg"),
            vec![Node::new(
                "panel.document",
                if *styled { Kind::Ansi } else { Kind::Text },
                Props::new()
                    .set(
                        "text",
                        if *styled {
                            text.to_string()
                        } else {
                            safe(text)
                        },
                    )
                    .set("wrap", "word"),
            )],
        ));
    }
    if !interactive(panel) {
        return None;
    }
    let (surface, items, order, selected, query) = match panel {
        Panel::SelectList {
            surface,
            items,
            descriptions,
            selected,
            filter,
            action,
        } => {
            let order = filtered_indices_for_action(items, descriptions, action, filter);
            let catalogue = items.iter().enumerate().map(|(index, label)| json!({"id":index.to_string(), "label":safe(label), "detail":descriptions.get(index).and_then(Option::as_deref).map(safe)})).collect::<Vec<_>>();
            (
                surface,
                catalogue,
                order.clone(),
                order.get(*selected).copied(),
                Some(filter.as_str()),
            )
        }
        Panel::SessionPicker { picker } => {
            let order = session_picker_ordering(picker);
            let catalogue = picker.active_rows().iter().enumerate().map(|(index, session)| json!({"id":index.to_string(), "label":safe(session.name.as_deref().unwrap_or(&session.title)), "detail":format!("{} messages · {}",session.message_count,safe(&session.id))})).collect::<Vec<_>>();
            (
                &picker.surface,
                catalogue,
                order.clone(),
                order.get(picker.selected).copied(),
                Some(picker.filter.as_str()),
            )
        }
        Panel::MessagePicker { picker } => {
            let catalogue = picker.messages.iter().enumerate().map(|(index, message)| json!({"id":index.to_string(), "label":safe(&message.text), "node":"user"})).collect::<Vec<_>>();
            (
                &picker.surface,
                catalogue,
                (0..picker.messages.len()).collect(),
                Some(picker.selected).filter(|index| *index < picker.messages.len()),
                None,
            )
        }
        _ => return None,
    };
    Some(Node::new(id(shell), Kind::Picker, Props::new().role("octet.picker")
        .set("title", safe(&surface.title)).text("subtitle", surface.purpose.as_deref().map(safe).unwrap_or_default())
        .set("query", query.map(Value::from).unwrap_or(Value::Null)).set("placeholder", "Filter…")
        .set("total", items.len()).set("items", items).set("order", order.iter().map(usize::to_string).collect::<Vec<_>>())
        .set("selected", selected.map(|index| index.to_string())).set("layout", "rows").set("size", "md").set("preview", "none")
        .set("empty", "No matches").set("actions", json!([{"id":"confirm","label":"Select","primary":true},{"id":"cancel","label":"Close","end":true}]))))
}

pub(super) fn select(shell: &mut ShellState, item: &str) -> Option<()> {
    let raw: usize = item.parse().ok()?;
    let panel = shell.panel.as_mut()?;
    if !interactive(panel) {
        return None;
    }
    match panel {
        Panel::SelectList {
            items,
            descriptions,
            selected,
            filter,
            action,
            ..
        } => {
            *selected = filtered_indices_for_action(items, descriptions, action, filter)
                .iter()
                .position(|index| *index == raw)?;
        }
        Panel::SessionPicker { picker } => {
            picker.selected = session_picker_ordering(picker)
                .iter()
                .position(|index| *index == raw)?;
        }
        Panel::MessagePicker { picker } => {
            if raw >= picker.messages.len() {
                return None;
            }
            picker.selected = raw;
        }
        _ => return None,
    }
    Some(())
}

pub(super) fn filter(shell: &ShellState) -> Option<&str> {
    let panel = shell.panel.as_ref()?;
    if !interactive(panel) {
        return None;
    }
    match panel {
        Panel::SelectList { filter, .. } => Some(filter),
        Panel::SessionPicker { picker } => Some(&picker.filter),
        _ => None,
    }
}

pub(super) fn replace_filter(shell: &mut ShellState, text: String) {
    match shell.panel.as_mut().expect("validated native picker") {
        Panel::SelectList {
            filter, selected, ..
        } => {
            *filter = text;
            *selected = 0;
        }
        Panel::SessionPicker { picker } => {
            picker.filter = text;
            picker.selected = 0;
            picker.scroll = 0;
            picker.entry_search = None;
        }
        _ => unreachable!("validated native picker filter"),
    }
}
