//! Host-rendered extension options menus (`menu/collect`).
//!
//! An extension that declares `contributes.menu` answers `menu/collect` with
//! the complete options menu the host shows when it is selected under
//! `/extensions`. Every action routes to one of that extension's own
//! manifest-declared commands, so a menu can only run what the extension could
//! already run; the host adds the confirmation boundary for destructive
//! actions. Menus are pulled each time they are shown, never pushed, so they
//! cannot go stale or displace semantic presentation state.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::extension_presentation::ExtensionPresentationStatus;

/// Maximum items across every level of one menu.
pub const MAX_EXTENSION_MENU_ITEMS: usize = 256;
/// Maximum nesting depth, counting the top level as one.
pub const MAX_EXTENSION_MENU_DEPTH: usize = 4;
/// Maximum UTF-8 bytes in an item id.
pub const MAX_EXTENSION_MENU_ID_BYTES: usize = 256;
/// Maximum UTF-8 bytes in a title or item label.
pub const MAX_EXTENSION_MENU_LABEL_BYTES: usize = 256;
/// Maximum UTF-8 bytes in an item description.
pub const MAX_EXTENSION_MENU_DESCRIPTION_BYTES: usize = 2 * 1_024;
/// Maximum UTF-8 bytes in a menu or submenu detail.
pub const MAX_EXTENSION_MENU_DETAIL_BYTES: usize = 16 * 1_024;
/// Maximum literal arguments routed by one action.
pub const MAX_EXTENSION_MENU_ARGUMENTS: usize = 32;
/// Maximum UTF-8 bytes in one routed argument.
pub const MAX_EXTENSION_MENU_ARGUMENT_BYTES: usize = 4 * 1_024;
/// Maximum encoded size of one complete menu.
pub const MAX_EXTENSION_MENU_BYTES: usize = 256 * 1_024;

/// The complete options menu an extension offers under `/extensions`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMenu {
    /// Display title; the host falls back to the extension name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Compact readiness shown above the items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ExtensionPresentationStatus>,
    /// Optional longer plain-text explanation shown with the status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Top-level items in display order.
    #[serde(default)]
    pub items: Vec<ExtensionMenuItem>,
}

/// One menu entry: an action routed to a declared command, or a submenu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMenuItem {
    /// Identifier, unique among its siblings, that keeps selection stable
    /// across refreshes.
    pub id: String,
    /// Host-rendered label.
    pub label: String,
    /// Optional one-line explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Manifest-declared command this action runs. Absent for a submenu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Literal arguments passed through normal command validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    /// Ask for host confirmation before running the action.
    #[serde(default, skip_serializing_if = "is_false")]
    pub destructive: bool,
    /// Preselected and emphasized; at most one per level.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recommended: bool,
    /// Submenu entries. Mutually exclusive with `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ExtensionMenuItem>>,
    /// Optional plain-text detail shown when a submenu opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ExtensionMenu {
    /// Validates bounds and that every action routes to a declared command.
    pub fn validate(&self, declared_commands: &[String]) -> Result<(), String> {
        let encoded = serde_json::to_vec(self)
            .map_err(|error| format!("extension menu cannot be encoded: {error}"))?;
        if encoded.len() > MAX_EXTENSION_MENU_BYTES {
            return Err(format!(
                "extension menu exceeds {MAX_EXTENSION_MENU_BYTES} encoded bytes"
            ));
        }
        if let Some(title) = &self.title {
            bounded("menu title", title, MAX_EXTENSION_MENU_LABEL_BYTES, true)?;
        }
        if let Some(status) = &self.status {
            bounded(
                "menu status label",
                &status.label,
                MAX_EXTENSION_MENU_LABEL_BYTES,
                true,
            )?;
            if let Some(detail) = &status.detail {
                bounded(
                    "menu status detail",
                    detail,
                    MAX_EXTENSION_MENU_DETAIL_BYTES,
                    false,
                )?;
            }
        }
        if let Some(detail) = &self.detail {
            bounded(
                "menu detail",
                detail,
                MAX_EXTENSION_MENU_DETAIL_BYTES,
                false,
            )?;
        }
        let mut count = 0;
        validate_items(&self.items, declared_commands, 1, &mut count)
    }
}

