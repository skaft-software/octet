//! Request-fenced temporary extension input. Secret answers are host-private.

use octet_tern::wire::{Kind, Node, Props};
use sexy_tui_rs::TextEditor;

use super::{tern_controls, ShellState};

fn prefix(shell: &ShellState) -> String {
    format!("prompt.{}", shell.tool_input_epoch)
}

pub(crate) fn focus(shell: &ShellState) -> Option<String> {
    shell.tool_input_prompt.as_ref()?;
    shell
        .tool_input_editor
        .as_ref()
        .map(|_| format!("{}.editor", prefix(shell)))
}

pub(crate) fn node(shell: &ShellState) -> Option<Node> {
    let prompt = shell.tool_input_prompt.as_ref()?;
    let id = prefix(shell);
    let mut children = vec![Node::new(
        format!("{id}.prompt"),
        Kind::Text,
        Props::new().set("text", prompt).set("wrap", "word"),
    )];
    if let Some(editor) = &shell.tool_input_editor {
        children.push(Node::new(
            format!("{id}.editor"),
            Kind::Editor,
            Props::new()
                .set("text", editor.text())
                .set(
                    "cursor",
                    editor.text()[..editor.cursor()].encode_utf16().count(),
                )
                .set("maxLines", 12)
                .set("placeholder", "Enter a value"),
        ));
    }
    let cancel = tern_controls::hint(shell, "tui.select.cancel", "cancels");
    let instruction = if shell.tool_input_editor.is_some() {
        format!(
            "{}; {cancel}",
            tern_controls::hint(shell, "tui.select.confirm", "submits")
        )
    } else {
        // Secret submission/cancellation are raw host controls. Picker bindings
        // must never reinterpret a printable credential byte as an action.
        "Input stays in the host; Enter submits, Esc cancels".to_owned()
    };
    children.push(Node::new(
        format!("{id}.host"),
        Kind::Text,
        Props::new().set("text", instruction).set("wrap", "word"),
    ));
    if shell.tool_input_overflowed {
        children.push(Node::new(
            format!("{id}.overflow"),
            Kind::Text,
            Props::new()
                .set(
                    "text",
                    format!(
                        "Input exceeds 4 KiB; {}; {}",
                        if shell.tool_input_editor.is_some() {
                            tern_controls::hint(shell, "tui.select.confirm", "to clear")
                        } else {
                            "Enter to clear (host only)".to_owned()
                        },
                        if shell.tool_input_editor.is_some() {
                            tern_controls::hint(shell, "tui.select.cancel", "to cancel")
                        } else {
                            "Esc to cancel".to_owned()
                        }
                    ),
                )
                .set("wrap", "word"),
        ));
    }
    let cancel_action = if shell.tool_input_editor.is_some() {
        tern_controls::action(shell, &id, "cancel", "Cancel", "tui.select.cancel")
    } else {
        Node::new(
            format!("{id}.cancel"),
            Kind::Kbd,
            Props::new()
                .set("keys", ["esc"])
                .set("title", "Cancel")
                .set("actions", serde_json::json!({"click":"cancel"})),
        )
    };
    let mut actions = vec![cancel_action];
    if shell.tool_input_editor.is_some() {
        actions.push(tern_controls::action(
            shell,
            &id,
            "confirm",
            if shell.tool_input_overflowed {
                "Clear rejected input"
            } else {
                "Submit"
            },
            "tui.select.confirm",
        ));
    }
    children.push(Node::with_children(
        format!("{id}.actions"),
        Kind::Row,
        Props::new().set("gap", "sm").set("wrap", true),
        actions,
    ));
    Some(Node::with_children(
        format!("{id}.sheet"),
        Kind::Overlay,
        Props::new()
            .role("octet.prompt")
            .text("head", "Extension input")
            .set("modal", true)
            .set("size", "lg"),
        children,
    ))
}

