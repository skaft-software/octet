//! Stateful product input policy. Exclusive panels and tool prompts keep their
//! own input owner; editor prefixes never escape into those surfaces.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use super::*;
use crate::tui::keymap::{self, keybindings::KeybindingsManager, InputAction};

pub(super) struct InputDispatch {
    pub(super) bindings: KeybindingsManager,
    jump_forward: Option<bool>,
    models: Vec<String>,
    cycle_target: Option<String>,
    generated_command: bool,
}

impl InputDispatch {
    pub(super) fn new(bindings: KeybindingsManager) -> Self {
        Self {
            bindings,
            jump_forward: None,
            models: Vec::new(),
            cycle_target: None,
            generated_command: false,
        }
    }
}

impl InteractiveShell {
    pub(super) fn reset_input_interaction(&mut self) {
        self.input_dispatch.jump_forward = None;
    }

    /// Only assistant semantic text is copied, never hidden reasoning, tools,
    /// padding, or a model protocol envelope.
    pub fn copy_last_assistant(&mut self) -> Option<String> {
        let text = self
            .state
            .borrow()
            .transcript
            .iter()
            .rev()
            .find_map(|block| {
                matches!(block, TranscriptBlock::Assistant(_)).then(|| block_copy_text(block))
            })?;
        self.state.borrow_mut().copy_buffer = Some(text.clone());
        #[cfg(not(test))]
        Self::set_clipboard(&text);
        Some(text)
    }

