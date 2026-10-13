//! Source-bound native session rename; persistence stays with the picker driver.
//!
//! `PickerState::rename` remains the ANSI projection of this request's editor.
//! The host stores one `State` alongside it and uses `begin` / `input`; neither
//! native gestures nor raw editing can touch the parent composer or its ledger.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use octet_tern::wire::{Kind, Node, Props, Span};
use serde_json::json;
use sexy_tui_rs::{TextEditAction, TextEditor};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

use super::super::{
    sanitize_ordinary_surface_cell, session_picker_ordering, tern_controls,
    OrdinarySurfaceLifecycle, Panel, PanelRequest, PickerState, ShellState,
};
use crate::session_store::SessionMeta;

const MAX_BYTES: usize = 4096;

/// Kept for the lifetime of the picker, including between rename requests.
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    epoch: u64,
    request: Option<Request>,
}

#[derive(Clone, Debug)]
struct Request {
    catalogue: String,
    target: SessionMeta,
    editor: TextEditor,
    revision: u64,
    error: Option<&'static str>,
}

/// Lossless content identity of the catalogue and its current ordering. Names,
/// paths, scope and selected-row details cannot change underneath a request.
fn catalogue(picker: &PickerState) -> String {
    let rows = picker
        .active_rows()
        .iter()
        .map(|row| {
            json!({
                "id": row.id, "path": row.path.as_os_str().as_encoded_bytes(),
                "title": row.title, "name": row.name, "tags": row.tags,
                "workspace": row.workspace.as_ref().map(|path| path.as_os_str().as_encoded_bytes()),
                "messages": row.message_count, "modified": format!("{:?}", row.modified),
                "pinned": row.pinned, "archived": row.archived, "trash": row.trashed_at_ms,
                "purge": row.purge_after_ms, "parent": row.forked_from_session_id,
                "entry": row.forked_from_entry_id,
            })
        })
        .collect::<Vec<_>>();
    let source = json!({
        "rows": rows, "order": session_picker_ordering(picker),
        "scope": format!("{:?}", picker.scope), "sort": format!("{:?}", picker.sort),
        "filter": picker.filter, "named": picker.named_only, "paths": picker.show_path,
        "current": picker.current_session_path.as_ref().map(|path| path.as_os_str().as_encoded_bytes()),
    });
    format!("{:x}", Sha256::digest(source.to_string().as_bytes()))
}

fn selected(picker: &PickerState) -> Option<&SessionMeta> {
    picker
        .active_rows()
        .get(*session_picker_ordering(picker).get(picker.selected)?)
}

fn current(picker: &PickerState) -> Option<&Request> {
    let request = picker.rename_state.request.as_ref()?;
    (!picker.confirming_delete && picker.rename.as_deref() == Some(request.editor.text()))
        .then_some(request)
}

fn valid(picker: &PickerState, request: &Request) -> bool {
    request.catalogue == catalogue(picker)
        && selected(picker)
            .is_some_and(|row| row.id == request.target.id && row.path == request.target.path)
}

fn picker(shell: &ShellState) -> Option<&PickerState> {
    // A temporary input request takes precedence over every panel gesture.
    if shell.tool_input_prompt.is_some() {
        return None;
    }
    let Panel::SessionPicker { picker } = shell.panel.as_ref()? else {
        return None;
    };
    current(picker)?;
    Some(picker)
}

fn prefix(shell: &ShellState) -> Option<String> {
    let picker = picker(shell)?;
    let request = current(picker)?;
    let mut hash = Sha256::new();
    let catalogue = catalogue(picker);
    for field in [catalogue.as_bytes(), request.editor.text().as_bytes()] {
        hash.update(field.len().to_le_bytes());
        hash.update(field);
    }
    hash.update(request.editor.cursor().to_le_bytes());
    Some(format!(
        "session-edit.{}.{}.{}.{:x}",
        shell.panel_epoch,
        picker.rename_state.epoch,
        request.revision,
        hash.finalize()
    ))
}

pub(crate) fn focus(shell: &ShellState) -> Option<String> {
    let picker = picker(shell)?;
    valid(picker, current(picker)?).then(|| format!("{}.editor", prefix(shell).unwrap()))
}

