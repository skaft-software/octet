//! Session facts and actions over the host-owned session catalogue.

use octet_tern::wire::{Kind, Node, Props, Span};
use serde_json::json;
use std::time::SystemTime;

use super::{sanitize_ordinary_surface_cell, PickerScope, PickerState, ShellState};

pub(super) fn decorate(
    shell: &ShellState,
    picker: &PickerState,
    props: Props,
    selected: Option<usize>,
) -> (Props, Vec<Node>) {
    let safe = |text: &str| sanitize_ordinary_surface_cell(text, shell.theme.unicode());
    let now = SystemTime::now();
    let current = |session: &crate::session_store::SessionMeta| {
        picker.current_session_path.as_ref() == Some(&session.path)
    };
    let catalogue = picker.active_rows().iter().enumerate().map(|(index, session)| {
        let mut badges = Vec::new();
        if current(session) { badges.push(json!({"text":"current","tone":"accent"})); }
        if session.forked_from_session_id.is_some() { badges.push(json!({"text":"fork"})); }
        if session.pinned { badges.push(json!({"text":"pinned"})); }
        let detail = if picker.show_path {
            safe(&session.path.display().to_string())
        } else {
            session.workspace.as_ref().map(|path| safe(&path.display().to_string())).unwrap_or_default()
        };
        json!({"id":index.to_string(),"label":safe(session.name.as_deref().unwrap_or(&session.title)),
            "detail":detail,"badges":badges,"facts":{"messages":session.message_count,
                "when":super::panel_render::session_age(session.modified, now)}})
    }).collect::<Vec<_>>();
    let selected_session = selected.and_then(|index| picker.active_rows().get(index));
    let preview = selected_session.map(|session| {
        let mut facts = vec![
            json!({"k":"Session","v":[{"t":safe(&session.id),"s":"dim mono"}]}),
            json!({"k":"Messages","v":session.message_count.to_string()}),
            json!({"k":"Updated","v":super::panel_render::session_age(session.modified, now)}),
            json!({"k":"File","v":[{"t":safe(&session.path.display().to_string()),"s":"path mono"}]}),
        ];
        if let Some(workspace) = &session.workspace {
            facts.push(json!({"k":"Workspace","v":[{"t":safe(&workspace.display().to_string()),"s":"path mono"}]}));
        }
        if let Some(parent) = &session.forked_from_session_id {
            facts.push(json!({"k":"Forked from","v":safe(parent)}));
        }
        if !session.tags.is_empty() {
            facts.push(json!({"k":"Tags","v":safe(&session.tags.join(", "))}));
        }
        vec![
            Node::new("panel.session.title", Kind::Text, Props::new().text("spans", vec![Span::styled(safe(session.name.as_deref().unwrap_or(&session.title)), "strong")])),
            Node::new("panel.session.prompt", Kind::Text, Props::new().set("text", safe(&session.title)).set("wrap", "word").set("hidden", session.name.is_none())),
            Node::new("panel.session.facts", Kind::Kv, Props::new().set("items", facts)),
        ]
    }).unwrap_or_default();
    let scope = if picker.scope == PickerScope::Current {
        "This workspace"
    } else {
        "All workspaces"
    };
    let props = props.set("title", "Sessions").set("subtitle", scope).set("icon", "session")
        .set("subtitle", format!("{scope} · {}", picker.sort.label())).set("noun", "sessions").set("placeholder", "Search sessions…")
        .set("items", catalogue).set("size", "lg").set("preview", if shell.size.0 < 80 {"below"} else {"side"})
        .set("current", picker.active_rows().iter().enumerate().filter(|(_, session)| current(session)).map(|(index, _)| index.to_string()).collect::<Vec<_>>())
        .set("columns", json!([{"id":"messages","head":"Messages","format":"num","priority":2},{"id":"when","head":"Updated","format":"time","priority":1}]))
        .set("strip", json!({"items":[
            {"id":"workspace","label":scope,"on":picker.scope == PickerScope::All},
            {"id":"sort","label":format!("Sort: {}", picker.sort.label())},
            {"id":"named","label":"Named only","on":picker.named_only},
            {"id":"paths","label":"Paths","on":picker.show_path}
        ]}))
        .set("actions", json!([
            {"id":"confirm","label":"Resume","keys":["enter"],"primary":true,"disabled":selected_session.is_none()},
            {"id":"search","label":"Search text","keys":["ctrl+f"],"disabled":picker.filter.trim().is_empty()},
            {"id":"rename","label":"Rename","keys":["ctrl+r"],"disabled":selected_session.is_none()},
            {"id":"delete","label":"Delete","keys":["ctrl+x"],"danger":true,"disabled":selected_session.is_none() || selected_session.is_some_and(current)},
            {"id":"cancel","label":"Close","keys":["esc"],"end":true}
        ]));
    (props, preview)
}
