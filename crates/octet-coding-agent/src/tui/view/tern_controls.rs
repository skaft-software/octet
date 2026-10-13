//! Native hints use the same resolved bindings as frontend gesture routing.

use std::sync::OnceLock;

use octet_tern::wire::{Kind, Node, Props};
use serde_json::{json, Value};

use super::ShellState;
use crate::tui::keymap::keybindings::KeybindingsManager;

pub(super) fn key<'a>(shell: &'a ShellState, binding: &str) -> Option<&'a str> {
    // Production publishes the actual frontend map. Missing maps occur only in
    // deterministic projections/tests; never read the user's configuration here.
    static DEFAULTS: OnceLock<KeybindingsManager> = OnceLock::new();
    shell
        .native_keys
        .as_ref()
        .unwrap_or_else(|| DEFAULTS.get_or_init(KeybindingsManager::current_platform))
        .get_keys(binding)
        .iter()
        .find(|key| super::tern_input::key_event(key).is_some())
        .map(String::as_str)
}

pub(super) fn label(shell: &ShellState, binding: &str) -> String {
    key(shell, binding).map_or_else(
        || "unavailable".to_owned(),
        |key| {
            key.split('+')
                .map(|part| match part {
                    "enter" => "Enter".to_owned(),
                    "escape" | "esc" => "Esc".to_owned(),
                    "ctrl" => "Ctrl".to_owned(),
                    "alt" => "Alt".to_owned(),
                    "shift" => "Shift".to_owned(),
                    "super" | "meta" => "Super".to_owned(),
                    other => other.to_uppercase(),
                })
                .collect::<Vec<_>>()
                .join("+")
        },
    )
}

pub(super) fn hint(shell: &ShellState, binding: &str, verb: &str) -> String {
    if key(shell, binding).is_some() {
        format!("{} {verb}", label(shell, binding))
    } else {
        format!("{verb} unavailable")
    }
}

pub(super) fn navigation(shell: &ShellState) -> String {
    format!(
        "{} · {}",
        hint(shell, "tui.select.up", "previous"),
        hint(shell, "tui.select.down", "next")
    )
}

pub(super) fn action(
    shell: &ShellState,
    panel: &str,
    name: &str,
    title: &str,
    binding: &str,
) -> Node {
    let key = key(shell, binding);
    let title = if key.is_some() {
        title.to_owned()
    } else {
        format!("{title} unavailable")
    };
    let mut props = Props::new()
        .set("gap", "xs")
        .set("align", "center")
        .set("title", &title);
    if key.is_some() {
        props = props.set("actions", json!({"click":name}));
    }
    let mut children = vec![Node::new(
        format!("{panel}.{name}.label"),
        Kind::Text,
        Props::new().set("text", &title),
    )];
    if let Some(key) = key {
        children.push(Node::new(
            format!("{panel}.{name}.key"),
            Kind::Kbd,
            Props::new().set("keys", [key]).set("title", &title),
        ));
    }
    Node::with_children(format!("{panel}.{name}"), Kind::Row, props, children)
}

pub(super) fn picker_action(
    shell: &ShellState,
    id: &str,
    title: &str,
    binding: &str,
    available: bool,
) -> Value {
    let mut action = json!({"id":id,"label":title});
    if let Some(key) = key(shell, binding) {
        action["keys"] = json!([key]);
        if !available {
            action["disabled"] = json!(true);
        }
    } else {
        // Only picker action declarations support disabled. Common node props
        // do not: ordinary rows instead omit their pointer action entirely.
        action["label"] = json!(format!("{title} unavailable"));
        action["disabled"] = json!("No keybinding");
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn remapped_confirm_is_the_hint_and_pointer_control_key() {
        let shell = ShellState {
            native_keys: Some(KeybindingsManager::with_platform(
                "linux",
                false,
                BTreeMap::from([("tui.select.confirm".into(), vec!["ctrl+y".into()])]),
            )),
            ..ShellState::default()
        };
        assert_eq!(
            hint(&shell, "tui.select.confirm", "submit"),
            "Ctrl+Y submit"
        );
        let node = action(&shell, "panel.1", "confirm", "Select", "tui.select.confirm");
        assert_eq!(
            node.p.as_ref().unwrap().as_map()["actions"]["click"],
            "confirm"
        );
        assert_eq!(
            node.c.as_ref().unwrap()[1].p.as_ref().unwrap().as_map()["keys"],
            json!(["ctrl+y"])
        );
        assert_eq!(
            picker_action(&shell, "confirm", "Select", "tui.select.confirm", true)["keys"],
            json!(["ctrl+y"])
        );
    }

    #[test]
    fn unbound_cancel_and_session_actions_are_unavailable_not_clickable() {
        let shell = ShellState {
            native_keys: Some(KeybindingsManager::with_platform(
                "linux",
                false,
                BTreeMap::from([
                    ("tui.select.cancel".into(), Vec::new()),
                    ("app.session.rename".into(), Vec::new()),
                ]),
            )),
            ..ShellState::default()
        };
        let node = action(&shell, "panel.1", "cancel", "Close", "tui.select.cancel");
        let props = node.p.as_ref().unwrap().as_map();
        assert_eq!(props["title"], "Close unavailable");
        assert!(!props.contains_key("actions"));
        assert!(!props.contains_key("disabled"));
        assert_eq!(node.c.as_ref().unwrap().len(), 1);
        for (id, binding) in [
            ("cancel", "tui.select.cancel"),
            ("rename", "app.session.rename"),
        ] {
            let action = picker_action(&shell, id, "Action", binding, true);
            assert_eq!(action["disabled"], "No keybinding");
            assert!(action.get("keys").is_none());
            assert_eq!(action["label"], "Action unavailable");
        }
        assert_eq!(
            picker_action(&shell, "delete", "Delete", "app.session.delete", false)["disabled"],
            true
        );
    }
}
