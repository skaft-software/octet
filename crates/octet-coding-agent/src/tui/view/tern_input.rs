//! The single frontend input owner routes native replies and editor gestures.

use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event as InputEvent, KeyCode, KeyEvent, KeyModifiers};
use octet_tern::frame::Incoming;
use octet_tern::wire::Event;

use super::renderer_runtime::SharedState;
use super::{invalidate_editor_autocomplete, request_file_index_scan, InteractiveShell};
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
    pub(super) scroll_supported: bool,
    pub(super) scroll: VecDeque<octet_tern::wire::ScrollBy>,
}

impl InteractiveShell {
    /// Only advertise-native navigation enters the native writer's bounded
    /// queue. The renderer owns credit admission and the actual frame write.
    pub(super) fn request_native_scroll(&self, by: octet_tern::wire::ScrollBy, count: usize) {
        let mut mailbox = self.state.native().lock().expect("native mailbox poisoned");
        if mailbox.accepting_input && mailbox.scroll_supported {
            let count = count.min(512usize.saturating_sub(mailbox.scroll.len()));
            mailbox.scroll.extend(std::iter::repeat_n(by, count));
        }
    }

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
    // A temporary input owner supersedes a still-open underlying panel.
    // Its native cancellation is request-fenced; other controls must never
    // synthesize Enter/Esc into an ordinary or host-private input request.
    if matches!(message, Incoming::Event(
        Event::Edit { id, .. } | Event::Select { id, .. }
        | Event::Activate { id, .. } | Event::Action { id, .. }
    ) if !id.starts_with("prompt."))
        && state.borrow().tool_input_prompt.is_some()
    {
        return None;
    }
    match message {
        Incoming::Event(Event::Select { sf, id, item } | Event::Activate { sf, id, item })
            if sf == super::tern::SURFACE && id.starts_with("completion.") =>
        {
            let mut shell = state.borrow_mut();
            if !super::tern::editor_focused(&shell) {
                return None;
            }
            let completion = super::tern_completion::Completion::capture(&shell)?;
            if id != &completion.id {
                return None;
            }
            let index = completion.index(item)?;
            let activate = matches!(message, Incoming::Event(Event::Activate { .. }));
            let binding = match completion.source {
                super::tern_completion::Source::Slash => {
                    shell.slash_selection = index;
                    "tui.select.confirm"
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
            let report = id.starts_with("report.");
            let current = if report {
                shell.overlay.is_some()
                    && shell.panel.is_none()
                    && shell.tool_input_prompt.is_none()
                    && !shell.extension_ui.remote_fullscreen_overlay
            } else {
                // A temporary request has its own prompt epoch, never the last
                // panel's cancellation authority.
                shell.panel.is_some() && shell.tool_input_prompt.is_none()
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
                        if report {
                            shell.overlay_epoch
                        } else {
                            shell.panel_epoch
                        }
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
            if id != &super::tern_picker::id(&shell)
                || !shell
                    .panel
                    .as_ref()
                    .is_some_and(super::tern_picker::interactive)
            {
                return None;
            }
            super::tern_picker::select(&mut shell, item)?;
            drop(shell);
            if matches!(message, Incoming::Event(Event::Activate { .. })) {
                return bound_key(state, "tui.select.confirm");
            }
            None
        }
        Incoming::Event(Event::Action {
            sf, id, act, value, ..
        }) if sf == super::tern::SURFACE && id.starts_with("panel.") => {
            let mut shell = state.borrow_mut();
            if !super::tern_picker::owns_action(&shell, id) {
                return None;
            }
            if matches!(act.as_str(), "cancel" | "close") {
                drop(shell);
                return bound_key(state, "tui.select.cancel");
            }
            if !shell
                .panel
                .as_ref()
                .is_some_and(super::tern_picker::interactive)
            {
                return None;
            }
            if act == "scope" {
                super::tern_picker::scope(&mut shell, value.as_deref()?)?;
                return None;
            }
            let session = matches!(shell.panel, Some(super::Panel::SessionPicker { .. }));
            let act = if session && act == "strip" {
                match value.as_deref()? {
                    action @ ("workspace" | "sort" | "named" | "paths") => action,
                    _ => return None,
                }
            } else {
                act.as_str()
            };
            drop(shell);
            bound_key(
                state,
                match act {
                    "workspace" if session => "tui.input.tab",
                    "sort" if session => "app.session.toggleSort",
                    "named" if session => "app.session.toggleNamedFilter",
                    "paths" if session => "app.session.togglePath",
                    "search" if session => "app.session.search",
                    "rename" if session => "app.session.rename",
                    "delete" if session => "app.session.delete",
                    "confirm" => "tui.select.confirm",
                    "up" => "tui.select.up",
                    "down" => "tui.select.down",
                    "cancel" | "close" => "tui.select.cancel",
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
            if id != &super::tern_picker::id(&shell)
                && id != &format!("{}.filter", super::tern_picker::id(&shell))
            {
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
        }) if sf == super::tern::SURFACE && id.starts_with("session-edit.") => {
            super::tern_picker::session_edit::edit(
                &mut state.borrow_mut(),
                id,
                *from,
                *to,
                text,
                *cursor,
                *len,
            );
            None
        }
        Incoming::Event(Event::Action {
            sf, id, act, value, ..
        }) if sf == super::tern::SURFACE && id.starts_with("session-edit.") => {
            if value.is_some() {
                return None;
            }
            let shell = state.borrow();
            let binding = super::tern_picker::session_edit::action(&shell, id, act)?;
            drop(shell);
            bound_key(state, binding)
        }
        Incoming::Event(Event::Edit {
            sf,
            id,
            from,
            to,
            text,
            cursor,
            len,
        }) if sf == super::tern::SURFACE && id.starts_with("prompt.") => {
            super::tern_prompt::edit(&mut state.borrow_mut(), id, *from, *to, text, *cursor, *len);
            None
        }
        Incoming::Event(Event::Action {
            sf, id, act, value, ..
        }) if sf == super::tern::SURFACE && id.starts_with("prompt.") => {
            if value.is_some() {
                return None;
            }
            let shell = state.borrow();
            let binding = super::tern_prompt::action(&shell, id, act)?;
            if shell.tool_input_editor.is_none() && binding == "tui.select.cancel" {
                // Secret controls belong to the private raw-input owner, not
                // ordinary picker bindings (which may be printable characters).
                return Some(InputEvent::Key(KeyEvent::new(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                )));
            }
            drop(shell);
            bound_key(state, binding)
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
            if !super::tern::editor_focused(&shell) || shell.startup_pending {
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
            let text_revision = shell.editor.text_revision();
            if !shell.editor.replace_range(from..to, &text) {
                return None;
            }
            shell.editor.set_cursor(cursor);
            shell.prompt_history_navigation = None;
            shell.composer_preferred_column = None;
            if shell.editor.text_revision() != text_revision {
                shell.slash_selection = 0;
                shell.slash_scroll = 0;
                shell.slash_popup_dismissed = false;
            }
            invalidate_editor_autocomplete(&mut shell);
            if shell.editor.cursor() == shell.editor.text().len() {
                request_file_index_scan(&mut shell);
            }
            None
        }
        Incoming::Event(Event::Action { sf, id, act, .. }) if sf == super::tern::SURFACE => {
            let shell = state.borrow();
            if !super::tern::editor_focused(&shell) || shell.startup_pending {
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

pub(super) fn key_event(key: &str) -> Option<KeyEvent> {
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
    use super::super::{tern_completion::Completion, SlashMenuAction};
    use super::*;
    use crate::tui::keymap::InputAction;

    fn native_slash_shell() -> InteractiveShell {
        let shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().startup_pending = false;
        shell.state.native().lock().unwrap().accepting_input = true;
        shell.state.borrow_mut().editor.set_text("/");
        shell
    }

    #[test]
    fn native_slash_activation_uses_confirm_instead_of_submit() {
        for name in ["/help", "/exit", "/late-extension"] {
            let mut shell = native_slash_shell();
            shell
                .set_extension_commands(vec![("late-extension".into(), "Extension".into())].into());
            shell.input_dispatch.bindings = KeybindingsManager::with_platform(
                "macos",
                false,
                std::collections::BTreeMap::from([
                    ("tui.input.submit".into(), vec!["ctrl+enter".into()]),
                    ("tui.select.confirm".into(), vec!["ctrl+y".into()]),
                ]),
            );
            let completion = Completion::capture(&shell.state.borrow()).unwrap();
            let index = completion
                .entries
                .iter()
                .position(|entry| entry.0 == name)
                .unwrap();
            let handler = shell.tern_input_handler();
            let event = handler(Incoming::Event(Event::Activate {
                sf: super::super::tern::SURFACE.into(),
                id: completion.id.clone(),
                item: completion.item_id(index),
            }));
            assert_eq!(
                event,
                Some(InputEvent::Key(KeyEvent::new(
                    KeyCode::Char('y'),
                    KeyModifiers::CONTROL
                )))
            );
            assert_eq!(shell.pending(), "/");
            assert_eq!(shell.state.borrow().slash_selection, index);
            assert_eq!(
                shell.translate_input(event, false),
                InputAction::SlashMenu(SlashMenuAction::Select)
            );
            assert!(shell.slash_menu(SlashMenuAction::Select));
            assert_eq!(shell.pending().trim_end(), name);
            assert!(!shell.slash_popup_open());
        }
    }

    #[test]
    fn native_slash_activation_respects_disabled_confirm() {
        let mut shell = native_slash_shell();
        shell.input_dispatch.bindings = KeybindingsManager::with_platform(
            "macos",
            false,
            std::collections::BTreeMap::from([("tui.select.confirm".into(), Vec::new())]),
        );
        let completion = Completion::capture(&shell.state.borrow()).unwrap();
        let index = completion.entries.len() - 1;
        let handler = shell.tern_input_handler();
        assert!(handler(Incoming::Event(Event::Activate {
            sf: super::super::tern::SURFACE.into(),
            id: completion.id.clone(),
            item: completion.item_id(index),
        }))
        .is_none());
        assert_eq!(shell.pending(), "/");
        assert_eq!(shell.state.borrow().slash_selection, index);
        assert!(shell.slash_popup_open());
    }

    #[test]
    fn native_cursor_only_edits_preserve_slash_selection_and_dismissal() {
        for dismissed in [false, true] {
            let shell = native_slash_shell();
            let (text_revision, revision) = {
                let mut state = shell.state.borrow_mut();
                state.slash_selection = 10;
                state.slash_scroll = 3;
                state.slash_popup_dismissed = dismissed;
                (state.editor.text_revision(), state.editor.revision())
            };
            let handler = shell.tern_input_handler();
            // An empty splice moves the caret; replacing text with itself is
            // also not a text mutation and must not reopen a dismissed popup.
            for (to, text, cursor) in [(0, "", 0), (1, "/", 1)] {
                handler(Incoming::Event(Event::Edit {
                    sf: super::super::tern::SURFACE.into(),
                    id: "composer.editor".into(),
                    from: 0,
                    to,
                    text: text.into(),
                    cursor,
                    len: 1,
                }));
                let state = shell.state.borrow();
                assert_eq!(state.editor.text(), "/");
                assert_eq!(state.editor.cursor(), cursor);
                assert_eq!(state.editor.text_revision(), text_revision);
                assert!(state.editor.revision() > revision);
                assert_eq!(state.slash_selection, 10);
                assert_eq!(state.slash_scroll, 3);
                assert_eq!(state.slash_popup_dismissed, dismissed);
                if !dismissed {
                    assert_eq!(Completion::capture(&state).unwrap().selected, 10);
                } else {
                    assert!(Completion::capture(&state).is_none());
                }
            }
        }
    }

    #[test]
    fn native_text_edits_reset_slash_selection_and_reopen_the_popup() {
        let shell = native_slash_shell();
        let text_revision = {
            let mut state = shell.state.borrow_mut();
            state.slash_selection = 10;
            state.slash_scroll = 3;
            state.slash_popup_dismissed = true;
            state.editor.text_revision()
        };
        let handler = shell.tern_input_handler();
        handler(Incoming::Event(Event::Edit {
            sf: super::super::tern::SURFACE.into(),
            id: "composer.editor".into(),
            from: 1,
            to: 1,
            text: "s".into(),
            cursor: 2,
            len: 1,
        }));
        let state = shell.state.borrow();
        assert_eq!(state.editor.text(), "/s");
        assert!(state.editor.text_revision() > text_revision);
        assert_eq!(state.slash_selection, 0);
        assert_eq!(state.slash_scroll, 0);
        assert!(!state.slash_popup_dismissed);
        assert_eq!(Completion::capture(&state).unwrap().selected, 0);
    }

    #[test]
    fn native_slash_paging_uses_its_viewport_only_while_it_owns_input() {
        for (columns, rows) in [(120, 40), (46, 8)] {
            let mut shell = native_slash_shell();
            shell.set_size(columns, rows);
            let ansi_page = super::super::shell_chrome(
                &shell.state.borrow(),
                columns,
                std::time::Instant::now(),
            )
            .suggestions
            .len()
            .saturating_sub(1)
            .max(1);
            assert_ne!(ansi_page, super::super::tern_completion::MAX_LINES);
            for native in [false, true, false] {
                shell.state.native().lock().unwrap().accepting_input = native;
                let page = if native {
                    super::super::tern_completion::MAX_LINES
                } else {
                    ansi_page
                };
                shell.slash_menu(SlashMenuAction::First);
                shell.slash_menu(SlashMenuAction::PageDown);
                assert_eq!(shell.state.borrow().slash_selection, page);
                shell.slash_menu(SlashMenuAction::PageDown);
                assert_eq!(shell.state.borrow().slash_selection, 2 * page);
                shell.slash_menu(SlashMenuAction::PageUp);
                assert_eq!(shell.state.borrow().slash_selection, page);
                shell.slash_menu(SlashMenuAction::PageUp);
                assert_eq!(shell.state.borrow().slash_selection, 0);
            }
        }
    }

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
    fn document_close_is_request_fenced_and_resolves_cancel_binding() {
        use super::super::{tern_picker, Panel};
        let mut shell = InteractiveShell::test_shell();
        shell.state.native().lock().unwrap().accepting_input = true;
        shell.input_dispatch.bindings = KeybindingsManager::with_platform(
            "linux",
            false,
            std::collections::BTreeMap::from([("tui.select.cancel".into(), vec!["ctrl+y".into()])]),
        );
        shell.open_panel(Panel::ReadOnlyDocument {
            title: "Transient instructions".into(),
            text: "No credential".into(),
            styled: false,
            scroll_from_bottom: 0,
        });
        let id = format!("{}.cancel", tern_picker::id(&shell.state.borrow()));
        let handler = shell.tern_input_handler();
        let action = Incoming::Event(Event::Action {
            sf: super::super::tern::SURFACE.into(),
            id,
            act: "cancel".into(),
            value: None,
            mods: None,
        });
        assert_eq!(
            handler(action.clone()),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::CONTROL,
            )))
        );
        shell.state.native().lock().unwrap().bindings = Some(KeybindingsManager::with_platform(
            "linux",
            false,
            std::collections::BTreeMap::from([("tui.select.cancel".into(), Vec::new())]),
        ));
        assert!(handler(action.clone()).is_none());
        shell.close_panel();
        assert!(handler(action).is_none());
    }

    #[test]
    fn temporary_input_rejects_all_underlying_panel_gestures() {
        use super::super::{tern_picker, OrdinarySurfaceMetadata, Panel, PanelAction};
        for secret in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.state.native().lock().unwrap().accepting_input = true;
            shell.open_panel(Panel::SelectList {
                surface: OrdinarySurfaceMetadata::new("Underlying choice"),
                items: vec!["First".into(), "Second".into()],
                descriptions: vec![None, None],
                selected: 0,
                filter: String::new(),
                action: PanelAction::SelectThinking(Vec::new()),
            });
            let id = tern_picker::id(&shell.state.borrow());
            shell.begin_tool_input("Current request", secret);
            let handler = shell.tern_input_handler();
            for verb in ["select", "activate", "action", "edit"] {
                let wire = serde_json::json!({"ev":verb,"sf":super::super::tern::SURFACE,
                    "id":id,"item":"1","act":"confirm","from":0,"to":0,"text":"stale",
                    "cursor":5,"len":0});
                let event = octet_tern::frame::decode_body("e", &wire.to_string()).unwrap();
                assert!(handler(event).is_none());
            }
            let state = shell.state.borrow();
            let Some(Panel::SelectList {
                selected, filter, ..
            }) = &state.panel
            else {
                panic!("panel retained")
            };
            assert_eq!(*selected, 0);
            assert_eq!(filter, "");
            assert_eq!(state.tool_input_prompt.as_deref(), Some("Current request"));
            assert_eq!(
                state.tool_input_editor.as_ref().map(|editor| editor.text()),
                (!secret).then_some("")
            );
        }
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
