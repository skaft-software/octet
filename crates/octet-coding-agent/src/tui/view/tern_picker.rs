//! Data-first native pickers over the same host catalogues and filtering policy.

use octet_tern::wire::{Kind, Node, Props, Span};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    filtered_indices_for_action, sanitize_ordinary_surface_cell, session_picker_ordering,
    tern_controls, Panel, PanelAction, ShellState,
};

#[path = "tern_picker_sheet.rs"]
mod sheet;
use sheet::node as list_sheet;

#[path = "tern_session_edit.rs"]
pub(crate) mod session_edit;

pub(super) fn id(shell: &ShellState) -> String {
    // Catalogue refreshes retain the host request, but must fence ordinal
    // gestures so an old reply cannot select a different worker or session.
    let mut hash = Sha256::new();
    let mut field = |value: &str| {
        hash.update(value.len().to_le_bytes());
        hash.update(value.as_bytes());
    };
    match shell.panel.as_ref() {
        Some(Panel::SelectList {
            items,
            descriptions,
            action,
            ..
        }) => {
            for (index, item) in items.iter().enumerate() {
                field(item);
                field(
                    descriptions
                        .get(index)
                        .and_then(Option::as_deref)
                        .unwrap_or_default(),
                );
                if let Some(panel) = action.subagent_panel() {
                    field(&panel.node_ids[index]);
                }
            }
        }
        Some(Panel::SessionPicker { picker }) => {
            field(if picker.rename.is_some() {
                "rename"
            } else if picker.confirming_delete {
                "trash"
            } else {
                "browse"
            });
            for row in picker.active_rows() {
                field(&row.id);
                field(&row.path.to_string_lossy());
                field(row.name.as_deref().unwrap_or(&row.title));
            }
            if picker.rename.is_some() || picker.confirming_delete {
                if let Some(index) = session_picker_ordering(picker).get(picker.selected) {
                    field(&picker.active_rows()[*index].id);
                }
            }
        }
        Some(Panel::MessagePicker { picker }) => {
            for message in &picker.messages {
                field(&message.entry_id);
            }
        }
        _ => return format!("panel.{}", shell.panel_epoch),
    }
    format!("panel.{}.{:x}", shell.panel_epoch, hash.finalize())
}

pub(super) fn interactive(panel: &Panel) -> bool {
    match panel {
        Panel::SelectList { action, .. } => !matches!(action, PanelAction::Confirmation),
        Panel::SessionPicker { picker } => picker.rename.is_none() && !picker.confirming_delete,
        Panel::MessagePicker { .. } => true,
        Panel::ReadOnlyDocument { .. } => false,
    }
}

/// The rename editor owns focus; browse lists keep their catalogue identity.
pub(super) fn focus(shell: &ShellState) -> Option<String> {
    if matches!(&shell.panel, Some(Panel::SessionPicker { picker }) if picker.rename.is_some()) {
        session_edit::focus(shell)
    } else {
        Some(id(shell))
    }
}

pub(super) fn node(shell: &ShellState) -> Option<Node> {
    let panel = shell.panel.as_ref()?;
    if matches!(panel, Panel::SessionPicker { picker } if picker.rename.is_some()) {
        return session_edit::node(shell);
    }
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
            vec![
                Node::new(
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
                ),
                tern_controls::action(shell, &id(shell), "cancel", "Close", "tui.select.cancel"),
            ],
        ));
    }
    if !interactive(panel) {
        return None;
    }
    if let Panel::SelectList {
        surface,
        items,
        descriptions,
        selected,
        filter,
        action,
        ..
    } = panel
    {
        if !matches!(
            action,
            PanelAction::SelectGroupedModel { .. } | PanelAction::SelectThinking(_)
        ) {
            return Some(list_sheet(
                shell,
                surface,
                items,
                descriptions,
                *selected,
                filter,
                action,
            ));
        }
    }
    if let Panel::SelectList {
        action: action @ PanelAction::SelectThinking(levels),
        selected,
        filter,
        items,
        descriptions,
        ..
    } = panel
    {
        let order = filtered_indices_for_action(items, descriptions, action, filter);
        return Some(thinking_node(
            shell,
            levels,
            &order,
            order.get(*selected).copied(),
        ));
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
    let (state, message) = surface.lifecycle.native_state(std::time::Instant::now());
    let props = Props::new()
        .role("octet.picker")
        .set("state", state)
        .set("message", message.map(safe))
        .set("title", safe(&surface.title))
        .text(
            "subtitle",
            surface.purpose.as_deref().map(safe).unwrap_or_default(),
        )
        .set("query", query.map(Value::from).unwrap_or(Value::Null))
        .set("cursor", query.map(|query| query.encode_utf16().count()))
        .set("placeholder", "Filter…")
        .set("noun", "results")
        .set("total", items.len())
        .set("items", items)
        .set(
            "order",
            order.iter().map(usize::to_string).collect::<Vec<_>>(),
        )
        .set("hits", Value::Object(hits))
        .set("selected", selected.map(|index| index.to_string()))
        .set("layout", "rows")
        .set("size", if order.len() > 10 { "lg" } else { "md" })
        .set("preview", "none")
        .set("focus", "list")
        .set("empty", "No matches")
        .set(
            "actions",
            picker_actions(shell, "Select", selected.is_some()),
        );
    let (props, preview) = decorate(shell, panel, props, &order, selected);
    Some(Node::with_children(id(shell), Kind::Picker, props, preview))
}

