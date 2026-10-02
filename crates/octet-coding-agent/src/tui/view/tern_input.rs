//! The single frontend input owner routes native replies and editor gestures.

use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event as InputEvent, KeyCode, KeyEvent, KeyModifiers};
use octet_tern::frame::Incoming;
use octet_tern::wire::Event;

use super::renderer_runtime::SharedState;
use super::{
    invalidate_editor_autocomplete, normal_editor_focused, request_file_index_scan,
    InteractiveShell,
};
use crate::tui::keymap::keybindings::{normalize_key_id, KeybindingsManager};

pub(crate) type Handler = Arc<dyn Fn(Incoming) -> Option<InputEvent> + Send + Sync>;

#[derive(Default)]
pub(super) struct Mailbox {
    pub(super) messages: VecDeque<Incoming>,
    pub(super) bindings: Option<KeybindingsManager>,
    pub(super) editor_resync: u64,
    /// OS-level keyboard-focus returns (`FocusGained`) observed by the
    /// frontend. The native renderer treats a change like `set_visible(true)`:
    /// it forces a frame and re-asserts `composer.editor` focus, because Tern
    /// does not always deliver a TSP `Visible(true)` when returning from
    /// another app or overlay (screen recording, space switch).
    pub(super) focus_resync: u64,
    pub(super) accepting_input: bool,
}

impl InteractiveShell {
    pub(crate) fn tern_input_handler(&self) -> Handler {
        self.state
            .native()
            .lock()
            .expect("native mailbox poisoned")
            .bindings = Some(self.input_dispatch.bindings.clone());
        let state = self.state.clone();
        let render = self.render_tx.clone();
        Arc::new(move |message| {
            let input = route(&state, &message);
            if matches!(message, Incoming::Event(Event::Edit { .. })) {
                state
                    .native()
                    .lock()
                    .expect("native mailbox poisoned")
                    .editor_resync += 1;
            }
            // Credit, appearance, and eviction messages are still owned by the
            // renderer, never discarded after filtering them from the draft.
            if matches!(
                message,
                Incoming::Reply(_)
                    | Incoming::Event(
                        Event::Ack { .. }
                            | Event::Theme { .. }
                            | Event::Motion { .. }
                            | Event::Resize { .. }
                            | Event::Gone { .. }
                            | Event::Visible { .. }
                            | Event::Toggle { .. }
                            | Event::Error { .. }
                    )
            ) {
                let mut mailbox = state.native().lock().expect("native mailbox poisoned");
                if mailbox.messages.len() < 512 {
                    mailbox.messages.push_back(message);
                }
            }
            if let Some(tx) = render.lock().expect("render sender poisoned").as_ref() {
                let _ = tx.try_send(super::renderer_runtime::RenderCommand::Render);
            }
            input
        })
    }
}

