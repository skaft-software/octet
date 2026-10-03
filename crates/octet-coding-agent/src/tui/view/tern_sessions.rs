//! Session facts and actions over the host-owned session catalogue.

use octet_tern::wire::{Kind, Node, Props, Span};
use serde_json::json;
use std::time::SystemTime;

use super::{sanitize_ordinary_surface_cell, tern_controls, PickerScope, PickerState, ShellState};

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
    let mut preview = selected_session.map(|session| {
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
    let mut strip = Vec::new();
    let mut unavailable = Vec::new();
    for (id, title, binding, on) in [
        (
            "workspace",
            scope.to_owned(),
            "tui.input.tab",
            picker.scope == PickerScope::All,
        ),
        (
            "sort",
            format!("Sort: {}", picker.sort.label()),
            "app.session.toggleSort",
            false,
        ),
        (
            "named",
            "Named only".to_owned(),
            "app.session.toggleNamedFilter",
            picker.named_only,
        ),
        (
            "paths",
            "Paths".to_owned(),
            "app.session.togglePath",
            picker.show_path,
        ),
    ] {
        if tern_controls::key(shell, binding).is_some() {
            strip.push(json!({"id":id,"label":format!("{title} · {}", tern_controls::label(shell, binding)),"on":on}));
        } else {
            // Strip items have no disabled prop. Keep unavailable controls as
            // plain text, not native strip items that can emit a click action.
            unavailable.push(format!("{title} unavailable"));
        }
    }
    if !unavailable.is_empty() {
        preview.push(Node::new(
            "panel.session.unavailable",
            Kind::Text,
            Props::new()
                .set("text", unavailable.join(" · "))
                .set("wrap", "word"),
        ));
    }
    let mut confirm = tern_controls::picker_action(
        shell,
        "confirm",
        "Resume",
        "tui.select.confirm",
        selected_session.is_some(),
    );
    confirm["primary"] = json!(true);
    let search = tern_controls::picker_action(
        shell,
        "search",
        "Search text",
        "app.session.search",
        !picker.filter.trim().is_empty(),
    );
    let rename = tern_controls::picker_action(
        shell,
        "rename",
        "Rename",
        "app.session.rename",
        selected_session.is_some(),
    );
    let mut delete = tern_controls::picker_action(
        shell,
        "delete",
        "Delete",
        "app.session.delete",
        selected_session.is_some() && !selected_session.is_some_and(current),
    );
    delete["danger"] = json!(true);
    let mut cancel =
        tern_controls::picker_action(shell, "cancel", "Close", "tui.select.cancel", true);
    cancel["end"] = json!(true);
    let props = props.set("title", "Sessions").set("subtitle", scope).set("icon", "session")
        .set("subtitle", format!("{scope} · {}", picker.sort.label())).set("noun", "sessions").set("placeholder", "Search sessions…")
        .set("items", catalogue).set("size", "lg").set("preview", if shell.size.0 < 80 {"below"} else {"side"})
        .set("current", picker.active_rows().iter().enumerate().filter(|(_, session)| current(session)).map(|(index, _)| index.to_string()).collect::<Vec<_>>())
        .set("columns", json!([{"id":"messages","head":"Messages","format":"num","priority":2},{"id":"when","head":"Updated","format":"time","priority":1}]))
        .set("strip", json!({"items":strip}))
        .set("actions", vec![confirm, search, rename, delete, cancel]);
    (props, preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::keybindings::KeybindingsManager;
    use std::collections::BTreeMap;

    #[test]
    fn session_footer_resolves_keys_and_strip_omits_unbound_controls() {
        let mut shell = ShellState::default();
        shell.native_keys = Some(KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([
                ("tui.select.confirm".into(), vec!["ctrl+y".into()]),
                ("tui.select.cancel".into(), Vec::new()),
                ("app.session.rename".into(), Vec::new()),
                ("app.session.toggleSort".into(), Vec::new()),
            ]),
        ));
        let picker = PickerState::new(Vec::new(), None);
        let (props, preview) = decorate(&shell, &picker, Props::new(), None);
        let props = props.as_map();
        let actions = props["actions"].as_array().unwrap();
        assert_eq!(actions[0]["keys"], json!(["ctrl+y"]));
        assert_eq!(actions[0]["disabled"], true);
        assert_eq!(actions[2]["label"], "Rename unavailable");
        assert_eq!(actions[2]["disabled"], "No keybinding");
        assert!(actions[2].get("keys").is_none());
        assert_eq!(actions[4]["label"], "Close unavailable");
        assert!(props["strip"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != "sort"));
        assert!(serde_json::to_string(&preview)
            .unwrap()
            .contains("Sort: Recent unavailable"));
    }
}
