//! Foreground leases for cached API 0.4 remote components. No render RPCs.

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use octet_agent::extension_process::{ExtensionProcess, ExtensionRequestFailure, ExtensionResourceOwner};
use octet_agent::extension_remote_ui::{
    ExtensionRemoteUiClosed, ExtensionRemoteUiFrame, ExtensionRemoteUiKey,
    ExtensionRemoteUiKeyKind, ExtensionRemoteUiKeyModifier, ExtensionRemoteUiMouse,
    ExtensionRemoteUiMouseButton, ExtensionRemoteUiMouseKind, ExtensionRemoteUiOperation,
    ExtensionRemoteUiPlacement, ExtensionRemoteUiResize,
};

use crate::tui::extension_components::{ExtensionComponentSurface, MAX_LIVE_COMPONENTS};
use crate::tui::view::InteractiveShell;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    pub(crate) components: Arc<ExtensionComponentSurface>,
    pub(crate) mounts: Vec<MountView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountView {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) placement: ExtensionRemoteUiPlacement,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) mouse_capture: bool,
}

impl Projection {
    pub(crate) fn mount(&self, placement: ExtensionRemoteUiPlacement) -> Option<&MountView> {
        self.mounts.iter().find(|mount| mount.placement == placement)
    }

    pub(crate) fn lines(&self, mount: &MountView, columns: u16, rows: u16) -> &[String] {
        // The renderer can observe a resize before the async input loop does.
        // Never draw a cached snapshot at the wrong geometry during that gap.
        if (mount.columns, mount.rows) != (columns.max(1), rows.max(1)) {
            return &[];
        }
        self.components.slot_lines(Some(&mount.id)).unwrap_or_default()
    }
}

struct Mount {
    process: ExtensionProcess,
    owner: ExtensionResourceOwner,
    surface_id: String,
    view: MountView,
    revision: Option<u64>,
}

#[derive(Default)]
pub(super) struct RemoteUi {
    mounts: Vec<Mount>,
    components: Arc<ExtensionComponentSurface>,
    next_id: u64,
}

type Refusal = (ExtensionRequestFailure, String);

impl RemoteUi {
    pub(super) fn is_empty(&self) -> bool { self.mounts.is_empty() }

    pub(super) fn projection(&self) -> Projection {
        Projection {
            components: self.components.clone(),
            mounts: self.mounts.iter().map(|mount| mount.view.clone()).collect(),
        }
    }

    pub(super) fn apply(
        &mut self,
        process: ExtensionProcess,
        owner: ExtensionResourceOwner,
        operation: ExtensionRemoteUiOperation,
        shell: &mut InteractiveShell,
        terminal_ceded: bool,
    ) -> Result<serde_json::Value, Refusal> {
        operation.validate()?;
        match operation {
            ExtensionRemoteUiOperation::Open { surface_id, title, placement, mouse_capture } => {
                if mouse_capture && placement != ExtensionRemoteUiPlacement::Fullscreen {
                    return Err((ExtensionRequestFailure::InvalidRequest, "mouse capture requires a fullscreen lease".into()));
                }
                if terminal_ceded || shell.remote_ui_input_blocked() {
                    return Err((ExtensionRequestFailure::InvalidRequest,
                        "remote UI conflicts with the current host input or terminal owner".into()));
                }
                if self.mounts.iter().any(|mount| mount.owner == owner && mount.surface_id == surface_id) {
                    return Err((ExtensionRequestFailure::InvalidRequest, "remote UI surface is already open".into()));
                }
                if self.mounts.iter().any(|mount| {
                    matches!(placement, ExtensionRemoteUiPlacement::Fullscreen | ExtensionRemoteUiPlacement::Editor
                        | ExtensionRemoteUiPlacement::Header | ExtensionRemoteUiPlacement::Footer)
                        && placement == mount.view.placement
                }) {
                    return Err((ExtensionRequestFailure::InvalidRequest, "remote UI placement already has an owner".into()));
                }
                if self.mounts.len() >= MAX_LIVE_COMPONENTS {
                    return Err((ExtensionRequestFailure::BoundsExceeded, "at most 16 remote UI mounts are supported".into()));
                }
                let (columns, rows) = shell.terminal_dimensions();
                let (columns, rows) = (columns.max(1), rows.max(1));
                let id = format!("remote.{}", self.next_id);
                self.next_id += 1;
                Arc::make_mut(&mut self.components).register_component(&id)
                    .map_err(|error| (ExtensionRequestFailure::BoundsExceeded, error.to_string()))?;
                self.mounts.push(Mount {
                    process, owner, surface_id,
                    view: MountView { id, title, placement, columns, rows, mouse_capture },
                    revision: None,
                });
                Ok(serde_json::json!({"columns": columns, "rows": rows}))
            }
            ExtensionRemoteUiOperation::Close { surface_id } => {
                let index = self.mounts.iter().position(|mount| mount.owner == owner && mount.surface_id == surface_id)
                    .ok_or_else(|| (ExtensionRequestFailure::InvalidRequest, "remote UI surface is not open for this owner".into()))?;
                // The ordinary close reply commits the backend removal. A
                // ui/closed notification here would remove it before that reply.
                self.remove(index, "closed by extension", false);
                Ok(serde_json::json!({}))
            }
        }
    }