    pub(super) fn panel_event(&self, event: &Event) -> Event {
        let Event::Key(key) = event else {
            return event.clone();
        };
        if keymap::is_close_key(key) {
            return event.clone();
        }
        let state = self.state.borrow();
        let bindings = &self.input_dispatch.bindings;
        let mut actions = vec![
            ("tui.select.up", KeyCode::Up, KeyModifiers::NONE),
            ("tui.select.down", KeyCode::Down, KeyModifiers::NONE),
            ("tui.select.pageUp", KeyCode::PageUp, KeyModifiers::NONE),
            ("tui.select.pageDown", KeyCode::PageDown, KeyModifiers::NONE),
            ("tui.select.confirm", KeyCode::Enter, KeyModifiers::NONE),
            ("tui.select.cancel", KeyCode::Esc, KeyModifiers::NONE),
        ];
        if let Some(Panel::SessionPicker { picker }) = state.panel.as_ref() {
            if picker.rename.is_none() && !picker.confirming_delete {
                actions.extend([
                    (
                        "app.session.toggleSort",
                        KeyCode::Char('s'),
                        KeyModifiers::CONTROL,
                    ),
                    (
                        "app.session.search",
                        KeyCode::Char('f'),
                        KeyModifiers::CONTROL,
                    ),
                    (
                        "app.session.toggleNamedFilter",
                        KeyCode::Char('n'),
                        KeyModifiers::CONTROL,
                    ),
                    (
                        "app.session.togglePath",
                        KeyCode::Char('p'),
                        KeyModifiers::CONTROL,
                    ),
                    (
                        "app.session.rename",
                        KeyCode::Char('r'),
                        KeyModifiers::CONTROL,
                    ),
                    ("app.session.delete", KeyCode::Delete, KeyModifiers::NONE),
                ]);
                if picker.filter.is_empty() {
                    actions.push((
                        "app.session.deleteNoninvasive",
                        KeyCode::Delete,
                        KeyModifiers::NONE,
                    ));
                }
            }
        }
        // Explicit bindings win over defaults, within this input owner's actions.
        actions.sort_by_key(|(id, _, _)| !bindings.user_bindings().contains_key(*id));
        for (id, code, modifiers) in &actions {
            if bindings.matches(key, id) {
                return Event::Key(KeyEvent::new_with_kind(*code, *modifiers, key.kind));
            }
        }
        // Replaced or disabled defaults do not survive via hard-coded fallbacks.
        for (id, code, modifiers) in actions {
            if bindings.user_bindings().contains_key(id)
                && key.code == code
                && key.modifiers == modifiers
            {
                return Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Null,
                    KeyModifiers::NONE,
                    key.kind,
                ));
            }
        }
        event.clone()
    }

    /// Reload only the user-owned keybinding file. No project input policy or
    /// executable resource is loaded here.
    pub fn reload_keybindings(&mut self) {
        self.input_dispatch.bindings.reload();
        self.input_dispatch.jump_forward = None;
        let conflicts = self.input_dispatch.bindings.get_conflicts();
        if !conflicts.is_empty() {
            self.state.borrow_mut().error = Some(format!(
                "{} conflicting user keybinding(s); /hotkeys shows resolved actions",
                conflicts.len()
            ));
        }
    }

    /// Ordered, available model ids supplied by the App's scoped catalog. A
    /// cycle returns an ordinary command, never mutates the active Run's model.
    pub fn set_model_cycle(&mut self, models: Vec<String>) {
        let mut seen = std::collections::HashSet::new();
        self.input_dispatch.models = models
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect();
        self.input_dispatch.cycle_target = None;
    }

    /// Consume a typed slash command, but never drain a draft for a generated
    /// model/session shortcut, even when its bytes happen to match the draft.
    pub fn consume_command_text(&mut self, text: String) -> String {
        if std::mem::take(&mut self.input_dispatch.generated_command) {
            text
        } else if text == self.pending() {
            self.drain_editor()
        } else {
            text
        }
    }

    /// Resolved hotkeys, not a static table that silently ignores user overrides.
    pub fn hotkeys_text(&self) -> String {
        let bindings = &self.input_dispatch.bindings;
        let mut text = String::from("Keybindings (~/.octet/keybindings.json)\nCtrl+D always coordinates close. /reload applies changes.\n\n");
        for definition in bindings.definitions() {
            let keys = bindings.get_keys(&definition.id);
            text.push_str(&format!(
                "{}  {}\n",
                definition.id,
                if keys.is_empty() {
                    "(unbound)".to_owned()
                } else {
                    keys.join(", ")
                }
            ));
        }
        for conflict in bindings.get_conflicts() {
            text.push_str(&format!(
                "\nConflict: {}: {}",
                conflict.key,
                conflict.keybindings.join(", ")
            ));
        }
        text
    }

    /// Translate real terminal input using the shell's loaded user map. This is
    /// the common idle/active/local-shell dispatch seam.
    pub fn translate_input(&mut self, event: Option<Event>, active: bool) -> InputAction {
        self.input_dispatch.generated_command = false;
        if event
            .as_ref()
            .is_some_and(|event| self.intercept_transcript_input(event))
        {
            return InputAction::Ignore;
        }
        if !matches!(event, Some(Event::Key(_)) | Some(Event::Resize(_, _))) {
            self.input_dispatch.jump_forward = None;
        }
        let focused = normal_editor_focused(&self.state.borrow());
        if !focused {
            self.input_dispatch.jump_forward = None;
            // Exclusive owners dispatch their own input. Never edit a hidden draft.
            return match event {
                Some(Event::Key(ref key)) if keymap::is_close_key(key) => InputAction::Closed,
                Some(Event::Resize(w, h)) => InputAction::Resize(w, h),
                Some(Event::FocusLost) => InputAction::FocusLost,
                Some(Event::FocusGained) => InputAction::FocusGained,
                None => InputAction::Closed,
                _ => InputAction::Ignore,
            };
        }
        if let Some(Event::Mouse(mouse)) = event.as_ref() {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let hit = {
                    let state = self.state.borrow();
                    let chrome = shell_chrome(&state, state.size.0, Instant::now());
                    let length = state.rendered_transcript(state.size.0).len();
                    let scroll =
                        resolved_scroll_from_bottom(&state, length, chrome.transcript_rows);
                    let capacity = transcript_viewport_capacity(chrome.transcript_rows, true);
                    scroll > 0
                        && capacity < chrome.transcript_rows
                        && usize::from(mouse.row) == capacity
                        && mouse.column < state.size.0
                };
                if hit {
                    return InputAction::JumpToTail;
                }
            }
        }
        let popup = self.slash_popup_open();
        if let Some(Event::Key(key)) = event.as_ref() {
            if key.kind == KeyEventKind::Release {
                return InputAction::Ignore;
            }
            // Close/clear/interrupt must cancel prefixes rather than becoming a
            // character target. A non-text key cancels and continues normally.
            if let Some(forward) = self.input_dispatch.jump_forward.take() {
                if let Some(character) = sexy_tui_rs::key_text(key) {
                    if key.kind == KeyEventKind::Press {
                        return InputAction::Edit(if forward {
                            EditAction::JumpForward(character)
                        } else {
                            EditAction::JumpBackward(character)
                        });
                    }
                    self.input_dispatch.jump_forward = Some(forward);
                    return InputAction::Ignore;
                }
            }
            if key.code == KeyCode::Char('d') && key.modifiers == KeyModifiers::CONTROL {
                return if key.kind == KeyEventKind::Press {
                    InputAction::Closed
                } else {
                    InputAction::Ignore
                };
            }
            let bindings = &self.input_dispatch.bindings;
            if !popup {
                // Dedicated history actions win over model cycling even in the
                // middle of a multiline draft. Capture the original cursor.
                for (id, previous) in [
                    ("tui.editor.historyPrevious", true),
                    ("tui.editor.historyNext", false),
                ] {
                    if bindings.matches(key, id) {
                        if !active {
                            let mut state = self.state.borrow_mut();
                            if previous
                                && state.prompt_history_navigation.is_none()
                                && !state.prompt_history.is_empty()
                            {
                                let draft = capture_prompt_history_draft(&mut state);
                                let index = state.prompt_history.len() - 1;
                                state.prompt_history_navigation =
                                    Some(PromptHistoryNavigation { index, draft });
                                restore_prompt_history_entry(&mut state, index);
                            } else {
                                navigate_prompt_history(
                                    &mut state,
                                    &if previous {
                                        EditAction::Up
                                    } else {
                                        EditAction::Down
                                    },
                                );
                            }
                        }
                        self.render();
                        return InputAction::Ignore;
                    }
                }
                for (id, forward) in [
                    ("tui.editor.jumpForward", true),
                    ("tui.editor.jumpBackward", false),
                ] {
                    if bindings.matches(key, id) {
                        if key.kind == KeyEventKind::Press {
                            self.input_dispatch.jump_forward = Some(forward);
                        }
                        return InputAction::Ignore;
                    }
                }
                // Explicit editor bindings have priority over default model and
                // viewport shortcuts while the composer owns focus.
                if let Some(action) = keymap::editor_binding(key, bindings, true) {
                    return InputAction::Edit(action);
                }
                if key.kind == KeyEventKind::Press && bindings.matches(key, "app.message.copy") {
                    self.copy_last_assistant();
                    return InputAction::Ignore;
                }
                if key.kind == KeyEventKind::Press {
                    for (id, forward) in [
                        ("app.model.cycleForward", true),
                        ("app.model.cycleBackward", false),
                    ] {
                        if bindings.matches(key, id) {
                            let models = &self.input_dispatch.models;
                            if models.is_empty() {
                                return InputAction::Ignore;
                            }
                            let current = self.state.borrow().model.clone();
                            let from = self
                                .input_dispatch
                                .cycle_target
                                .as_deref()
                                .unwrap_or(&current);
                            let index = match models.iter().position(|model| model == from) {
                                Some(index) if forward => (index + 1) % models.len(),
                                Some(index) => (index + models.len() - 1) % models.len(),
                                None if forward => 0,
                                None => models.len() - 1,
                            };
                            let target = models[index].clone();
                            self.input_dispatch.cycle_target = Some(target.clone());
                            self.input_dispatch.generated_command = true;
                            return InputAction::Command(format!("/model {target}"));
                        }
                    }
                }
                for (id, forward) in [
                    ("tui.altScreen.previousPrompt", false),
                    ("tui.altScreen.nextPrompt", true),
                ] {
                    if bindings.matches(key, id) {
                        self.scroll_to_prompt(forward);
                        self.render();
                        return InputAction::Ignore;
                    }
                }
                // Editor page actions reuse semantic viewport navigation instead
                // of inventing a second independently scrolling composer.
                for (id, direction) in [("tui.editor.pageUp", -1), ("tui.editor.pageDown", 1)] {
                    if bindings.matches(key, id) {
                        self.scroll(direction);
                        self.render();
                        return InputAction::Ignore;
                    }
                }
                let application_viewport = self.state.borrow().application_viewport_requested;
                for (id, direction, divisor) in [
                    ("tui.altScreen.halfPageUp", -1i16, 2usize),
                    ("tui.altScreen.halfPageDown", 1, 2),
                    ("tui.altScreen.lineUp", -1, usize::MAX),
                    ("tui.altScreen.lineDown", 1, usize::MAX),
                ] {
                    if bindings.matches(key, id) {
                        let rows = {
                            let state = self.state.borrow();
                            super::viewport::transcript_viewport_capacity_for_state(
                                &state,
                                state.size.0,
                            )
                        };
                        let step = (rows / divisor).max(1).min(i16::MAX as usize) as i16;
                        self.scroll_lines(direction * step);
                        self.render();
                        return InputAction::Ignore;
                    }
                }
                for (id, bottom) in [("tui.altScreen.top", false), ("tui.altScreen.bottom", true)] {
                    if (application_viewport || bindings.user_bindings().contains_key(id))
                        && bindings.matches(key, id)
                    {
                        if bottom {
                            self.jump_to_tail();
                        } else {
                            self.scroll_to_top();
                        }
                        self.render();
                        return InputAction::Ignore;
                    }
                }
            }
        }
        let typed_submission = matches!(event.as_ref(), Some(Event::Key(key))
            if self.input_dispatch.bindings.matches(key, "tui.input.submit")
                || (key.code == KeyCode::Char('s') && key.modifiers == KeyModifiers::CONTROL));
        let action = {
            let state = self.state.borrow();
            keymap::translate_with_bindings(
                event,
                active,
                state.editor.text(),
                popup,
                &self.input_dispatch.bindings,
            )
        };
        self.input_dispatch.generated_command =
            matches!(action, InputAction::Command(_)) && !typed_submission;
        action
    }

    fn scroll_to_top(&mut self) {
        self.state.borrow().transcript_scroll_activity();
        if !self.state.borrow().run.is_active() {
            if let Err(error) = self.materialize_deferred_history() {
                self.state.borrow_mut().error =
                    Some(format!("could not load older session history: {error}"));
            }
        }
        let mut state = self.state.borrow_mut();
        state.application_viewport_requested = true;
        state.viewport_anchor.set(None);
        let maximum = max_scroll_from_bottom(&state, state.size.0);
        state.scroll_from_bottom.set(maximum);
        state.follow_tail = maximum == 0;
        retain_viewport_anchor(&state);
    }

    /// Semantic OSC133 A/B/C metadata is indexed from host transcript blocks,
    /// not untrusted text. It never enters copy, no-color output or terminal ANSI.
    pub fn scroll_to_prompt(&mut self, forward: bool) {
        self.state.borrow().transcript_scroll_activity();
        if !forward && !self.state.borrow().run.is_active() {
            if let Err(error) = self.materialize_deferred_history() {
                self.state.borrow_mut().error =
                    Some(format!("could not load older session history: {error}"));
                return;
            }
        }
        let mut state = self.state.borrow_mut();
        let chrome = shell_chrome(&state, state.size.0, Instant::now());
        let length = state.rendered_transcript(state.size.0).len();
        let scroll = resolved_scroll_from_bottom(&state, length, chrome.transcript_rows);
        let capacity = transcript_viewport_capacity(chrome.transcript_rows, scroll > 0);
        let top = length.saturating_sub(scroll).saturating_sub(capacity);
        let target = state.prompt_jump_target(top, forward);
        if let Some(target) = target {
            state.application_viewport_requested = true;
            state.viewport_anchor.set(None);
            let capacity = transcript_viewport_capacity(chrome.transcript_rows, true).max(1);
            let maximum = max_scroll_from_bottom(&state, state.size.0);
            let next = length
                .saturating_sub(target.saturating_add(capacity))
                .min(maximum);
            state.scroll_from_bottom.set(next);
            state.follow_tail = next == 0;
            if next == 0 {
                state.jump_to_tail();
            }
            retain_viewport_anchor(&state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn dispatch(shell: &mut InteractiveShell, event: Event) -> InputAction {
        let action = shell.translate_input(Some(event), false);
        match &action {
            InputAction::Edit(edit) => shell.apply_edit(edit.clone()),
            InputAction::Scroll(direction) => shell.scroll(*direction),
            InputAction::ScrollLines(direction) => shell.scroll_lines(*direction),
            InputAction::JumpToTail => shell.jump_to_tail(),
            _ => {}
        }
        action
    }

    fn configure(shell: &mut InteractiveShell, json: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("keybindings.json"), json).unwrap();
        shell.input_dispatch.bindings =
            KeybindingsManager::create(directory.path(), "linux", false);
        directory
    }

    #[test]
    fn product_keybinding_file_reload_unbinding_and_editor_history() {
        let mut shell = InteractiveShell::test_shell();
        let directory = configure(
            &mut shell,
            r#"{
            "tui.editor.undo":"ctrl+z", "tui.editor.redo":"ctrl+r",
            "tui.editor.cursorWordLeft":"ctrl+b", "tui.editor.deleteCharForward":[]
        }"#,
        );
        for character in "alpha beta".chars() {
            dispatch(
                &mut shell,
                key(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        dispatch(&mut shell, key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "alpha");
        dispatch(&mut shell, key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "alpha beta");
        dispatch(&mut shell, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(
            shell.state.borrow().editor.cursor(),
            6,
            "custom word-left wins over default left"
        );
        assert_eq!(
            dispatch(&mut shell, key(KeyCode::Delete, KeyModifiers::NONE)),
            InputAction::Ignore
        );
        std::fs::write(
            directory.path().join("keybindings.json"),
            r#"{"tui.editor.undo":[]}"#,
        )
        .unwrap();
        shell.reload_keybindings();
        assert_eq!(
            dispatch(&mut shell, key(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            InputAction::Ignore
        );
        assert!(shell.hotkeys_text().contains("tui.editor.undo  (unbound)"));
        assert_eq!(
            shell.translate_input(Some(key(KeyCode::Char('d'), KeyModifiers::CONTROL)), false),
            InputAction::Closed
        );
    }

    #[test]
    fn product_visual_cursor_motion_breaks_undo_coalescing() {
        let mut shell = InteractiveShell::test_shell();
        let _directory = configure(&mut shell, r#"{"tui.editor.undo":"ctrl+z"}"#);
        for c in "abc".chars() {
            dispatch(&mut shell, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        dispatch(&mut shell, key(KeyCode::Home, KeyModifiers::NONE));
        dispatch(&mut shell, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(shell.pending(), "xabc");
        dispatch(&mut shell, key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "abc");
        dispatch(&mut shell, key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "");
    }

    #[test]
    fn product_kill_yank_pop_word_jumps_and_focus_reset() {
        let mut shell = InteractiveShell::test_shell();
        dispatch(&mut shell, Event::Paste("alpha beta gamma".into()));
        dispatch(&mut shell, key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "alpha beta ");
        dispatch(&mut shell, key(KeyCode::Left, KeyModifiers::NONE));
        dispatch(&mut shell, key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "alpha  ");
        dispatch(&mut shell, key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(shell.pending(), "alpha beta ");
        dispatch(&mut shell, key(KeyCode::Char('y'), KeyModifiers::ALT));
        assert_eq!(shell.pending(), "alpha gamma ");
        dispatch(&mut shell, key(KeyCode::Home, KeyModifiers::NONE));
        dispatch(&mut shell, key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        dispatch(&mut shell, key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(shell.state.borrow().editor.cursor(), 6);
        dispatch(&mut shell, key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert_eq!(
            dispatch(&mut shell, Event::FocusLost),
            InputAction::FocusLost
        );
        dispatch(&mut shell, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(shell.pending(), "alpha xgamma ");
    }

    #[test]
    fn product_shortcut_command_consumer_preserves_even_a_matching_draft() {
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("fixture", "a", "high");
        shell.set_model_cycle(vec!["a".into(), "b".into()]);
        shell.prefill_editor("/model b".into());
        let action =
            shell.translate_input(Some(key(KeyCode::Char('p'), KeyModifiers::CONTROL)), false);
        let InputAction::Command(text) = action else {
            panic!("model command");
        };
        assert_eq!(shell.consume_command_text(text), "/model b");
        assert_eq!(shell.pending(), "/model b");
        let action = shell.translate_input(Some(key(KeyCode::Enter, KeyModifiers::NONE)), false);
        let InputAction::Command(text) = action else {
            panic!("typed command");
        };
        assert_eq!(shell.consume_command_text(text), "/model b");
        assert!(shell.pending().is_empty());
    }

    #[test]
    fn product_model_cycle_uses_ordered_scope_and_queues_from_last_target() {
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("fixture", "b", "high");
        shell.set_model_cycle(vec!["c".into(), "b".into(), "a".into(), "c".into()]);
        for expected in ["/model a", "/model c", "/model b"] {
            assert_eq!(
                shell.translate_input(Some(key(KeyCode::Char('p'), KeyModifiers::CONTROL)), true),
                InputAction::Command(expected.into())
            );
        }
        assert_eq!(
            shell.translate_input(
                Some(key(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                )),
                true
            ),
            InputAction::Command("/model c".into())
        );
        assert_eq!(
            shell.selected_identity().0,
            "b",
            "dispatch cannot mutate an active run's model"
        );
        let repeated = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
            KeyEventKind::Repeat,
        ));
        assert_eq!(
            shell.translate_input(Some(repeated), true),
            InputAction::Ignore
        );
        let _directory = configure(&mut shell, r#"{"tui.editor.historyPrevious":"ctrl+p"}"#);
        shell.on_prompt_submitted("historic");
        shell.prefill_editor("multiline\ndraft".into());
        shell.state.borrow_mut().editor.set_cursor(3);
        assert_eq!(
            shell.translate_input(Some(key(KeyCode::Char('p'), KeyModifiers::CONTROL)), false),
            InputAction::Ignore
        );
        assert_eq!(shell.pending(), "historic");
    }

    #[test]
    fn product_selector_owns_custom_keys_and_never_edits_hidden_draft() {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("draft".into());
        let _directory = configure(
            &mut shell,
            r#"{"tui.select.down":"ctrl+n","tui.select.confirm":"ctrl+y"}"#,
        );
        shell.open_panel(Panel::SelectList {
            surface: OrdinarySurfaceMetadata::new("Select model"),
            items: vec!["one".into(), "two".into()],
            descriptions: vec![None, None],
            selected: 0,
            filter: String::new(),
            action: PanelAction::SelectModel(vec![]),
        });
        assert_eq!(
            shell.translate_input(Some(key(KeyCode::Char('w'), KeyModifiers::CONTROL)), false),
            InputAction::Ignore
        );
        assert!(shell
            .panel_input(&key(KeyCode::Down, KeyModifiers::NONE))
            .is_none());
        assert_eq!(
            shell.highlighted_panel_index(),
            Some(0),
            "old default is disabled"
        );
        shell.panel_input(&key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(shell.highlighted_panel_index(), Some(1));
        let selected = shell.panel_input(&key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert!(matches!(selected, Some((PanelResult::Confirm(1), _))));
        assert_eq!(shell.pending(), "draft");
    }

    fn viewport_top(shell: &InteractiveShell) -> usize {
        let state = shell.state.borrow();
        let chrome = shell_chrome(&state, state.size.0, Instant::now());
        let length = state.rendered_transcript(state.size.0).len();
        let scroll = resolved_scroll_from_bottom(&state, length, chrome.transcript_rows);
        length
            .saturating_sub(scroll)
            .saturating_sub(transcript_viewport_capacity(
                chrome.transcript_rows,
                scroll > 0,
            ))
    }

    #[test]
    fn product_prompt_jump_reflows_and_preserves_draft_and_history_anchor() {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(60, 16);
        for index in 0..15 {
            shell.on_local_command_submitted(&format!(
                "prompt {index}: {}",
                "wrapped message ".repeat(12)
            ));
        }
        shell.prefill_editor("untouched".into());
        let before = viewport_top(&shell);
        assert_eq!(
            dispatch(&mut shell, key(KeyCode::Up, KeyModifiers::CONTROL)),
            InputAction::Ignore
        );
        let target = viewport_top(&shell);
        assert!(target < before);
        {
            let state = shell.state.borrow();
            assert!(state
                .transcript_cache
                .borrow()
                .prompt_zones
                .prompt_rows()
                .contains(&target));
            assert!(!state.follow_tail);
        }
        shell.notice("new output while reading history");
        assert_eq!(viewport_top(&shell), target);
        shell.set_size(40, 16);
        dispatch(&mut shell, key(KeyCode::Up, KeyModifiers::CONTROL));
        let resized_target = viewport_top(&shell);
        let state = shell.state.borrow();
        assert!(state
            .transcript_cache
            .borrow()
            .prompt_zones
            .prompt_rows()
            .contains(&resized_target));
        assert_eq!(state.editor.text(), "untouched");
        assert!(!state
            .rendered_transcript(40)
            .iter()
            .any(|line| line.contains("\x1b]133;")));
    }

    #[test]
    fn product_click_latest_and_alt_wheel_use_the_semantic_viewport() {
        use crossterm::event::MouseEvent;
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(60, 16);
        for index in 0..30 {
            shell.on_local_command_submitted(&format!("prompt {index}"));
        }
        let action = dispatch(
            &mut shell,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::ALT,
            }),
        );
        assert_eq!(action, InputAction::ScrollLines(-15));
        let row = {
            let state = shell.state.borrow();
            transcript_viewport_capacity(
                shell_chrome(&state, 60, Instant::now()).transcript_rows,
                true,
            ) as u16
        };
        let action = dispatch(
            &mut shell,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert_eq!(action, InputAction::JumpToTail);
        assert!(shell.state.borrow().follow_tail);
    }
}