fn picker_actions(shell: &ShellState, confirm_label: &str, available: bool) -> Vec<Value> {
    let mut confirm = tern_controls::picker_action(
        shell,
        "confirm",
        confirm_label,
        "tui.select.confirm",
        available,
    );
    confirm["primary"] = json!(true);
    let mut cancel =
        tern_controls::picker_action(shell, "cancel", "Close", "tui.select.cancel", true);
    cancel["end"] = json!(true);
    vec![confirm, cancel]
}

/// Presentation enrichments keep selection indices in the host's catalogue.
fn decorate(
    shell: &ShellState,
    panel: &Panel,
    mut props: Props,
    order: &[usize],
    selected: Option<usize>,
) -> (Props, Vec<Node>) {
    let safe = |value: &str| sanitize_ordinary_surface_cell(value, shell.theme.unicode());
    if let Panel::SessionPicker { picker } = panel {
        return super::tern_sessions::decorate(shell, picker, props, selected);
    }
    let Panel::SelectList {
        action,
        items,
        descriptions,
        filter,
        ..
    } = panel
    else {
        return (props, Vec::new());
    };
    match action {
        PanelAction::SelectGroupedModel {
            models,
            providers,
            details,
            scope,
        } => {
            let catalogue = models.iter().enumerate().map(|(index, model)| {
                let detail = details.get(index);
                let label = detail.map(|detail| detail.name.as_str()).unwrap_or_else(|| items[index].strip_suffix(" (current)").unwrap_or(&items[index]));
                json!({"id":index.to_string(), "label":safe(label), "detail":[{"t":safe(&model.0),"s":"dim mono"}],
                    "facts":{"ctx":detail.map(|detail| detail.context),"price":detail.map(|detail| safe(&detail.price))},
                    "badges":[]})
            }).collect::<Vec<_>>();
            let all_matches = super::panel_render::searched_indices_for_action(
                items,
                descriptions,
                action,
                filter,
            );
            let mut scopes = vec![
                json!({"id":"all","label":"All models","icon":"list","count":all_matches.len()}),
            ];
            let mut active_scope = "all".to_owned();
            for (index, provider) in providers.iter().enumerate() {
                if providers[..index].contains(provider) {
                    continue;
                }
                let scope_id = format!("provider.{index}");
                if scope.as_ref() == Some(provider) {
                    active_scope = scope_id.clone();
                }
                // TSP's provider-mark slot supports seeded text, not image blobs.
                let initials = provider.chars().take(2).collect::<String>().to_uppercase();
                scopes.push(json!({"id":scope_id,"label":safe(provider),"mark":{"text":safe(&initials),"seed":provider.to_lowercase()},"group":"Providers",
                    "count":all_matches.iter().filter(|index| &providers[**index] == provider).count()}));
            }
            let mut grouped_order = Vec::new();
            let mut last = None;
            for index in order {
                let provider = &providers[*index];
                if last != Some(provider) {
                    grouped_order.push(json!({"group":format!("provider.{}", providers.iter().position(|item| item == provider).unwrap()),"label":safe(provider),"count":order.iter().filter(|index| &providers[**index] == provider).count()}));
                    last = Some(provider);
                }
                grouped_order.push(Value::String(index.to_string()));
            }
            let current = models
                .iter()
                .position(|model| model.0 == shell.model)
                .map(|index| index.to_string())
                .into_iter()
                .collect::<Vec<_>>();
            props = props.set("title", "Models").set("subtitle", "").set("icon", "model").set("noun", "models").set("placeholder", "Search models…")
                .set("items", catalogue).set("order", grouped_order).set("scopes", scopes).set("scope", active_scope)
                .set("current", current).set("size", "lg").set("preview", "side")
                .set("columns", json!([{"id":"ctx","head":"Ctx","format":"num","priority":3},{"id":"price","head":"$/M in · out","format":"price","priority":2}]))
                .set("actions", picker_actions(shell, "Switch", selected.is_some()));
            let preview = selected
                .and_then(|index| {
                    details.get(index).map(|detail| {
                        model_preview(shell, index, &models[index].0, &providers[index], detail)
                    })
                })
                .unwrap_or_default();
            (props, preview)
        }
        _ => (props, Vec::new()),
    }
}