    fn remove(&mut self, index: usize, reason: &str, notify: bool) {
        let mount = self.mounts.remove(index);
        Arc::make_mut(&mut self.components).remove_component(&mount.view.id);
        if notify && mount.process.is_running()
            && mount.process.extension_instance_id() == mount.owner.extension_instance_id
            && mount.process.health_snapshot().generation == mount.owner.process_generation
        {
            let _ = mount.process.notify_remote_ui_closed(ExtensionRemoteUiClosed {
                surface_id: mount.surface_id,
                reason: reason.to_owned(),
            });
        }
    }

    pub(super) fn revoke(&mut self, reason: &str) {
        while !self.mounts.is_empty() { self.remove(self.mounts.len() - 1, reason, true); }
    }

    pub(super) fn reconcile(&mut self, foreground: Option<&str>, size: (u16, u16)) -> Vec<String> {
        let mut errors = Vec::new();
        let (columns, rows) = (size.0.max(1), size.1.max(1));
        let mut index = 0;
        while index < self.mounts.len() {
            let mount = &mut self.mounts[index];
            if foreground != Some(mount.owner.session_id.as_str())
                || !mount.process.is_running()
                || mount.process.extension_instance_id() != mount.owner.extension_instance_id
                || mount.process.health_snapshot().generation != mount.owner.process_generation
                || !mount.process.remote_ui_surface_is_current(&mount.owner, &mount.surface_id)
            {
                self.remove(index, "remote UI foreground owner or process generation ended", true);
                continue;
            }
            if (mount.view.columns, mount.view.rows) != (columns, rows) {
                mount.view.columns = columns;
                mount.view.rows = rows;
                // Invalidate geometry, but retain the accepted revision. A stale
                // post-resize high revision must not poison the next good frame.
                let _ = Arc::make_mut(&mut self.components).store_render(&mount.view.id, columns, Vec::new());
                if let Err(error) = mount.process.notify_remote_ui_resize(ExtensionRemoteUiResize {
                    surface_id: mount.surface_id.clone(), columns, rows,
                }) {
                    let detail = format!("remote UI {} resize delivery failed: {error}", mount.view.title);
                    self.remove(index, "remote UI resize delivery failed", true);
                    errors.push(detail);
                    continue;
                }
            }
            index += 1;
        }
        errors
    }

    pub(super) fn accept_frame(&mut self, process: &ExtensionProcess, frame: ExtensionRemoteUiFrame) -> bool {
        let Some(mount) = self.mounts.iter_mut().find(|mount| {
            mount.owner == frame.resource_owner && mount.surface_id == frame.surface_id
                && process.extension_instance_id() == mount.owner.extension_instance_id
        }) else { return false; };
        // Validate geometry and process ownership BEFORE touching revision.
        if !process.is_running()
            || frame.generation != process.health_snapshot().generation
            || frame.generation != mount.owner.process_generation
            || (frame.columns, frame.rows) != (mount.view.columns, mount.view.rows)
            || mount.revision.is_some_and(|revision| frame.revision <= revision)
        { return false; }
        if Arc::make_mut(&mut self.components)
            .store_render(&mount.view.id, frame.columns, frame.lines).is_err()
        { return false; }
        mount.revision = Some(frame.revision);
        true
    }

    pub(super) fn route_input(&mut self, shell: &mut InteractiveShell, event: &Event) -> bool {
        if shell.remote_ui_input_blocked() { return false; }
        let Some(index) = self.mounts.iter().position(|mount| mount.view.placement == ExtensionRemoteUiPlacement::Fullscreen)
            .or_else(|| self.mounts.iter().position(|mount| mount.view.placement == ExtensionRemoteUiPlacement::Editor))
            else { return false; };
        if matches!(event, Event::Key(key) if matches!(key.code, KeyCode::Char('d' | 'D')) && key.modifiers.contains(KeyModifiers::CONTROL)) {
            // Only a fresh press requests exit; no kind of Ctrl+D is component data.
            return !matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press);
        }
        if let Event::Key(key) = event {
            if matches!(key.code, KeyCode::Char('g' | 'G')) && key.modifiers.contains(KeyModifiers::CONTROL) {
                if key.kind == KeyEventKind::Press { self.remove(index, "returned to octet with Ctrl+G", true); }
                return true;
            }
            if let Some(key) = normalized_key(&self.mounts[index].surface_id, key) {
                if let Err(error) = self.mounts[index].process.notify_remote_ui_key(key) {
                    self.remove(index, "remote UI key delivery failed", true);
                    shell.error(format!("remote UI closed because key delivery failed: {error}"));
                }
            }
            return true;
        }
        if let Event::Mouse(mouse) = event {
            let mount = &self.mounts[index];
            if mount.view.mouse_capture && mouse.column < mount.view.columns && mouse.row < mount.view.rows {
                if let Some(mouse) = normalized_mouse(&mount.surface_id, mouse) {
                    if let Err(error) = mount.process.notify_remote_ui_mouse(mouse) {
                        self.remove(index, "remote UI mouse delivery failed", true);
                        shell.error(format!("remote UI closed because mouse delivery failed: {error}"));
                    }
                }
            }
            return true;
        }
        // Unsupported component gestures never mutate the hidden composer.
        matches!(event, Event::Paste(_))
    }
}