/// Enter rename at the selected source identity, never at a reusable ordinal.
pub(crate) fn begin(picker: &mut PickerState) -> bool {
    if picker.rename.is_some() || picker.confirming_delete {
        return false;
    }
    let Some(target) = selected(picker).cloned() else {
        return false;
    };
    let text = target.name.as_deref().unwrap_or(&target.title);
    if text.len() > MAX_BYTES || text.chars().any(char::is_control) {
        failed(
            picker,
            "session label cannot be edited as a bounded single-line name",
        );
        return false;
    }
    let editor = TextEditor::with_text(text);
    picker.rename_state.epoch = picker.rename_state.epoch.saturating_add(1);
    picker.rename = Some(editor.text().to_owned());
    picker.rename_state.request = Some(Request {
        catalogue: catalogue(picker),
        editor,
        target,
        revision: 0,
        error: None,
    });
    true
}

fn failed(picker: &mut PickerState, reason: &'static str) {
    picker.surface.lifecycle = OrdinarySurfaceLifecycle::recoverable_error(
        reason,
        Instant::now() + Duration::from_secs(3),
    );
}

fn finish(picker: &mut PickerState) {
    picker.rename = None;
    picker.rename_state.request = None;
}

pub(crate) fn cancel(picker: &mut PickerState) {
    finish(picker);
    picker.surface.lifecycle =
        OrdinarySurfaceLifecycle::cancelled("rename", Instant::now() + Duration::from_secs(2));
}

/// Returns only an existing driver request; never opens a store or mutates disk.
pub(crate) fn submit(picker: &mut PickerState) -> Option<PanelRequest> {
    let request = current(picker)?;
    if !valid(picker, request) {
        failed(
            picker,
            "session catalogue changed; cancel rename and select the session again",
        );
        return None;
    }
    let name = request.editor.text().trim();
    if name.is_empty() || request.error.is_some() {
        return None;
    }
    let result = PanelRequest::RenameSession {
        id: request.target.id.clone(),
        path: request.target.path.clone(),
        name: name.to_owned(),
    };
    finish(picker);
    Some(result)
}

/// Feed the host's normalized panel event here while `rename.is_some()`. A
/// disabled binding arrives as Null; unrelated picker shortcuts remain consumed.
pub(crate) fn input(picker: &mut PickerState, event: &Event, width: usize) -> Option<PanelRequest> {
    let action = match event {
        Event::Key(key) if crate::tui::keymap::accepts_key_event(key) => {
            if key.modifiers.is_empty() {
                match key.code {
                    KeyCode::Esc => {
                        cancel(picker);
                        return None;
                    }
                    KeyCode::Enter => return submit(picker),
                    KeyCode::Backspace => Some(TextEditAction::Backspace),
                    KeyCode::Delete => Some(TextEditAction::Delete),
                    KeyCode::Left => Some(TextEditAction::Left),
                    KeyCode::Right => Some(TextEditAction::Right),
                    KeyCode::Home => Some(TextEditAction::Home),
                    KeyCode::End => Some(TextEditAction::End),
                    _ => sexy_tui_rs::key_text(key).map(TextEditAction::Char),
                }
            } else if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                sexy_tui_rs::key_text(key).map(TextEditAction::Char)
            } else {
                None
            }
        }
        Event::Paste(text) if text.len() > MAX_BYTES => {
            let request = picker.rename_state.request.as_mut()?;
            request.error = Some("Name exceeds 4 KiB; edit a shorter name");
            request.revision = request.revision.saturating_add(1);
            return None;
        }
        Event::Paste(text) => Some(TextEditAction::Paste(text.clone())),
        _ => None,
    }?;
    apply(picker, action, width);
    None
}