fn model_preview(
    shell: &ShellState,
    index: usize,
    model: &str,
    provider: &str,
    detail: &crate::tui::pickers::ModelPickerDetail,
) -> Vec<Node> {
    let safe = |value: &str| sanitize_ordinary_surface_cell(value, shell.theme.unicode());
    let mut badges = detail
        .badges
        .iter()
        .map(|badge| safe(badge))
        .collect::<Vec<_>>();
    if model == shell.model {
        badges.insert(0, "current".into());
    }
    let mut facts = vec![
        ("Provider", safe(provider)),
        ("Context", count_label(detail.context)),
        ("Max output", count_label(detail.output)),
        ("Price / M in · out", safe(&detail.price)),
        ("Input", safe(&detail.input)),
    ];
    if let Some(price) = &detail.cache_price {
        facts.push(("Cache read / M", safe(price)));
    }
    for (key, value) in &detail.source {
        facts.push((
            match key.as_str() {
                "knowledge" => "Knowledge cutoff",
                "release_date" => "Released",
                "last_updated" => "Updated",
                "open_weights" => "Open weights",
                _ => continue,
            },
            safe(value),
        ));
    }
    vec![
        Node::new("panel.preview.name", Kind::Text, Props::new().text("spans", vec![Span::styled(safe(&detail.name), "strong")])),
        Node::new("panel.preview.id", Kind::Text, Props::new().text("spans", vec![Span::styled(safe(model), "dim mono")]).set("wrap", "word")),
        Node::with_children("panel.preview.badges", Kind::Row, Props::new().set("gap", "xs").set("wrap", true), badges.into_iter().enumerate().map(|(badge_index, badge)| Node::new(format!("panel.preview.{index}.badge{badge_index}"), Kind::Badge, Props::new().set("text", &badge).set("tone", if badge == "current" { "success" } else { "muted" }))).collect()),
        Node::new("panel.preview.facts", Kind::Kv, Props::new().set("items", facts.into_iter().map(|(key, value)| json!({"k":[{"t":key,"s":"muted"}],"v":[{"t":value,"s":"mono"}]})).collect::<Vec<_>>())),
    ]
}