pub(crate) async fn notified(wake: &Option<Arc<tokio::sync::Notify>>) {
    match wake {
        Some(wake) => wake.notified().await,
        None => std::future::pending::<()>().await,
    }
}

pub(crate) fn normalized_key(surface_id: &str, key: &crossterm::event::KeyEvent) -> Option<ExtensionRemoteUiKey> {
    let name = match key.code {
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(character) if !character.is_control() => character.to_string(),
        KeyCode::Enter => "Enter".into(), KeyCode::Esc => "Escape".into(),
        KeyCode::Tab | KeyCode::BackTab => "Tab".into(),
        KeyCode::Backspace => "Backspace".into(), KeyCode::Delete => "Delete".into(),
        KeyCode::Insert => "Insert".into(), KeyCode::Home => "Home".into(), KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(), KeyCode::PageDown => "PageDown".into(),
        KeyCode::Up => "ArrowUp".into(), KeyCode::Down => "ArrowDown".into(),
        KeyCode::Left => "ArrowLeft".into(), KeyCode::Right => "ArrowRight".into(),
        KeyCode::F(number) => format!("F{number}"),
        _ => return None,
    };
    let mut modifiers = normalized_modifiers(key.modifiers);
    if key.code == KeyCode::BackTab && !modifiers.contains(&ExtensionRemoteUiKeyModifier::Shift) {
        modifiers.insert(0, ExtensionRemoteUiKeyModifier::Shift);
    }
    Some(ExtensionRemoteUiKey {
        surface_id: surface_id.into(), key: name,
        kind: match key.kind { KeyEventKind::Press => ExtensionRemoteUiKeyKind::Press,
            KeyEventKind::Repeat => ExtensionRemoteUiKeyKind::Repeat, KeyEventKind::Release => ExtensionRemoteUiKeyKind::Release },
        modifiers,
    })
}

fn normalized_modifiers(flags: KeyModifiers) -> Vec<ExtensionRemoteUiKeyModifier> {
    let mut modifiers = Vec::new();
    for (flag, modifier) in [
        (KeyModifiers::SHIFT, ExtensionRemoteUiKeyModifier::Shift),
        (KeyModifiers::ALT, ExtensionRemoteUiKeyModifier::Alt),
        (KeyModifiers::CONTROL, ExtensionRemoteUiKeyModifier::Control),
        (KeyModifiers::SUPER, ExtensionRemoteUiKeyModifier::Super),
    ] {
        if flags.contains(flag) { modifiers.push(modifier); }
    }
    modifiers
}

pub(crate) fn normalized_mouse(surface_id: &str, mouse: &crossterm::event::MouseEvent) -> Option<ExtensionRemoteUiMouse> {
    use ExtensionRemoteUiMouseButton as Button;
    use ExtensionRemoteUiMouseKind as Kind;
    let button = |button| match button {
        MouseButton::Left => Button::Left, MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Right,
    };
    let (kind, button, wheel_delta) = match mouse.kind {
        MouseEventKind::Down(value) => (Kind::Press, button(value), 0),
        MouseEventKind::Up(value) => (Kind::Release, button(value), 0),
        MouseEventKind::Drag(value) => (Kind::Drag, button(value), 0),
        MouseEventKind::Moved => (Kind::Move, Button::None, 0),
        MouseEventKind::ScrollDown => (Kind::Wheel, Button::None, 1),
        MouseEventKind::ScrollUp => (Kind::Wheel, Button::None, -1),
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return None,
    };
    Some(ExtensionRemoteUiMouse {
        surface_id: surface_id.into(), kind, button,
        // Crossterm already converts SGR mouse coordinates to zero-based cells.
        x: mouse.column, y: mouse.row, wheel_delta,
        modifiers: normalized_modifiers(mouse.modifiers),
    })
}

#[cfg(all(test, unix))]
mod tests;