fn validate_items(
    items: &[ExtensionMenuItem],
    declared_commands: &[String],
    depth: usize,
    count: &mut usize,
) -> Result<(), String> {
    if depth > MAX_EXTENSION_MENU_DEPTH {
        return Err(format!(
            "extension menu nests deeper than {MAX_EXTENSION_MENU_DEPTH} levels"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut recommended = 0;
    for item in items {
        *count += 1;
        if *count > MAX_EXTENSION_MENU_ITEMS {
            return Err(format!(
                "extension menu has more than {MAX_EXTENSION_MENU_ITEMS} items"
            ));
        }
        bounded("menu item id", &item.id, MAX_EXTENSION_MENU_ID_BYTES, true)?;
        if !ids.insert(item.id.as_str()) {
            return Err(format!("duplicate menu item id {:?}", item.id));
        }
        bounded(
            "menu item label",
            &item.label,
            MAX_EXTENSION_MENU_LABEL_BYTES,
            true,
        )?;
        if let Some(description) = &item.description {
            bounded(
                "menu item description",
                description,
                MAX_EXTENSION_MENU_DESCRIPTION_BYTES,
                false,
            )?;
        }
        if let Some(detail) = &item.detail {
            bounded(
                "menu item detail",
                detail,
                MAX_EXTENSION_MENU_DETAIL_BYTES,
                false,
            )?;
        }
        if item.recommended {
            recommended += 1;
            if recommended > 1 {
                return Err("a menu level may recommend at most one item".into());
            }
        }
        match (&item.command, &item.items) {
            (Some(command), None) => {
                if !declared_commands.iter().any(|declared| declared == command) {
                    return Err(format!(
                        "menu item {:?} routes to undeclared command {command:?}",
                        item.id
                    ));
                }
                if item.arguments.len() > MAX_EXTENSION_MENU_ARGUMENTS {
                    return Err(format!(
                        "menu item {:?} has more than {MAX_EXTENSION_MENU_ARGUMENTS} arguments",
                        item.id
                    ));
                }
                for argument in &item.arguments {
                    bounded(
                        "menu item argument",
                        argument,
                        MAX_EXTENSION_MENU_ARGUMENT_BYTES,
                        false,
                    )?;
                }
            }
            (None, Some(children)) => {
                if !item.arguments.is_empty() || item.destructive {
                    return Err(format!(
                        "submenu {:?} cannot carry arguments or be destructive",
                        item.id
                    ));
                }
                validate_items(children, declared_commands, depth + 1, count)?;
            }
            _ => {
                return Err(format!(
                    "menu item {:?} must either route to a command or open a submenu",
                    item.id
                ));
            }
        }
    }
    Ok(())
}

fn bounded(kind: &str, value: &str, limit: usize, required: bool) -> Result<(), String> {
    if required && value.trim().is_empty() {
        return Err(format!("{kind} must not be empty"));
    }
    if value.len() > limit {
        return Err(format!("{kind} exceeds {limit} bytes"));
    }
    if value.contains('\0') {
        return Err(format!("{kind} contains a NUL byte"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_presentation::ExtensionPresentationState;

    fn action(id: &str, command: &str) -> ExtensionMenuItem {
        ExtensionMenuItem {
            id: id.to_owned(),
            label: format!("Run {id}"),
            description: None,
            command: Some(command.to_owned()),
            arguments: vec![id.to_owned()],
            destructive: false,
            recommended: false,
            items: None,
            detail: None,
        }
    }

    fn submenu(id: &str, items: Vec<ExtensionMenuItem>) -> ExtensionMenuItem {
        ExtensionMenuItem {
            id: id.to_owned(),
            label: id.to_owned(),
            description: None,
            command: None,
            arguments: Vec::new(),
            destructive: false,
            recommended: false,
            items: Some(items),
            detail: None,
        }
    }

    fn declared() -> Vec<String> {
        vec!["tool".to_owned()]
    }

    #[test]
    fn a_nested_menu_routed_to_declared_commands_is_valid() {
        let mut setup = action("setup", "tool");
        setup.recommended = true;
        let menu = ExtensionMenu {
            title: Some("Tool".into()),
            status: Some(ExtensionPresentationStatus {
                state: ExtensionPresentationState::Active,
                label: "Ready".into(),
                detail: None,
            }),
            detail: None,
            items: vec![
                setup,
                submenu(
                    "servers",
                    vec![action("restart", "tool"), submenu("empty", Vec::new())],
                ),
            ],
        };
        menu.validate(&declared()).unwrap();
        let wire = serde_json::to_value(&menu).unwrap();
        assert_eq!(serde_json::from_value::<ExtensionMenu>(wire).unwrap(), menu);
    }

    #[test]
    fn actions_cannot_route_outside_the_extensions_declared_commands() {
        let menu = ExtensionMenu {
            items: vec![action("escape", "other")],
            ..ExtensionMenu::default()
        };
        assert!(menu
            .validate(&declared())
            .unwrap_err()
            .contains("undeclared command"));
    }

    #[test]
    fn malformed_menus_are_rejected() {
        let both = ExtensionMenuItem {
            items: Some(Vec::new()),
            ..action("both", "tool")
        };
        let neither = ExtensionMenuItem {
            command: None,
            ..action("neither", "tool")
        };
        let mut destructive_submenu = submenu("danger", Vec::new());
        destructive_submenu.destructive = true;
        let mut first = action("one", "tool");
        first.recommended = true;
        let mut second = action("two", "tool");
        second.recommended = true;
        let mut too_deep = action("leaf", "tool");
        for level in 0..MAX_EXTENSION_MENU_DEPTH {
            too_deep = submenu(&format!("level-{level}"), vec![too_deep]);
        }
        for items in [
            vec![both],
            vec![neither],
            vec![destructive_submenu],
            vec![first, second],
            vec![action("same", "tool"), action("same", "tool")],
            vec![too_deep],
            vec![ExtensionMenuItem {
                label: " ".into(),
                ..action("blank", "tool")
            }],
            (0..=MAX_EXTENSION_MENU_ITEMS)
                .map(|index| action(&format!("item-{index}"), "tool"))
                .collect(),
        ] {
            let menu = ExtensionMenu {
                items,
                ..ExtensionMenu::default()
            };
            assert!(menu.validate(&declared()).is_err(), "{menu:?}");
        }
    }
}