fn route(state: &SharedState, message: &Incoming) -> Option<InputEvent> {
    if matches!(
        message,
        Incoming::Event(
            Event::Edit { .. }
                | Event::Select { .. }
                | Event::Activate { .. }
                | Event::Action { .. }
        )
    ) && !state
        .native()
        .lock()
        .expect("native mailbox poisoned")
        .accepting_input
    {
        return None;
    }
    match message {
        Incoming::Event(Event::Select { sf, id, item } | Event::Activate { sf, id, item })
            if sf == super::tern::SURFACE && id.starts_with("completion.") =>
        {
            let mut shell = state.borrow_mut();
            let completion = super::tern_completion::Completion::capture(&shell)?;
            if id != &completion.id {
                return None;
            }
            let index = completion.index(item)?;
            let activate = matches!(message, Incoming::Event(Event::Activate { .. }));
            let binding = match completion.source {
                super::tern_completion::Source::Slash => {
                    shell.slash_selection = index;
                    "tui.input.submit"
                }
                super::tern_completion::Source::Path => {
                    shell.path_selection = index;
                    "tui.input.tab"
                }
                super::tern_completion::Source::Extension => {
                    shell.extension_autocomplete_selection = Some((shell.editor.revision(), index));
                    "tui.input.tab"
                }
            };
            drop(shell);
            activate.then(|| bound_key(state, binding)).flatten()
        }
        Incoming::Event(Event::Action { sf, id, act, .. })
            if sf == super::tern::SURFACE
                && (id.starts_with("report.") || id.starts_with("modal."))
                && matches!(act.as_str(), "close" | "cancel") =>
        {
            let shell = state.borrow();
            let current = if id.starts_with("report.") {
                shell.overlay.is_some()
            } else {
                shell.panel.is_some() || shell.tool_input_prompt.is_some()
            };
            if !current
                || id
                    != &format!(
                        "{}.{}",
                        if id.starts_with("report.") {
                            "report"
                        } else {
                            "modal"
                        },
                        shell.panel_epoch
                    )
            {
                return None;
            }
            drop(shell);
            bound_key(state, "tui.select.cancel")
        }
        Incoming::Event(Event::Select { sf, id, item } | Event::Activate { sf, id, item })
            if sf == super::tern::SURFACE =>
        {
            let mut shell = state.borrow_mut();
            if id != &format!("panel.{}", shell.panel_epoch) {
                return None;
            }
            super::tern_picker::select(&mut shell, item)?;
            drop(shell);
            if matches!(message, Incoming::Event(Event::Activate { .. })) {
                return bound_key(state, "tui.select.confirm");
            }
            None
        }
        Incoming::Event(Event::Action { sf, id, act, .. })
            if sf == super::tern::SURFACE && id.starts_with("panel.") =>
        {
            let shell = state.borrow();
            if id != &format!("panel.{}", shell.panel_epoch)
                || !shell
                    .panel
                    .as_ref()
                    .is_some_and(super::tern_picker::interactive)
            {
                return None;
            }
            drop(shell);
            bound_key(
                state,
                match act.as_str() {
                    "confirm" => "tui.select.confirm",
                    "cancel" => "tui.select.cancel",
                    _ => return None,
                },
            )
        }
        Incoming::Event(Event::Edit {
            sf,
            id,
            from,
            to,
            text,
            cursor: _,
            len,
        }) if sf == super::tern::SURFACE && id.starts_with("panel.") => {
            let mut shell = state.borrow_mut();
            if id != &format!("panel.{}", shell.panel_epoch) {
                return None;
            }
            let filter = super::tern_picker::filter(&shell)?;
            if filter.encode_utf16().count() != *len {
                return None;
            }
            let from = utf16_boundary(filter, *from)?;
            let to = utf16_boundary(filter, *to)?;
            if from > to || text.chars().any(char::is_control) {
                return None;
            }
            let mut edited = filter.to_owned();
            edited.replace_range(from..to, text);
            super::tern_picker::replace_filter(&mut shell, edited);
            None
        }
        Incoming::Event(Event::Edit {
            sf,
            id,
            from,
            to,
            text,
            cursor,
            len,
        }) if sf == super::tern::SURFACE && id == "composer.editor" => {
            let mut shell = state.borrow_mut();
            if !normal_editor_focused(&shell) || shell.startup_pending {
                return None;
            }
            let source = shell.editor.text();
            if source.encode_utf16().count() != *len {
                return None; // Gesture against an older rendered draft.
            }
            let from = utf16_boundary(source, *from)?;
            let to = utf16_boundary(source, *to)?;
            if from > to
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
            {
                return None;
            }
            let mut edited = source.to_owned();
            edited.replace_range(from..to, text);
            let raw_cursor = utf16_boundary(&edited, *cursor)?;
            let cursor = sexy_tui_rs::TextEditor::normalize_paste(&edited[..raw_cursor]).len();
            let text = sexy_tui_rs::TextEditor::normalize_paste(text);
            if !shell.editor.replace_range(from..to, &text) {
                return None;
            }
            shell.editor.set_cursor(cursor);
            shell.prompt_history_navigation = None;
            shell.composer_preferred_column = None;
            shell.slash_selection = 0;
            shell.slash_scroll = 0;
            shell.slash_popup_dismissed = false;
            invalidate_editor_autocomplete(&mut shell);
            if shell.editor.cursor() == shell.editor.text().len() {
                request_file_index_scan(&mut shell);
            }
            None
        }
        Incoming::Event(Event::Action { sf, id, act, .. }) if sf == super::tern::SURFACE => {
            let shell = state.borrow();
            if !normal_editor_focused(&shell) || shell.startup_pending {
                return None;
            }
            let binding = match (id.as_str(), act.as_str()) {
                ("composer.model", "model") => "app.model.select",
                ("composer.effort", "effort") => "app.thinking.cycle",
                ("composer.send", "send") => "tui.input.submit",
                ("composer.stop", "stop") => "app.interrupt",
                _ => return None,
            };
            drop(shell);
            bound_key(state, binding)
        }
        _ => None,
    }
}

fn bound_key(state: &SharedState, binding: &str) -> Option<InputEvent> {
    state
        .native()
        .lock()
        .expect("native mailbox poisoned")
        .bindings
        .as_ref()?
        .get_keys(binding)
        .iter()
        .find_map(|key| key_event(key).map(InputEvent::Key))
}

// TSP offsets are UTF-16, while TextEditor offsets are UTF-8 boundaries. Never
// round through half a surrogate or apply a stale gesture to a different draft.
fn utf16_boundary(text: &str, offset: usize) -> Option<usize> {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 == offset {
            return Some(byte);
        }
        utf16 += character.len_utf16();
        if utf16 > offset {
            return None;
        }
    }
    (utf16 == offset).then_some(text.len())
}