fn thinking_node(
    shell: &ShellState,
    levels: &[crate::config::ThinkingLevel],
    order: &[usize],
    selected: Option<usize>,
) -> Node {
    let panel = id(shell);
    let item_id = |index| format!("{panel}.item.{index}");
    let items = order
        .iter()
        .map(|index| {
            let level = levels[*index];
            let description = thinking_detail(level);
            Node::new(
                item_id(*index),
                Kind::Item,
                Props::new()
                    .text(
                        "label",
                        vec![
                            Span::styled(
                                "● ",
                                if level == crate::config::ThinkingLevel::Off {
                                    "dim"
                                } else {
                                    "accent"
                                },
                            ),
                            Span::styled(level.label(), "mono"),
                        ],
                    )
                    .text("detail", description)
                    .text(
                        "value",
                        vec![Span::styled(
                            if level.label() == shell.reasoning {
                                shell.theme.glyph("success")
                            } else {
                                ""
                            },
                            "success",
                        )],
                    ),
            )
        })
        .collect();
    Node::with_children(
        format!("{panel}.sheet"),
        Kind::Overlay,
        Props::new()
            .role("omp.overlay.thinking")
            .set("modal", true)
            .set("size", "sm")
            .text("head", "Thinking"),
        vec![
            Node::new(
                format!("{panel}.model"),
                Kind::Text,
                Props::new().text(
                    "spans",
                    vec![Span::styled(
                        sanitize_ordinary_surface_cell(
                            crate::presentation::model::footer_model_name(
                                &shell.model_display,
                                &shell.model,
                            ),
                            shell.theme.unicode(),
                        ),
                        "muted",
                    )],
                ),
            ),
            Node::with_children(
                &panel,
                Kind::List,
                Props::new()
                    .set("selected", selected.map(item_id))
                    .set("max", json!({"lines":9})),
                items,
            ),
            Node::with_children(
                format!("{panel}.actions"),
                Kind::Row,
                Props::new().set("gap", "sm").set("align", "center"),
                vec![
                    Node::new(
                        format!("{panel}.hint"),
                        Kind::Text,
                        Props::new().text(
                            "spans",
                            vec![Span::styled(tern_controls::navigation(shell), "dim")],
                        ),
                    ),
                    Node::new(
                        format!("{panel}.gap"),
                        Kind::Row,
                        Props::new().set("grow", 1),
                    ),
                    tern_controls::action(shell, &panel, "cancel", "Close", "tui.select.cancel"),
                    tern_controls::action(shell, &panel, "confirm", "Apply", "tui.select.confirm"),
                ],
            ),
        ],
    )
}

/// Only exact controls of the current catalogue and host request can act.
pub(super) fn owns_action(shell: &ShellState, node: &str) -> bool {
    // Rename has exact, source-revision-bound controls, never browse controls.
    if matches!(&shell.panel, Some(Panel::SessionPicker { picker }) if picker.rename.is_some()) {
        return false;
    }
    let panel = id(shell);
    node == panel
        || ["sheet", "confirm", "cancel", "up", "down"]
            .iter()
            .any(|suffix| node == format!("{panel}.{suffix}"))
}

fn count_label(value: u64) -> String {
    value
        .to_string()
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).expect("decimal digits"))
        .collect::<Vec<_>>()
        .join(",")
}

fn thinking_detail(level: crate::config::ThinkingLevel) -> &'static str {
    use crate::config::ThinkingLevel::*;
    match level {
        Off => "No reasoning",
        On => "Reasoning enabled",
        Minimal => "Very brief reasoning",
        Low => "Light reasoning",
        Medium => "Balanced depth and latency",
        High => "Deep reasoning",
        Xhigh => "Extended reasoning",
        Max => "Maximum reasoning",
        Ultra => "Maximum reasoning with subagents",
    }
}

/// Provider navigation uses exactly the same index projection as keyboard input.
pub(super) fn scope(shell: &mut ShellState, value: &str) -> Option<()> {
    let Panel::SelectList {
        action: PanelAction::SelectGroupedModel {
            providers, scope, ..
        },
        selected,
        ..
    } = shell.panel.as_mut()?
    else {
        return None;
    };
    let next = if value == "all" {
        None
    } else {
        let index = value.strip_prefix("provider.")?.parse::<usize>().ok()?;
        if value != format!("provider.{index}") {
            return None;
        }
        let provider = providers.get(index)?;
        if providers[..index].contains(provider) {
            return None;
        }
        Some(provider.clone())
    };
    if *scope != next {
        *scope = next;
        *selected = 0;
        shell.panel_epoch = shell.panel_epoch.wrapping_add(1);
        shell.painted_panel = None;
    }
    Some(())
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
    let raw: usize = if matches!(
        &shell.panel,
        Some(Panel::SelectList { action, .. }) if !matches!(action, PanelAction::SelectGroupedModel { .. })
    ) {
        item.strip_prefix(&format!("{}.item.", id(shell)))?
            .parse()
            .ok()?
    } else {
        item.parse().ok()?
    };
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
    if matches!(
        shell.panel,
        Some(Panel::SelectList {
            action: PanelAction::Confirmation,
            ..
        })
    ) {
        return None;
    }
    match panel {
        Panel::SelectList {
            action: PanelAction::SelectThinking(_),
            ..
        } => None,
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