fn apply(picker: &mut PickerState, action: TextEditAction, width: usize) -> Option<()> {
    let request = current(picker)?;
    if !valid(picker, request) {
        return None;
    }
    let inserted = match &action {
        TextEditAction::Char(c) if !c.is_control() => c.len_utf8(),
        TextEditAction::Char(_) => return None,
        TextEditAction::Paste(text) if !text.chars().any(char::is_control) => text.len(),
        TextEditAction::Paste(_) => return None,
        _ => 0,
    };
    if request.editor.text().len().saturating_add(inserted) > MAX_BYTES {
        let request = picker.rename_state.request.as_mut()?;
        request.error = Some("Name exceeds 4 KiB; edit a shorter name");
        request.revision = request.revision.saturating_add(1);
        return None;
    }
    let request = picker.rename_state.request.as_mut()?;
    if request.editor.apply(action, width.max(1)) {
        request.revision = request.revision.saturating_add(1);
        request.error = None;
        picker.rename = Some(request.editor.text().to_owned());
    }
    Some(())
}

/// Exact request, catalogue, text and caret revision are encoded in the node
/// identity. A same-length stale splice, including an ABA edit, is not accepted.
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
    if focus(shell).as_deref() != Some(id) || text.chars().any(char::is_control) {
        return None;
    }
    let picker = picker(shell)?;
    let source = current(picker)?.editor.text();
    if source.encode_utf16().count() != len {
        return None;
    }
    let from = utf16_boundary(source, from)?;
    let to = utf16_boundary(source, to)?;
    if from > to {
        return None;
    }
    // Check size before allocation and validate the post-splice caret before
    // changing any source, editor history, cursor or error state.
    let size = source
        .len()
        .checked_sub(to - from)?
        .checked_add(text.len())?;
    if size > MAX_BYTES {
        let Panel::SessionPicker { picker } = shell.panel.as_mut()? else {
            return None;
        };
        let request = picker.rename_state.request.as_mut()?;
        request.error = Some("Name exceeds 4 KiB; edit a shorter name");
        request.revision = request.revision.saturating_add(1);
        return None;
    }
    let mut edited = source.to_owned();
    edited.replace_range(from..to, text);
    let cursor = utf16_boundary(&edited, cursor)?;
    let mut next = current(picker)?.editor.clone();
    if !next.edit_range(from..to, text, cursor) {
        return None;
    }
    if next.cursor() != cursor {
        return None;
    }
    let Panel::SessionPicker { picker } = shell.panel.as_mut()? else {
        return None;
    };
    let request = picker.rename_state.request.as_mut()?;
    request.editor = next;
    request.revision = request.revision.saturating_add(1);
    request.error = None;
    picker.rename = Some(edited);
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

/// Resolved bindings retain host ownership, including disabled select/cancel.
pub(crate) fn action(shell: &ShellState, id: &str, act: &str) -> Option<&'static str> {
    let prefix = prefix(shell)?;
    let picker = picker(shell)?;
    let binding = match act {
        "cancel" if id == format!("{prefix}.cancel") => "tui.select.cancel",
        "confirm"
            if id == format!("{prefix}.confirm")
                && valid(picker, current(picker)?)
                && current(picker)?.error.is_none()
                && !current(picker)?.editor.text().trim().is_empty() =>
        {
            "tui.select.confirm"
        }
        _ => return None,
    };
    tern_controls::key(shell, binding)?;
    Some(binding)
}