/// Only this request's labeled controls can synthesize host selection keys.
/// A secret submit is intentionally absent: Enter must arrive via host input.
pub(crate) fn action(shell: &ShellState, id: &str, act: &str) -> Option<&'static str> {
    shell.tool_input_prompt.as_ref()?;
    let current = prefix(shell);
    if id == format!("{current}.cancel") && act == "cancel" {
        Some("tui.select.cancel")
    } else if shell.tool_input_editor.is_some()
        && id == format!("{current}.confirm")
        && act == "confirm"
    {
        Some("tui.select.confirm")
    } else {
        None
    }
}

/// Atomic, UTF-16-fenced edits over the exact same editor used by raw keys.
#[allow(clippy::too_many_arguments)]
pub(crate) fn edit(
    shell: &mut ShellState,
    id: &str,
    from: usize,
    to: usize,
    text: &str,
    cursor: usize,
    len: usize,
) -> Option<()> {
    if focus(shell).as_deref() != Some(id) {
        return None;
    }
    let editor = shell.tool_input_editor.as_mut()?;
    let source = editor.text();
    if source.encode_utf16().count() != len
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return None;
    }
    let from = utf16_boundary(source, from)?;
    let to = utf16_boundary(source, to)?;
    if from > to {
        return None;
    }
    let mut raw = source.to_owned();
    raw.replace_range(from..to, text);
    let cursor = utf16_boundary(&raw, cursor)?;
    let cursor = TextEditor::normalize_paste(&raw[..cursor]).len();
    let replacement = TextEditor::normalize_paste(text);
    if source.len() - (to - from) + replacement.len() > 4096 {
        shell.tool_input_overflowed = true;
        shell.tool_input_revision = shell.tool_input_revision.saturating_add(1);
        return None;
    }
    if !editor.edit_range(from..to, &replacement, cursor) {
        return None;
    }
    shell.tool_input_revision = shell.tool_input_revision.saturating_add(1);
    Some(())
}

