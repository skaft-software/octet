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
            let catalogue = items
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    json!({"id": index.to_string(), "label": safe(label), "detail": descriptions.get(index).and_then(Option::as_deref).map(safe)})
                })
                .collect::<Vec<_>>();
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
            let catalogue = picker
                .active_rows()
                .iter()
                .enumerate()
                .map(|(index, session)| {
                    json!({"id": index.to_string(), "label": safe(session.name.as_deref().unwrap_or(&session.title)), "detail": format!("{} messages · {}", session.message_count, safe(&session.id))})
                })
                .collect::<Vec<_>>();
            (
                &picker.surface,
                catalogue,
                order.clone(),
                order.get(picker.selected).copied(),
                Some(picker.filter.as_str()),
            )
        }
        Panel::MessagePicker { picker } => {
            let catalogue = picker
                .messages
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    json!({"id": index.to_string(), "label": safe(&message.text), "node": "user"})
                })
                .collect::<Vec<_>>();
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
    // Highlight the program's own match ranges, so the query reads as hits in
    // the catalogue rather than as a separately filtered list.
    let mut hits = serde_json::Map::new();
    if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
        for index in &order {
            let Some(label) = items.get(*index).and_then(|item| item["label"].as_str()) else {
                continue;
            };
            let ranges = match_ranges(label, query);
            if !ranges.is_empty() {
                hits.insert(index.to_string(), Value::Array(ranges));
            }
        }
    }
    Some(Node::new(id(shell), Kind::Picker, Props::new().role("octet.picker")
        .set("title", safe(&surface.title)).text("subtitle", surface.purpose.as_deref().map(safe).unwrap_or_default())
        .set("query", query.map(Value::from).unwrap_or(Value::Null))
        .set("cursor", query.map(|query| query.encode_utf16().count()))
        .set("placeholder", "Filter…").set("noun", "results")
        .set("total", items.len()).set("items", items).set("order", order.iter().map(usize::to_string).collect::<Vec<_>>())
        .set("hits", Value::Object(hits))
        .set("selected", selected.map(|index| index.to_string())).set("layout", "rows").set("size", if order.len() > 10 { "lg" } else { "md" }).set("preview", "none")
        .set("focus", "list")
        .set("empty", "No matches").set("actions", json!([{"id":"confirm","label":"Select","keys":["enter"],"primary":true},{"id":"cancel","label":"Close","keys":["esc"],"end":true}]))))
}

/// Case-insensitive occurrences of `query` in `label` as UTF-16 `[from, to)`
/// ranges, which is the coordinate space TSP hit ranges use.
fn match_ranges(label: &str, query: &str) -> Vec<Value> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = needle.chars().collect();
    let chars: Vec<(usize, char)> = label.char_indices().collect();
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < chars.len() && ranges.len() < 64 {
        let mut matched = 0;
        let mut cursor = start;
        while matched < needle.len() && cursor < chars.len() {
            let character = chars[cursor]
                .1
                .to_lowercase()
                .next()
                .unwrap_or(chars[cursor].1);
            if character != needle[matched] {
                break;
            }
            matched += 1;
            cursor += 1;
        }
        if matched == needle.len() {
            let byte = |index: usize| chars.get(index).map_or(label.len(), |(offset, _)| *offset);
            ranges.push(json!([
                label[..byte(start)].encode_utf16().count(),
                label[..byte(cursor)].encode_utf16().count(),
            ]));
            start = cursor.max(start + 1);
        } else {
            start += 1;
        }
    }
    ranges
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