pub(crate) fn node(shell: &ShellState) -> Option<Node> {
    let picker = picker(shell)?;
    let request = current(picker)?;
    let id = prefix(shell)?;
    let safe = |text: &str| sanitize_ordinary_surface_cell(text, shell.theme.unicode());
    let target = &request.target;
    let mut facts = vec![
        json!({"k":"Session", "v":safe(&target.id)}),
        json!({"k":"File", "v":safe(&target.path.display().to_string())}),
        json!({"k":"Messages", "v":target.message_count.to_string()}),
        json!({"k":"Updated", "v":super::super::panel_render::session_age(target.modified, std::time::SystemTime::now())}),
    ];
    if let Some(workspace) = &target.workspace {
        facts.push(json!({"k":"Workspace", "v":safe(&workspace.display().to_string())}));
    }
    if let Some(parent) = &target.forked_from_session_id {
        facts.push(json!({"k":"Forked from", "v":safe(parent)}));
    }
    if !target.tags.is_empty() {
        facts.push(json!({"k":"Tags", "v":safe(&target.tags.join(", "))}));
    }
    let mut children = vec![Node::new(
        format!("{id}.label"),
        Kind::Text,
        Props::new()
            .text(
                "spans",
                vec![Span::styled(
                    safe(target.name.as_deref().unwrap_or(&target.title)),
                    "strong",
                )],
            )
            .set("wrap", "word"),
    )];
    let available = valid(picker, request);
    if available {
        children.push(Node::new(
            format!("{id}.editor"),
            Kind::Editor,
            Props::new()
                .set("text", request.editor.text())
                .set(
                    "cursor",
                    request.editor.text()[..request.editor.cursor()]
                        .encode_utf16()
                        .count(),
                )
                // The host name stays single-line; allow visual wrapping so
                // narrow panes do not display only the caret's last line.
                .set("maxLines", 3)
                .set("placeholder", "Enter a session name"),
        ));
    } else {
        children.push(Node::new(
            format!("{id}.stale"),
            Kind::Text,
            Props::new()
                .set(
                    "text",
                    "Session catalogue changed; cancel rename and select the session again",
                )
                .set("wrap", "word"),
        ));
    }
    if let Some(error) = request.error {
        children.push(Node::new(
            format!("{id}.error"),
            Kind::Text,
            Props::new().set("text", error).set("wrap", "word"),
        ));
    }
    children.push(Node::new(
        format!("{id}.hint"),
        Kind::Text,
        Props::new()
            .set(
                "text",
                format!(
                    "{} · {}",
                    tern_controls::hint(shell, "tui.select.confirm", "renames"),
                    tern_controls::hint(shell, "tui.select.cancel", "cancels"),
                ),
            )
            .set("wrap", "word"),
    ));
    let mut controls = vec![tern_controls::action(
        shell,
        &id,
        "cancel",
        "Cancel",
        "tui.select.cancel",
    )];
    if available && request.error.is_none() && !request.editor.text().trim().is_empty() {
        controls.push(tern_controls::action(
            shell,
            &id,
            "confirm",
            "Rename",
            "tui.select.confirm",
        ));
    }
    children.push(Node::with_children(
        format!("{id}.actions"),
        Kind::Row,
        Props::new().set("gap", "sm").set("wrap", true),
        controls,
    ));
    // Keep the editable value and owned actions before potentially long source
    // facts, so narrow/zoomed sheets do not bury their primary interaction.
    children.push(Node::new(
        format!("{id}.source"),
        Kind::Text,
        Props::new()
            .set("text", safe(&target.title))
            .set("hidden", target.name.is_none())
            .set("wrap", "word"),
    ));
    children.push(Node::new(
        format!("{id}.facts"),
        Kind::Kv,
        Props::new().set("items", facts),
    ));
    Some(Node::with_children(
        format!("{id}.sheet"),
        Kind::Overlay,
        Props::new()
            .role("octet.session.rename")
            .text("head", "Rename session")
            .set("modal", true)
            .set("size", "md"),
        children,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::keybindings::KeybindingsManager;
    use std::{collections::BTreeMap, path::PathBuf, time::SystemTime};

    fn shell() -> ShellState {
        let row = SessionMeta {
            id: "one".into(),
            path: PathBuf::from("/fixture/one.jsonl"),
            title: "Complete original prompt".into(),
            name: Some("a🦀雪".into()),
            tags: vec!["tag".into()],
            pinned: false,
            archived: false,
            trashed_at_ms: None,
            purge_after_ms: None,
            forked_from_session_id: Some("parent".into()),
            forked_from_entry_id: None,
            message_count: 12,
            modified: SystemTime::UNIX_EPOCH,
            workspace: Some("/fixture/workspace".into()),
        };
        let mut shell = ShellState::default();
        let chip = shell
            .ledger
            .attach_pasted_text("original opaque pasted text".into());
        shell.editor.set_text(format!("parent draft {chip}"));
        shell.editor.set_cursor(3);
        let mut picker = PickerState::new(vec![row], None);
        assert!(begin(&mut picker));
        shell.panel = Some(Panel::SessionPicker {
            picker: Box::new(picker),
        });
        shell
    }

    fn picker_mut(shell: &mut ShellState) -> &mut PickerState {
        let Some(Panel::SessionPicker { picker }) = shell.panel.as_mut() else {
            panic!("picker");
        };
        picker
    }

    #[test]
    fn utf16_edits_and_cursor_are_checked_atomically() {
        let mut shell = shell();
        let id = focus(&shell).unwrap();
        let revision = shell.editor.revision();
        // Half a surrogate, reversed/out-of-bounds range, stale length, bad
        // post-edit caret and controls cannot partially replace source.
        for (from, to, text, cursor, len) in [
            (2, 3, "x", 2, 4),
            (3, 1, "x", 2, 4),
            (0, 5, "x", 1, 4),
            (0, 1, "x", 1, 3),
            (0, 1, "x", 2, 4),
            (0, 1, "\n", 1, 4),
            (0, 1, "x", 9, 4),
        ] {
            assert_eq!(edit(&mut shell, &id, from, to, text, cursor, len), None);
            assert_eq!(picker(&shell).unwrap().rename.as_deref(), Some("a🦀雪"));
            assert_eq!(focus(&shell).as_deref(), Some(id.as_str()));
        }
        assert_eq!(edit(&mut shell, &id, 1, 3, "b", 2, 4), Some(()));
        assert_eq!(picker(&shell).unwrap().rename.as_deref(), Some("ab雪"));
        assert_eq!(current(picker(&shell).unwrap()).unwrap().editor.cursor(), 2);
        assert!(shell.editor.text().starts_with("parent draft "));
        assert_eq!(shell.editor.cursor(), 3);
        assert_eq!(shell.editor.revision(), revision);
        let attachments = shell.ledger.take_all();
        assert_eq!(attachments.len(), 1);
        assert!(matches!(&attachments[0].payload,
            crate::tui::composer::AttachmentPayload::PastedText(text) if text == "original opaque pasted text"));
    }

    #[test]
    fn edits_never_round_a_caret_or_range_inside_a_grapheme() {
        let mut shell = shell();
        let id = focus(&shell).unwrap();
        assert_eq!(edit(&mut shell, &id, 0, 4, "e\u{301}", 2, 4), Some(()));
        let id = focus(&shell).unwrap();
        assert_eq!(edit(&mut shell, &id, 0, 1, "x", 2, 2), None);
        assert_eq!(edit(&mut shell, &id, 0, 0, "", 1, 2), None);
        assert_eq!(picker(&shell).unwrap().rename.as_deref(), Some("e\u{301}"));
        assert_eq!(focus(&shell).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn same_length_stale_edits_and_aba_are_fenced_by_source_revision() {
        let mut shell = shell();
        let first = focus(&shell).unwrap();
        assert_eq!(edit(&mut shell, &first, 0, 1, "b", 1, 4), Some(()));
        assert_eq!(edit(&mut shell, &first, 0, 1, "z", 1, 4), None);
        let second = focus(&shell).unwrap();
        assert_eq!(edit(&mut shell, &second, 0, 1, "a", 1, 4), Some(()));
        assert_ne!(focus(&shell).as_deref(), Some(first.as_str()));
        assert_eq!(edit(&mut shell, &first, 0, 1, "z", 1, 4), None);
    }

    #[test]
    fn catalogue_replacement_never_retargets_rename_and_cancel_reopen_is_fenced() {
        let mut shell = shell();
        let old_editor = focus(&shell).unwrap();
        let old_cancel = format!("{}.cancel", prefix(&shell).unwrap());
        picker_mut(&mut shell).rows[0].id = "two".into();
        picker_mut(&mut shell).rows[0].path = "/fixture/two.jsonl".into();
        assert!(focus(&shell).is_none());
        assert_eq!(edit(&mut shell, &old_editor, 0, 1, "b", 1, 4), None);
        assert_eq!(submit(picker_mut(&mut shell)), None);
        assert!(serde_json::to_string(&node(&shell).unwrap())
            .unwrap()
            .contains("catalogue changed"));
        assert_eq!(action(&shell, &old_cancel, "cancel"), None);
        let cancel_id = format!("{}.cancel", prefix(&shell).unwrap());
        assert_eq!(
            action(&shell, &cancel_id, "cancel"),
            Some("tui.select.cancel")
        );
        cancel(picker_mut(&mut shell));
        assert!(begin(picker_mut(&mut shell)));
        assert_eq!(action(&shell, &cancel_id, "cancel"), None);
        assert_eq!(edit(&mut shell, &old_editor, 0, 1, "b", 1, 4), None);
    }

    #[test]
    fn details_only_catalogue_change_also_invalidates_request() {
        let mut shell = shell();
        picker_mut(&mut shell).rows[0].workspace = Some("/different/workspace".into());
        assert!(focus(&shell).is_none());
        assert_eq!(submit(picker_mut(&mut shell)), None);
        assert!(picker_mut(&mut shell).rename.is_some());
    }

    #[test]
    fn over_limit_splice_and_raw_paste_preserve_source_and_report_overflow() {
        let mut shell = shell();
        let id = focus(&shell).unwrap();
        assert_eq!(
            edit(
                &mut shell,
                &id,
                0,
                4,
                &"x".repeat(MAX_BYTES + 1),
                MAX_BYTES + 1,
                4
            ),
            None
        );
        assert_eq!(picker(&shell).unwrap().rename.as_deref(), Some("a🦀雪"));
        assert!(serde_json::to_string(&node(&shell).unwrap())
            .unwrap()
            .contains("4 KiB"));
        assert_eq!(submit(picker_mut(&mut shell)), None);
        assert_eq!(
            input(
                picker_mut(&mut shell),
                &Event::Paste("x".repeat(MAX_BYTES)),
                80
            ),
            None
        );
        assert_eq!(picker(&shell).unwrap().rename.as_deref(), Some("a🦀雪"));
        let id = focus(&shell).unwrap();
        assert_eq!(
            edit(&mut shell, &id, 0, 4, &"x".repeat(MAX_BYTES), MAX_BYTES, 4),
            Some(())
        );
        assert_eq!(
            apply(picker_mut(&mut shell), TextEditAction::Char('雪'), 80),
            None
        );
        assert_eq!(
            picker(&shell).unwrap().rename.as_ref().unwrap().len(),
            MAX_BYTES
        );
    }

    #[test]
    fn raw_edit_and_submit_keep_existing_driver_ownership() {
        let mut shell = shell();
        let old = focus(&shell).unwrap();
        apply(picker_mut(&mut shell), TextEditAction::Backspace, 80);
        apply(picker_mut(&mut shell), TextEditAction::Char('b'), 80);
        assert_eq!(edit(&mut shell, &old, 0, 1, "z", 1, 4), None);
        assert_eq!(
            submit(picker_mut(&mut shell)),
            Some(PanelRequest::RenameSession {
                id: "one".into(),
                path: "/fixture/one.jsonl".into(),
                name: "a🦀b".into(),
            })
        );
        assert!(node(&shell).is_none());
        assert!(shell.editor.text().starts_with("parent draft "));
        assert_eq!(shell.editor.cursor(), 3);
        assert!(!shell.ledger.is_empty());
    }

    #[test]
    fn rename_interaction_precedes_long_facts_and_unnamed_source_is_not_duplicated() {
        let mut shell = shell();
        let picker = picker_mut(&mut shell);
        cancel(picker);
        picker.rows[0].name = None;
        assert!(begin(picker));
        let sheet = node(&shell).unwrap();
        let children = sheet.c.as_ref().unwrap();
        let position = |suffix: &str| {
            children
                .iter()
                .position(|node| node.id.ends_with(suffix))
                .unwrap()
        };
        assert!(position(".editor") < position(".facts"));
        assert!(position(".actions") < position(".facts"));
        assert_eq!(
            children[position(".source")].p.as_ref().unwrap().as_map()["hidden"],
            true
        );
    }

    #[test]
    fn full_source_facts_and_resolved_or_disabled_controls_are_projected() {
        let mut shell = shell();
        shell.native_keys = Some(KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([
                ("tui.select.confirm".into(), vec!["ctrl+y".into()]),
                ("tui.select.cancel".into(), Vec::new()),
            ]),
        ));
        let id = prefix(&shell).unwrap();
        assert_eq!(
            action(&shell, &format!("{id}.confirm"), "confirm"),
            Some("tui.select.confirm")
        );
        assert_eq!(action(&shell, &format!("{id}.cancel"), "cancel"), None);
        assert_eq!(action(&shell, &id, "confirm"), None);
        let rendered = serde_json::to_string(&node(&shell).unwrap()).unwrap();
        for detail in [
            "Complete original prompt",
            "/fixture/one.jsonl",
            "/fixture/workspace",
            "parent",
            "tag",
            "Ctrl+Y renames",
            "cancels unavailable",
        ] {
            assert!(rendered.contains(detail), "missing {detail}");
        }
        assert!(rendered.contains("\"k\":\"editor\""));
        assert!(rendered.contains("\"cursor\":4"));
        assert!(!rendered.contains("parent draft"));
        assert!(!rendered.contains("\"click\":\"cancel\""));
        assert!(!rendered.contains("\"disabled\""));
    }

    #[test]
    fn native_route_remapped_submit_and_cancel_preserve_parent_and_driver_ownership() {
        use super::super::super::InteractiveShell;
        use crossterm::event::KeyEvent;
        use octet_tern::{frame::Incoming, wire::Event as NativeEvent};

        let mut interactive = InteractiveShell::test_shell();
        interactive.input_dispatch.bindings = KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([
                ("tui.select.confirm".into(), vec!["ctrl+y".into()]),
                ("tui.select.cancel".into(), vec!["ctrl+q".into()]),
            ]),
        );
        {
            let mut state = interactive.state.borrow_mut();
            *state = shell();
            state.startup_pending = false;
            state.native_keys = Some(interactive.input_dispatch.bindings.clone());
        }
        interactive.state.native().lock().unwrap().accepting_input = true;
        let handler = interactive.tern_input_handler();
        let editor = focus(&interactive.state.borrow()).unwrap();
        handler(Incoming::Event(NativeEvent::Edit {
            sf: super::super::super::tern::SURFACE.into(),
            id: editor,
            from: 0,
            to: 1,
            text: "b".into(),
            cursor: 1,
            len: 4,
        }));
        assert_eq!(
            picker(&interactive.state.borrow())
                .unwrap()
                .rename
                .as_deref(),
            Some("b🦀雪")
        );
        let control = |act: &str| {
            Incoming::Event(NativeEvent::Action {
                sf: super::super::super::tern::SURFACE.into(),
                id: format!("{}.{act}", prefix(&interactive.state.borrow()).unwrap()),
                act: act.into(),
                value: None,
                mods: None,
            })
        };
        let confirm = control("confirm");
        let cancel = control("cancel");
        assert_eq!(
            handler(cancel.clone()),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL
            )))
        );
        let event = handler(confirm).unwrap();
        assert_eq!(
            event,
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))
        );
        interactive.panel_input(&event);
        assert_eq!(
            interactive.drain_panel_requests(),
            vec![PanelRequest::RenameSession {
                id: "one".into(),
                path: "/fixture/one.jsonl".into(),
                name: "b🦀雪".into(),
            }]
        );
        assert!(handler(cancel).is_none());
        let state = interactive.state.borrow();
        assert_eq!(state.editor.cursor(), 3);
        assert!(state.editor.text().starts_with("parent draft "));
        assert!(!state.ledger.is_empty());
    }

    #[test]
    fn temporary_input_owner_suppresses_rename_controls_and_edits() {
        let mut shell = shell();
        let editor = focus(&shell).unwrap();
        let confirm = format!("{}.confirm", prefix(&shell).unwrap());
        shell.begin_tool_input("Temporary input", false);
        assert_eq!(edit(&mut shell, &editor, 0, 1, "b", 1, 4), None);
        assert_eq!(action(&shell, &confirm, "confirm"), None);
        assert!(node(&shell).is_none());
        shell.end_tool_input();
        assert_eq!(focus(&shell).as_deref(), Some(editor.as_str()));
    }

    #[test]
    fn delete_confirmation_retains_ansi_only_no_positive_pointer_authority() {
        let mut shell = shell();
        cancel(picker_mut(&mut shell));
        picker_mut(&mut shell).confirming_delete = true;
        assert!(node(&shell).is_none());
        assert!(focus(&shell).is_none());
        assert!(super::super::node(&shell).is_none());
        assert!(!super::super::interactive(shell.panel.as_ref().unwrap()));
        assert_eq!(action(&shell, "session-edit.0.confirm", "confirm"), None);
        assert!(!begin(picker_mut(&mut shell)));
    }
}
