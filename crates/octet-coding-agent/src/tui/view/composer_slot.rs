//! The one composer slot and its fixed input order.
//!
//! Octet's renderer always draws the transcript, chrome, status and slash
//! popup. The composer slot hosts either the native editor or an extension
//! editor (a Pi custom editor projection) and this module is the single place
//! that answers three questions: which editor occupies the slot, which keys the
//! host reserves before that editor may see them, and which text decisions the
//! slot's owner has not observed yet.
//!
//! The order is fixed: host-reserved keys, then Octet's open slash menu, then
//! the extension-terminal-input consumers (API `0.4` `ui/input`), then the
//! slot's editor. Editing keys reach the extension component as keys so its own
//! paste and undo state stays intact; the host reaches it with a bounded text
//! replacement only when the host itself decided the draft (completion, clear).

use crossterm::event::{Event, KeyEventKind};

use super::*;
use crate::extensions::remote_ui::MountView;
use crate::tui::keymap::InputAction;

/// One host decision for the slot editor's text. `None` clears the draft.
/// A component answers with an ordinary `composer/set` checkpoint, so the host
/// never guesses the editor's cursor, pastes or undo stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerSlotWrite {
    pub(crate) surface_id: String,
    pub(crate) mount_id: String,
    pub(crate) text: Option<String>,
    pub(crate) paste: bool,
}

/// Key-bound host grammar that must never be forwarded to a slot editor. Editor
/// bindings (`tui.editor.*`) are deliberately absent: the slot editor resolves
/// its own movement and editing keys from the same resolved key map.
const HOST_RESERVED_EDITOR_KEYS: &[&str] = &[
    "app.clear",
    "app.interrupt",
    "app.exit",
    "tui.input.submit",
    "app.message.followUp",
    "app.message.dequeue",
    "app.model.select",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.thinking.cycle",
    "app.tools.expand",
    "app.thinking.toggle",
    "app.message.copy",
    "app.clipboard.pasteImage",
    "app.session.new",
    "app.session.fork",
    "app.session.resume",
    "tui.altScreen.pageUp",
    "tui.altScreen.pageDown",
    "tui.altScreen.previousPrompt",
    "tui.altScreen.nextPrompt",
    "tui.altScreen.halfPageUp",
    "tui.altScreen.halfPageDown",
    "tui.altScreen.lineUp",
    "tui.altScreen.lineDown",
    "tui.altScreen.top",
    "tui.altScreen.bottom",
    "tui.altScreen.search",
    "tui.altScreen.searchNext",
    "tui.altScreen.searchPrevious",
    "tui.altScreen.searchClose",
    "tui.editor.jumpForward",
    "tui.editor.jumpBackward",
];

impl InteractiveShell {
    /// The editor that currently occupies the composer slot.
    pub(crate) fn composer_slot_mount(&self) -> Option<MountView> {
        self.state
            .borrow()
            .extension_ui
            .remote
            .mount(octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Editor)
            .cloned()
    }

    /// Whether a custom editor, rather than the native editor, draws the slot.
    pub(crate) fn extension_editor_owns_composer(&self) -> bool {
        self.composer_slot_mount().is_some()
    }

    /// Whether the slot's editor, not the host grammar, owns this key.
    ///
    /// Host-reserved keys keep their native owner so submit, follow-up, clear,
    /// close, steering and every explicit user override cannot be swallowed by
    /// an extension component. An open Octet slash menu keeps its own keys. All
    /// remaining keys are editor data, including keys the host key map does not
    /// bind at all: an unimplemented binding never steals a key from the slot.
    pub(crate) fn slot_editor_takes_key(&self, event: &Event, active: bool) -> bool {
        let Event::Key(key) = event else {
            return matches!(event, Event::Paste(_));
        };
        if key.kind == KeyEventKind::Release {
            // Release data belongs to the component; the host grammar is
            // one-shot and never a held key.
            return true;
        }
        if crate::tui::keymap::is_close_key(key) {
            return false;
        }
        let action = {
            let state = self.state.borrow();
            crate::tui::keymap::translate_with_bindings(
                Some(event.clone()),
                active,
                state.editor.text(),
                !state.slash_popup_dismissed
                    && !super::input_overlays::input_slash_suggestions(&state).is_empty(),
                &self.input_dispatch.bindings,
            )
        };
        if matches!(action, InputAction::Edit(_)) {
            // Explicit editor overrides beat default app bindings, exactly as
            // in the native translator (except coordinated close/clear).
            return true;
        }
        matches!(action, InputAction::Ignore)
            && !HOST_RESERVED_EDITOR_KEYS
                .iter()
                .any(|id| self.input_dispatch.bindings.matches(key, id))
    }

    /// Record a host decision for the slot editor's text. The extension host
    /// delivers it on its next drain; without a slot editor the native editor is
    /// the only owner and this is a no-op.
    pub(crate) fn set_composer_slot_text(&mut self, text: impl Into<String>) {
        let Some(mount) = self.composer_slot_mount() else {
            return;
        };
        let mut state = self.state.borrow_mut();
        state.pending_composer_write = Some(ComposerSlotWrite {
            surface_id: mount.surface_id,
            mount_id: mount.id,
            text: Some(text.into()),
            paste: false,
        });
    }

    /// Preserve the mounted editor's native paste/undo policy for clipboard
    /// text. Delivery is fenced and checkpointed like terminal paste.
    pub(crate) fn paste_composer_slot(&mut self, text: String) -> bool {
        let Some(mount) = self.composer_slot_mount() else {
            return false;
        };
        self.state.borrow_mut().pending_composer_write = Some(ComposerSlotWrite {
            surface_id: mount.surface_id,
            mount_id: mount.id,
            text: Some(text),
            paste: true,
        });
        true
    }

    /// Record a host clear for the slot editor's draft.
    pub(crate) fn clear_composer_slot(&mut self) {
        let Some(mount) = self.composer_slot_mount() else {
            return;
        };
        let mut state = self.state.borrow_mut();
        state.pending_composer_write = Some(ComposerSlotWrite {
            surface_id: mount.surface_id,
            mount_id: mount.id,
            text: None,
            paste: false,
        });
    }

    /// Re-offer the native draft mirror to the slot editor after a host restore.
    /// One pending write wins: a refusal therefore replaces a pending clear
    /// instead of racing it, and no component ever loses its genuine draft.
    pub(crate) fn sync_composer_slot_draft(&mut self) {
        let text = self.state.borrow().editor.text().to_owned();
        self.set_composer_slot_text(text);
    }

    /// Take the one pending host decision, if the host made one since the last
    /// drain. The extension host owns the wire delivery.
    pub(crate) fn take_composer_slot_write(&self) -> Option<ComposerSlotWrite> {
        self.state.borrow_mut().pending_composer_write.take()
    }
}