fn utf16_boundary(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += character.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sexy_tui_rs::TextEditAction;

    #[test]
    fn ordinary_raw_and_native_input_share_one_request_and_preserve_draft() {
        let mut shell = ShellState::default();
        shell.editor.set_text("parent draft");
        let parent_revision = shell.editor.revision();
        shell.begin_tool_input("First line\nSecond line", false);
        let id = focus(&shell).unwrap();
        let confirm = format!("{}.confirm", prefix(&shell));
        assert_eq!(
            action(&shell, &confirm, "confirm"),
            Some("tui.select.confirm")
        );
        assert_eq!(action(&shell, &id, "confirm"), None);
        shell.edit_tool_input(TextEditAction::Char('🦀'));
        assert_eq!(focus(&shell).as_deref(), Some(id.as_str()));
        assert_eq!(edit(&mut shell, &id, 2, 2, "雪", 3, 2), Some(()));
        assert_eq!(edit(&mut shell, &id, 0, 0, "x", 1, 2), None);
        assert_eq!(edit(&mut shell, &id, 1, 1, "x", 2, 3), None);
        assert_eq!(edit(&mut shell, &id, 0, 0, "\u{1b}", 1, 3), None);
        let sheet = node(&shell).unwrap();
        assert_eq!(
            sheet.c.as_ref().unwrap()[0].p.as_ref().unwrap().as_map()["text"],
            "First line\nSecond line"
        );
        assert_eq!(shell.end_tool_input().as_deref(), Some("🦀雪"));
        assert_eq!(shell.editor.text(), "parent draft");
        assert_eq!(shell.editor.revision(), parent_revision);
        assert!(focus(&shell).is_none());
        assert!(node(&shell).is_none());
        shell.begin_tool_input("Next request", false);
        assert_ne!(focus(&shell).as_deref(), Some(id.as_str()));
        assert_eq!(edit(&mut shell, &id, 0, 0, "stale", 5, 0), None);
        assert_eq!(action(&shell, &confirm, "confirm"), None);
    }

    #[test]
    fn prompt_hints_resolve_bindings_and_unbound_cancel_has_no_pointer_action() {
        use crate::tui::keymap::keybindings::KeybindingsManager;
        let mut shell = ShellState::default();
        shell.native_keys = Some(KeybindingsManager::with_platform(
            "linux",
            false,
            std::collections::BTreeMap::from([
                ("tui.select.confirm".into(), vec!["ctrl+y".into()]),
                ("tui.select.cancel".into(), Vec::new()),
            ]),
        ));
        shell.begin_tool_input("Ordinary", false);
        let rendered = serde_json::to_string(&node(&shell).unwrap()).unwrap();
        assert!(rendered.contains("Ctrl+Y submits; cancels unavailable"));
        assert!(rendered.contains("Cancel unavailable"));
        assert!(!rendered.contains("\"click\":\"cancel\""));
        assert!(!rendered.contains("\"disabled\""));
        shell.end_tool_input();
        shell.begin_tool_input("Secret", true);
        let rendered = serde_json::to_string(&node(&shell).unwrap()).unwrap();
        assert!(rendered.contains("Enter submits"));
        assert!(rendered.contains("Esc cancels"));
        assert!(!rendered.contains("Ctrl+Y"));
        assert!(rendered.contains("\"click\":\"cancel\""));
        assert!(rendered.contains("\"keys\":[\"esc\"]"));
        assert!(!rendered.contains("\"click\":\"confirm\""));
        assert!(!rendered.contains("\"k\":\"editor\""));
    }

    #[test]
    fn over_limit_edits_are_atomic_and_visible() {
        let mut shell = ShellState::default();
        shell.begin_tool_input("Value", false);
        let id = focus(&shell).unwrap();
        shell.edit_tool_input(TextEditAction::Paste("a".repeat(4096)));
        shell.edit_tool_input(TextEditAction::Char('b'));
        assert_eq!(shell.tool_input_editor.as_ref().unwrap().text().len(), 4096);
        assert!(shell.tool_input_overflowed);
        assert_eq!(
            edit(&mut shell, &id, 0, 4096, &"b".repeat(4097), 4097, 4096),
            None
        );
        assert_eq!(
            shell.tool_input_editor.as_ref().unwrap().text(),
            "a".repeat(4096)
        );
        assert_eq!(edit(&mut shell, &id, 0, 4096, "short", 5, 4096), Some(()));
        assert!(shell.tool_input_overflowed);
        assert!(serde_json::to_string(&node(&shell).unwrap())
            .unwrap()
            .contains("Input exceeds 4 KiB"));
    }

    #[test]
    fn secret_sheet_has_no_editor_or_draft_and_cannot_submit_natively() {
        let mut shell = ShellState::default();
        shell.editor.set_text("private parent draft");
        shell.begin_tool_input("Secret\nFull prompt", true);
        let prefix = prefix(&shell);
        assert!(focus(&shell).is_none());
        assert!(shell.tool_input_editor.is_none());
        assert_eq!(
            edit(
                &mut shell,
                &format!("{prefix}.editor"),
                0,
                0,
                "secret",
                6,
                0
            ),
            None
        );
        assert_eq!(
            action(&shell, &format!("{prefix}.confirm"), "confirm"),
            None
        );
        assert_eq!(
            action(&shell, &format!("{prefix}.cancel"), "cancel"),
            Some("tui.select.cancel")
        );
        assert_eq!(action(&shell, "prompt.0.cancel", "cancel"), None);
        let sheet = node(&shell).unwrap();
        assert!(sheet
            .c
            .as_ref()
            .unwrap()
            .iter()
            .all(|child| child.k != Kind::Editor));
        let rendered = serde_json::to_string(&sheet).unwrap();
        assert!(!rendered.contains("private parent draft"));
        assert!(rendered.contains("Input stays in the host; Enter submits, Esc cancels"));
        shell.end_tool_input();
        assert_eq!(action(&shell, &format!("{prefix}.cancel"), "cancel"), None);
    }
}