fn key_event(key: &str) -> Option<KeyEvent> {
    let key = normalize_key_id(key);
    let (prefix, base) = if key.ends_with("++") {
        (&key[..key.len() - 2], "+")
    } else {
        key.rsplit_once('+').unwrap_or(("", &key))
    };
    let mut modifiers = KeyModifiers::NONE;
    for part in prefix.split('+').filter(|part| !part.is_empty()) {
        modifiers |= match part {
            "ctrl" => KeyModifiers::CONTROL,
            "shift" => KeyModifiers::SHIFT,
            "alt" => KeyModifiers::ALT,
            "super" => KeyModifiers::SUPER,
            _ => return None,
        };
    }
    let code = match base {
        "enter" => KeyCode::Enter,
        "escape" => KeyCode::Esc,
        "tab" if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        value if value.chars().count() == 1 => KeyCode::Char(value.chars().next()?),
        value => KeyCode::F(
            value
                .strip_prefix('f')?
                .parse()
                .ok()
                .filter(|n| (1..=24).contains(n))?,
        ),
    };
    Some(KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_edits_are_normalized_and_rejected_after_ownership_handback() {
        let shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_pending = false;
        let handler = shell.tern_input_handler();
        let edit = Incoming::Event(Event::Edit {
            sf: super::super::tern::SURFACE.into(),
            id: "composer.editor".into(),
            from: 0,
            to: 0,
            text: "a🦀\r\n雪".into(),
            cursor: 6,
            len: 0,
        });
        assert!(handler(edit.clone()).is_none());
        assert!(shell.pending().is_empty());
        shell.state.native().lock().unwrap().accepting_input = true;
        handler(edit.clone());
        assert_eq!(shell.pending(), "a🦀\n雪");
        assert_eq!(shell.state.borrow().editor.cursor(), "a🦀\n雪".len());
        handler(edit); // stale length
        assert_eq!(shell.pending(), "a🦀\n雪");
        shell.state.native().lock().unwrap().accepting_input = false;
        let action = Incoming::Event(Event::Action {
            sf: super::super::tern::SURFACE.into(),
            id: "composer.send".into(),
            act: "send".into(),
            value: None,
            mods: None,
        });
        assert!(handler(action).is_none());
    }

    #[test]
    fn native_extension_selection_accepts_the_selected_item_without_reordering() {
        let mut shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_pending = false;
        shell.state.native().lock().unwrap().accepting_input = true;
        shell.state.borrow_mut().editor.set_text("@");
        let snapshot = shell.extension_editor_snapshot();
        let items = ["alpha", "beta"]
            .into_iter()
            .map(|value| super::super::ShellAutocompleteItem {
                value: value.into(),
                label: value.into(),
                description: None,
            })
            .collect();
        assert!(shell.set_extension_autocomplete(&snapshot, "@".into(), items));
        let completion =
            super::super::tern_completion::Completion::capture(&shell.state.borrow()).unwrap();
        let handler = shell.tern_input_handler();
        handler(Incoming::Event(Event::Select {
            sf: super::super::tern::SURFACE.into(),
            id: completion.id.clone(),
            item: completion.item_id(1),
        }));
        assert_eq!(
            super::super::tern_completion::Completion::capture(&shell.state.borrow())
                .unwrap()
                .selected,
            1
        );
        assert!(shell.accept_extension_autocomplete());
        assert_eq!(shell.pending(), "beta");
    }

    #[test]
    fn native_utf16_edits_do_not_split_unicode() {
        assert_eq!(utf16_boundary("a🦀雪", 1), Some(1));
        assert_eq!(utf16_boundary("a🦀雪", 2), None);
        assert_eq!(utf16_boundary("a🦀雪", 3), Some(5));
        assert_eq!(utf16_boundary("a🦀雪", 4), Some(8));
        assert_eq!(utf16_boundary("a🦀雪", 5), None);
    }

    #[test]
    fn pointer_actions_use_resolved_bindings() {
        let mut shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_pending = false;
        shell.state.native().lock().unwrap().accepting_input = true;
        shell.input_dispatch.bindings = KeybindingsManager::with_platform(
            "macos",
            false,
            std::collections::BTreeMap::from([(
                "tui.input.submit".into(),
                vec!["ctrl+enter".into()],
            )]),
        );
        let handler = shell.tern_input_handler();
        let action = Incoming::Event(Event::Action {
            sf: super::super::tern::SURFACE.into(),
            id: "composer.send".into(),
            act: "send".into(),
            value: None,
            mods: None,
        });
        assert_eq!(
            handler(action.clone()),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL
            )))
        );
        shell.state.native().lock().unwrap().bindings = Some(KeybindingsManager::with_platform(
            "macos",
            false,
            std::collections::BTreeMap::from([("tui.input.submit".into(), Vec::new())]),
        ));
        assert!(handler(action).is_none());
    }
}
