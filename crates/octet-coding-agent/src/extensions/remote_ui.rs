//! Foreground leases for cached API 0.4 remote components. No render RPCs.

use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use octet_agent::extension_process::{
    ExtensionEditorCheckpoint, ExtensionProcess, ExtensionRequestFailure, ExtensionResourceOwner,
};
use octet_agent::extension_remote_ui::{
    ExtensionRemoteUiClosed, ExtensionRemoteUiEditorInput, ExtensionRemoteUiFrame,
    ExtensionRemoteUiKey, ExtensionRemoteUiKeyKind, ExtensionRemoteUiKeyModifier,
    ExtensionRemoteUiMouse, ExtensionRemoteUiMouseButton, ExtensionRemoteUiMouseKind,
    ExtensionRemoteUiOperation, ExtensionRemoteUiPlacement, ExtensionRemoteUiResize,
    MAX_EXTENSION_REMOTE_UI_REVISION,
};

use crate::tui::extension_components::{ExtensionComponentSurface, MAX_LIVE_COMPONENTS};
use crate::tui::view::InteractiveShell;
use octet_agent::extension_remote_ui::ExtensionRemoteUiEditorText;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    pub(crate) components: Arc<ExtensionComponentSurface>,
    pub(crate) mounts: Vec<MountView>,
    pub(crate) chrome: Option<ChromeState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeState {
    pub(crate) title: Option<String>,
    pub(crate) working: crate::tui::view::ShellExtensionWorking,
    pub(crate) hidden_thinking_label: Option<String>,
}

struct ChromeLease {
    process: ExtensionProcess,
    owner: ExtensionResourceOwner,
    state: ChromeState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountView {
    pub(crate) id: String,
    /// Extension-local surface identifier this mount was opened with.
    pub(crate) surface_id: String,
    pub(crate) title: String,
    pub(crate) placement: ExtensionRemoteUiPlacement,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) mouse_capture: bool,
    pub(crate) native_editor: bool,
}

impl Projection {
    pub(crate) fn mount(&self, placement: ExtensionRemoteUiPlacement) -> Option<&MountView> {
        self.mounts
            .iter()
            .find(|mount| mount.placement == placement)
    }

    pub(crate) fn lines(&self, mount: &MountView, columns: u16, rows: u16) -> &[String] {
        // The renderer can observe a resize before the async input loop does.
        // Never draw a cached snapshot at the wrong geometry during that gap.
        if (mount.columns, mount.rows) != (columns.max(1), rows.max(1)) {
            return &[];
        }
        self.components
            .slot_lines(Some(&mount.id))
            .unwrap_or_default()
    }
}

struct Mount {
    process: ExtensionProcess,
    owner: ExtensionResourceOwner,
    surface_id: String,
    view: MountView,
    revision: Option<u64>,
    editor: Option<EditorRecovery>,
    editors: crate::native_editor::EditorService,
}

/// The hidden native composer is the recovery point, not the last painted frame.
/// A mount's clock is independent of render revisions and never survives removal.
struct EditorRecovery {
    input_revision: u64,
    acknowledged_input_revision: u64,
    checkpoint_revision: u64,
    /// Native draft revision the component's next checkpoint answers.
    composer_revision: u64,
    // The host cleared the slot: an admitted command may move focus into a
    // panel before its editor answers, so permit only that acknowledged empty
    // clear checkpoint and nothing else while focus is elsewhere.
    expected_clear: bool,
}

impl EditorRecovery {
    fn notice(&self) -> Option<String> {
        let pending = self.input_revision - self.acknowledged_input_revision;
        (pending != 0).then(|| format!(
            "Custom editor closed; {pending} input events were not acknowledged and may contain edits that were not recovered. The native draft retains the last host checkpoint."
        ))
    }
}

#[derive(Default)]
pub(super) struct RemoteUi {
    mounts: Vec<Mount>,
    components: Arc<ExtensionComponentSurface>,
    next_id: u64,
    chrome: Option<ChromeLease>,
    // Terminal input waiting for a completed editor checkpoint. These remain
    // events for the ordinary native dispatcher, never a second submit path.
    pending_input: VecDeque<(String, Event)>,
    replaying_input: bool,
}

type Refusal = (ExtensionRequestFailure, String);

/// Opt-in lifecycle trace for correlating remote UI surfaces with their
/// extension-process generation. Disabled unless explicitly enabled by the
/// operator so normal interactive sessions do not write diagnostic output.
fn trace_remote_ui_lifecycle(event: &str, owner: &ExtensionResourceOwner, detail: &str) {
    if std::env::var_os("OCTET_TRACE_REMOTE_UI_LIFECYCLE").is_none() {
        return;
    }
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    eprintln!(
        "remote-ui-lifecycle ts_ms={timestamp_ms} event={event} session={} instance={} generation={} {detail}",
        owner.session_id,
        owner.extension_instance_id,
        owner.process_generation,
    );
}

/// Metadata only: count before allocating the response tree and count encoded
/// bytes without building a second potentially oversized serialized buffer.
fn validate_theme_list(themes: &[serde_json::Value]) -> Result<(), Refusal> {
    const MAX_THEMES: usize = 128;
    const MAX_THEME_LIST_BYTES: usize = 512 * 1024;
    if themes.len() > MAX_THEMES {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            "theme catalog exceeds 128 metadata records".into(),
        ));
    }
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_THEME_LIST_BYTES {
                return Err(std::io::Error::other("theme metadata exceeds frame budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), themes).map_err(|_| {
        (
            ExtensionRequestFailure::BoundsExceeded,
            "theme metadata exceeds the bounded physical frame budget".into(),
        )
    })
}

impl RemoteUi {
    pub(super) fn composer_editor_identity(
        &self,
    ) -> Option<(String, crate::native_editor::ComposerEditorIdentity)> {
        let mount = self.mounts.iter().find(|mount| mount.editor.is_some())?;
        Some((mount.view.id.clone(), mount.editors.composer_identity()?))
    }

    pub(super) fn composer_editor_matches(
        &self,
        snapshot: &crate::tui::view::ShellEditorSnapshot,
    ) -> bool {
        let Some(mount) = self.mounts.iter().find(|mount| mount.editor.is_some()) else {
            return true;
        };
        mount
            .editors
            .composer_snapshot()
            .is_some_and(|native| native.text == snapshot.text && native.cursor == snapshot.cursor)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.mounts.is_empty() && self.chrome.is_none()
    }

    pub(super) fn projection(&self) -> Projection {
        Projection {
            components: self.components.clone(),
            mounts: self.mounts.iter().map(|mount| mount.view.clone()).collect(),
            chrome: self.chrome.as_ref().map(|lease| lease.state.clone()),
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
            ExtensionRemoteUiOperation::Chrome { chrome } => {
                use octet_agent::extension_remote_ui::ExtensionRemoteUiChrome as Chrome;
                match chrome {
                    Chrome::Editor {
                        surface_id,
                        mount_id,
                        editor_id,
                        operation,
                    } => {
                        let surface_current =
                            process.remote_ui_surface_is_current(&owner, &surface_id);
                        if terminal_ceded || !surface_current {
                            trace_remote_ui_lifecycle(
                                "editor_reject",
                                &owner,
                                &format!(
                                    "surface={} terminal_ceded={} surface_current={}",
                                    surface_id, terminal_ceded, surface_current
                                ),
                            );
                            return Err((
                                ExtensionRequestFailure::NotForegroundOwner,
                                "native editor terminal or surface lease retired".into(),
                            ));
                        }
                        let Some(mount) = self
                            .mounts
                            .iter_mut()
                            .find(|mount| mount.owner == owner && mount.surface_id == surface_id)
                        else {
                            trace_remote_ui_lifecycle(
                                "editor_reject",
                                &owner,
                                &format!("surface={surface_id} reason=mount_not_open_for_owner"),
                            );
                            return Err((
                                ExtensionRequestFailure::NotForegroundOwner,
                                "native editor surface retired".into(),
                            ));
                        };
                        if mount.view.placement == ExtensionRemoteUiPlacement::Editor {
                            if mount_id.as_deref() != Some(mount.view.id.as_str()) {
                                return Err((
                                    ExtensionRequestFailure::InvalidRequest,
                                    "native editor mount fence mismatch".into(),
                                ));
                            }
                        } else if mount_id.is_some() {
                            return Err((
                                ExtensionRequestFailure::InvalidRequest,
                                "composer mount fence on another placement".into(),
                            ));
                        }
                        let reply = mount.editors.request(editor_id.as_deref(), operation)?;
                        mount.view.native_editor = mount.editors.composer_identity().is_some();
                        return Ok(reply);
                    }
                    Chrome::Get => {
                        return Ok(serde_json::json!({
                            "tools_expanded": shell.verbose_tools(),
                            "theme": shell.extension_theme_get(None),
                        }));
                    }
                    Chrome::ThemeGet { name } => {
                        return Ok(serde_json::json!({
                            "theme": shell.extension_theme_get(name.as_deref()),
                        }));
                    }
                    Chrome::ThemeList {} => {
                        let themes = shell.extension_theme_list();
                        validate_theme_list(&themes)?;
                        return Ok(serde_json::json!({"themes": themes}));
                    }
                    Chrome::ThemeSet { name } => {
                        if terminal_ceded {
                            return Err((
                                ExtensionRequestFailure::NotForegroundOwner,
                                "terminal is ceded".into(),
                            ));
                        }
                        return Ok(match shell.extension_theme_set(&name) {
                            Ok(theme) => {
                                // Native selection and the palette replacement are
                                // committed before the synchronous receipt. This
                                // sends only observations, never factory RPCs.
                                self.publish_theme(&process, &owner, &theme);
                                serde_json::json!({"success": true, "theme": theme})
                            }
                            Err(error) => serde_json::json!({
                                "success": false,
                                "error": error,
                                "theme": shell.extension_theme_get(None),
                            }),
                        });
                    }
                    Chrome::ToolsExpanded { expanded } => shell.set_verbose_tools(expanded),
                    Chrome::DesktopNotification { title, body } => {
                        if terminal_ceded {
                            return Err((
                                ExtensionRequestFailure::NotForegroundOwner,
                                "terminal is ceded".into(),
                            ));
                        }
                        shell.queue_desktop_notification(title, body)?;
                    }
                    change => {
                        if self
                            .chrome
                            .as_ref()
                            .is_none_or(|lease| lease.owner != owner)
                        {
                            self.chrome = Some(ChromeLease {
                                process,
                                owner,
                                state: ChromeState::default(),
                            });
                        }
                        let state = &mut self.chrome.as_mut().expect("chrome lease").state;
                        match change {
                            Chrome::Title { title } => state.title = Some(title),
                            Chrome::WorkingMessage { message } => state.working.message = message,
                            Chrome::WorkingVisible { visible } => {
                                state.working.visible = Some(visible)
                            }
                            Chrome::WorkingIndicator {
                                frames,
                                interval_ms,
                            } => {
                                state.working.frames = frames;
                                state.working.interval_ms = interval_ms;
                            }
                            Chrome::HiddenThinking { label } => state.hidden_thinking_label = label,
                            Chrome::Editor { .. }
                            | Chrome::Get
                            | Chrome::ThemeGet { .. }
                            | Chrome::ThemeList {}
                            | Chrome::ThemeSet { .. }
                            | Chrome::ToolsExpanded { .. }
                            | Chrome::DesktopNotification { .. } => unreachable!(),
                        }
                    }
                }
                Ok(serde_json::json!({"tools_expanded": shell.verbose_tools()}))
            }
            ExtensionRemoteUiOperation::Open {
                surface_id,
                title,
                placement,
                mouse_capture,
            } => {
                if mouse_capture && placement != ExtensionRemoteUiPlacement::Fullscreen {
                    return Err((
                        ExtensionRequestFailure::InvalidRequest,
                        "mouse capture requires a fullscreen lease".into(),
                    ));
                }
                if terminal_ceded || shell.remote_ui_open_blocked(placement) {
                    return Err((
                        ExtensionRequestFailure::InvalidRequest,
                        "remote UI conflicts with the current host input or terminal owner".into(),
                    ));
                }
                if self
                    .mounts
                    .iter()
                    .any(|mount| mount.owner == owner && mount.surface_id == surface_id)
                {
                    return Err((
                        ExtensionRequestFailure::InvalidRequest,
                        "remote UI surface is already open".into(),
                    ));
                }
                if self.mounts.iter().any(|mount| {
                    matches!(
                        placement,
                        ExtensionRemoteUiPlacement::Fullscreen
                            | ExtensionRemoteUiPlacement::Editor
                            | ExtensionRemoteUiPlacement::Header
                            | ExtensionRemoteUiPlacement::Footer
                    ) && placement == mount.view.placement
                }) {
                    return Err((
                        ExtensionRequestFailure::InvalidRequest,
                        "remote UI placement already has an owner".into(),
                    ));
                }
                if self.mounts.len() >= MAX_LIVE_COMPONENTS {
                    return Err((
                        ExtensionRequestFailure::BoundsExceeded,
                        "at most 16 remote UI mounts are supported".into(),
                    ));
                }
                let (columns, rows) = shell.terminal_dimensions();
                let (columns, rows) = (columns.max(1), rows.max(1));
                let id = format!("remote.{}", self.next_id);
                self.next_id += 1;
                Arc::make_mut(&mut self.components)
                    .register_component(&id)
                    .map_err(|error| {
                        (ExtensionRequestFailure::BoundsExceeded, error.to_string())
                    })?;
                let editor =
                    (placement == ExtensionRemoteUiPlacement::Editor).then(|| EditorRecovery {
                        input_revision: 0,
                        acknowledged_input_revision: 0,
                        checkpoint_revision: 0,
                        composer_revision: shell.extension_editor_snapshot().revision,
                        expected_clear: false,
                    });
                let mut result = serde_json::json!({"columns": columns, "rows": rows});
                if editor.is_some() {
                    result["editor_mount_id"] = id.clone().into();
                }
                trace_remote_ui_lifecycle(
                    "open",
                    &owner,
                    &format!(
                        "surface={} mount={} placement={placement:?}",
                        surface_id, id
                    ),
                );
                self.mounts.push(Mount {
                    editors: crate::native_editor::EditorService::for_mount(id.clone()),
                    process,
                    owner: owner.clone(),
                    surface_id: surface_id.clone(),
                    view: MountView {
                        id,
                        surface_id: surface_id.clone(),
                        title,
                        placement,
                        columns,
                        rows,
                        native_editor: false,
                        mouse_capture,
                    },
                    revision: None,
                    editor,
                });
                Ok(result)
            }
            ExtensionRemoteUiOperation::Close { surface_id } => {
                let Some(index) = self
                    .mounts
                    .iter()
                    .position(|mount| mount.owner == owner && mount.surface_id == surface_id)
                else {
                    trace_remote_ui_lifecycle(
                        "close_reject",
                        &owner,
                        &format!("surface={surface_id} reason=not_open_for_owner"),
                    );
                    return Err((
                        ExtensionRequestFailure::InvalidRequest,
                        "remote UI surface is not open for this owner".into(),
                    ));
                };
                // The ordinary close reply commits the backend removal. A
                // ui/closed notification here would remove it before that reply.
                if let Some(notice) = self.remove(index, "closed by extension", false) {
                    shell.notice(notice);
                }
                Ok(serde_json::json!({}))
            }
        }
    }

    fn publish_theme(
        &self,
        requesting: &ExtensionProcess,
        owner: &ExtensionResourceOwner,
        theme: &serde_json::Value,
    ) {
        let publish = |process: &ExtensionProcess| {
            if process.descriptor().manifest.runtime.sharing
                != octet_agent::extension_process::ExtensionRuntimeSharing::Isolated
            {
                return;
            }
            let mut state = process.current_context().host;
            state.theme = Some(theme.clone());
            process.set_host_state(state);
        };
        publish(requesting);
        for mount in &self.mounts {
            if mount.owner.session_id == owner.session_id
                && mount.process.is_running()
                && mount.process.extension_instance_id() == mount.owner.extension_instance_id
                && mount.process.health_snapshot().generation == mount.owner.process_generation
            {
                publish(&mount.process);
            }
        }
    }

    fn remove(&mut self, index: usize, reason: &str, notify: bool) -> Option<String> {
        let mount = self.mounts.remove(index);
        trace_remote_ui_lifecycle(
            "retire",
            &mount.owner,
            &format!(
                "surface={} mount={} placement={:?} reason={}",
                mount.surface_id, mount.view.id, mount.view.placement, reason
            ),
        );
        self.pending_input.retain(|(id, _)| id != &mount.view.id);
        Arc::make_mut(&mut self.components).remove_component(&mount.view.id);
        if notify
            && mount.process.is_running()
            && mount.process.extension_instance_id() == mount.owner.extension_instance_id
            && mount.process.health_snapshot().generation == mount.owner.process_generation
        {
            let _ = mount
                .process
                .notify_remote_ui_closed(ExtensionRemoteUiClosed {
                    surface_id: mount.surface_id,
                    reason: reason.to_owned(),
                });
        }
        mount.editor.as_ref().and_then(EditorRecovery::notice)
    }

    pub(super) fn revoke(&mut self, reason: &str) -> Vec<String> {
        self.chrome = None;
        let mut notices = Vec::new();
        while !self.mounts.is_empty() {
            notices.extend(self.remove(self.mounts.len() - 1, reason, true));
        }
        notices
    }

    pub(super) fn editor_owns_composer(&self) -> bool {
        self.editor_mount().is_some()
    }

    /// Deliver one host decision for the composer slot to the mounted editor.
    /// The component applies the text and answers with a `composer/set`
    /// checkpoint, so undo/paste state never leaves its owner on a refusal.
    /// The mount's expected native revision advances with the host decision, so
    /// the component's answering checkpoint is not mistaken for a stale draft.
    pub(super) fn deliver_editor_text(
        &mut self,
        write: &crate::tui::view::ComposerSlotWrite,
        shell: &mut InteractiveShell,
    ) {
        let Some(index) = self.mounts.iter().position(|mount| {
            mount.editor.is_some()
                && mount.view.id == write.mount_id
                && mount.view.surface_id == write.surface_id
        }) else {
            return;
        };
        let text = write.text.clone().unwrap_or_default();
        let clears = write.text.is_none();
        let editor_input = if write.paste {
            let mount = &mut self.mounts[index];
            let editor = mount.editor.as_mut().expect("editor mount");
            if editor.input_revision == MAX_EXTENSION_REMOTE_UI_REVISION {
                if let Some(notice) = self.remove(index, "editor input revision exhausted", true) {
                    shell.notice(notice);
                }
                return;
            }
            editor.input_revision += 1;
            Some(ExtensionRemoteUiEditorInput {
                mount_id: mount.view.id.clone(),
                input_revision: editor.input_revision,
            })
        } else {
            None
        };
        let mirror = shell.extension_editor_snapshot();
        let cursor = if mirror.text == text {
            mirror.cursor
        } else {
            text.len()
        };
        let native_changed = if write.paste {
            None
        } else {
            match self.mounts[index].editors.replace_composer(&text, cursor) {
                Ok(changed) => changed,
                Err((_, detail)) => {
                    // The host projected its decision into the recovery mirror
                    // before delivery. A native refusal must restore that mirror
                    // too, before another action can admit the rejected draft.
                    let native = self.mounts[index]
                        .editors
                        .composer_snapshot()
                        .expect("bound native composer");
                    shell.extension_set_editor_at(native.text, Some(native.cursor));
                    self.mounts[index]
                        .editor
                        .as_mut()
                        .expect("editor mount")
                        .composer_revision = shell.extension_editor_revision();
                    shell.error(detail);
                    return;
                }
            }
        };
        let result =
            self.mounts[index]
                .process
                .notify_remote_ui_editor_text(ExtensionRemoteUiEditorText {
                    surface_id: self.mounts[index].view.surface_id.clone(),
                    text,
                    paste: write.paste,
                    native_changed,
                    editor_input,
                });
        match result {
            Ok(()) => {
                if let Some(editor) = self.mounts[index].editor.as_mut() {
                    editor.composer_revision = shell.extension_editor_snapshot().revision;
                    editor.expected_clear = clears;
                }
            }
            Err(error) => {
                if let Some(notice) = self.remove(index, "remote UI editor write failed", true) {
                    shell.notice(notice);
                }
                shell.error(format!(
                    "remote UI editor closed because the slot write failed: {error}"
                ));
            }
        }
    }

    /// The live composer-slot editor mount, if one owns the slot.
    pub(super) fn editor_mount(&self) -> Option<&MountView> {
        self.mounts
            .iter()
            .find(|mount| mount.editor.is_some())
            .map(|mount| &mount.view)
    }

    pub(super) fn checkpoint_editor(
        &mut self,
        owner: &ExtensionResourceOwner,
        checkpoint: &ExtensionEditorCheckpoint,
        text: String,
        shell: &mut InteractiveShell,
    ) -> Result<(), Refusal> {
        let invalid = |detail: &str| (ExtensionRequestFailure::InvalidRequest, detail.to_owned());
        let mount = self
            .mounts
            .iter_mut()
            .find(|mount| {
                mount.owner == *owner
                    && mount.surface_id == checkpoint.surface_id
                    && mount.view.id == checkpoint.mount_id
            })
            .ok_or_else(|| invalid("editor checkpoint belongs to a retired or foreign mount"))?;
        let editor = mount
            .editor
            .as_mut()
            .ok_or_else(|| invalid("composer checkpoints require an editor mount"))?;
        if checkpoint.input_revision > editor.input_revision
            || checkpoint.input_revision < editor.acknowledged_input_revision
            || checkpoint.checkpoint_revision <= editor.checkpoint_revision
        {
            return Err(invalid(
                "editor checkpoint revision is stale or was never issued",
            ));
        }
        let snapshot = shell.extension_editor_snapshot();
        // The host cleared the slot and the component answers that exact clear.
        // A native command can move focus into a panel before the answer
        // arrives, so only this acknowledged empty replacement is admitted
        // without focus; an unrelated draft still cannot write while a panel
        // owns input.
        let admitted_clear = text.is_empty()
            && editor.expected_clear
            && checkpoint.input_revision == editor.acknowledged_input_revision
            && checkpoint.checkpoint_revision > editor.checkpoint_revision;
        if (!snapshot.focused && !admitted_clear) || snapshot.revision != editor.composer_revision {
            return Err(invalid(
                "native composer changed or has another input owner",
            ));
        }
        // The process guard excludes concurrent cancellation and retirement;
        // the same shell owner serializes rescue, local edits and focus changes.
        let committed = if admitted_clear {
            // This is the receipt for the host's already-applied clear, not
            // another text decision. Re-clearing here creates an echo loop.
            snapshot
        } else {
            let cursor = mount
                .editors
                .composer_snapshot()
                .filter(|snapshot| snapshot.text == text)
                .map(|snapshot| snapshot.cursor);
            shell.extension_set_editor_at(text, cursor)
        };
        editor.composer_revision = committed.revision;
        editor.acknowledged_input_revision = checkpoint.input_revision;
        editor.checkpoint_revision = checkpoint.checkpoint_revision;
        editor.expected_clear = false;
        Ok(())
    }

    pub(super) fn reconcile(&mut self, foreground: Option<&str>, size: (u16, u16)) -> Vec<String> {
        if self.chrome.as_ref().is_some_and(|lease| {
            foreground != Some(lease.owner.session_id.as_str())
                || !lease.process.is_running()
                || lease.process.extension_instance_id() != lease.owner.extension_instance_id
                || lease.process.health_snapshot().generation != lease.owner.process_generation
        }) {
            self.chrome = None;
        }
        let mut errors = Vec::new();
        let (columns, rows) = (size.0.max(1), size.1.max(1));
        let mut index = 0;
        while index < self.mounts.len() {
            let mount = &mut self.mounts[index];
            if foreground != Some(mount.owner.session_id.as_str())
                || !mount.process.is_running()
                || mount.process.extension_instance_id() != mount.owner.extension_instance_id
                || mount.process.health_snapshot().generation != mount.owner.process_generation
                || !mount
                    .process
                    .remote_ui_surface_is_current(&mount.owner, &mount.surface_id)
            {
                errors.extend(self.remove(
                    index,
                    "remote UI foreground owner or process generation ended",
                    true,
                ));
                continue;
            }
            if (mount.view.columns, mount.view.rows) != (columns, rows) {
                mount.view.columns = columns;
                mount.view.rows = rows;
                // Invalidate geometry, but retain the accepted revision. A stale
                // post-resize high revision must not poison the next good frame.
                let _ = Arc::make_mut(&mut self.components).store_render(
                    &mount.view.id,
                    columns,
                    Vec::new(),
                );
                if let Err(error) = mount
                    .process
                    .notify_remote_ui_resize(ExtensionRemoteUiResize {
                        surface_id: mount.surface_id.clone(),
                        columns,
                        rows,
                    })
                {
                    let detail = format!(
                        "remote UI {} resize delivery failed: {error}",
                        mount.view.title
                    );
                    errors.extend(self.remove(index, "remote UI resize delivery failed", true));
                    errors.push(detail);
                    continue;
                }
            }
            index += 1;
        }
        errors
    }

    pub(super) fn accept_frame(
        &mut self,
        process: &ExtensionProcess,
        frame: ExtensionRemoteUiFrame,
    ) -> bool {
        let Some(mount) = self.mounts.iter_mut().find(|mount| {
            mount.owner == frame.resource_owner
                && mount.surface_id == frame.surface_id
                && process.extension_instance_id() == mount.owner.extension_instance_id
        }) else {
            return false;
        };
        // Validate geometry and process ownership BEFORE touching revision.
        if !process.is_running()
            || frame.generation != process.health_snapshot().generation
            || frame.generation != mount.owner.process_generation
            || (frame.columns, frame.rows) != (mount.view.columns, mount.view.rows)
            || mount
                .revision
                .is_some_and(|revision| frame.revision <= revision)
        {
            return false;
        }
        if Arc::make_mut(&mut self.components)
            .store_render(&mount.view.id, frame.columns, frame.lines)
            .is_err()
        {
            return false;
        }
        mount.revision = Some(frame.revision);
        true
    }

    pub(super) fn take_ready_composer_event(&mut self) -> Option<Event> {
        // An explicit fullscreen view owns fresh input, never a buffered
        // composer event. Keep those events fenced to their editor mount.
        if self
            .mounts
            .iter()
            .any(|mount| mount.view.placement == ExtensionRemoteUiPlacement::Fullscreen)
        {
            return None;
        }
        let (id, _) = self.pending_input.front()?;
        let mount = self.mounts.iter().find(|mount| &mount.view.id == id)?;
        let editor = mount.editor.as_ref()?;
        if editor.input_revision != editor.acknowledged_input_revision || editor.expected_clear {
            return None;
        }
        self.replaying_input = true;
        self.pending_input.pop_front().map(|(_, event)| event)
    }

    /// Preserve terminal ordering across a draft-sensitive native decision.
    /// Buffered data stays bounded and belongs to the original editor mount.
    fn defer_composer_event(
        &mut self,
        index: usize,
        shell: &mut InteractiveShell,
        event: &Event,
    ) -> bool {
        let editor = self.mounts[index].editor.as_ref().expect("editor mount");
        let waiting =
            editor.input_revision != editor.acknowledged_input_revision || editor.expected_clear;
        let ordered_data = matches!(event, Event::Key(_) | Event::Paste(_));
        let close = shell.composer_event_interrupts_input(event);
        if !close
            && ordered_data
            && ((!self.pending_input.is_empty())
                || waiting && shell.composer_event_needs_checkpoint(event))
        {
            let bytes = |event: &Event| match event {
                Event::Paste(text) => text.len(),
                _ => 0,
            };
            if self.pending_input.len() >= 128
                || self
                    .pending_input
                    .iter()
                    .map(|(_, event)| bytes(event))
                    .sum::<usize>()
                    + bytes(event)
                    > octet_agent::extension_process::MAX_EXTENSION_COMPOSER_TEXT_BYTES
            {
                shell.error(
                    "Custom editor input queue is full; this event was not admitted.".into(),
                );
            } else {
                self.pending_input
                    .push_back((self.mounts[index].view.id.clone(), event.clone()));
            }
            return true;
        }
        if close {
            self.pending_input.clear();
        }
        false
    }

    /// Route one terminal event to a remote surface. One terminal owner keeps a
    /// fixed order: the host's reserved grammar and slash menu run before the
    /// composer slot's editor, while an explicit full-screen extension view is
    /// a deliberate takeover. `active` is the run state the native policy is
    /// classifying against.
    pub(super) fn route_input(
        &mut self,
        shell: &mut InteractiveShell,
        event: &Event,
        active: bool,
    ) -> bool {
        let replaying = std::mem::take(&mut self.replaying_input);
        if shell.remote_ui_input_blocked() {
            return false;
        }
        let Some(index) = self
            .mounts
            .iter()
            .position(|mount| mount.view.placement == ExtensionRemoteUiPlacement::Fullscreen)
            .or_else(|| {
                self.mounts
                    .iter()
                    .position(|mount| mount.view.placement == ExtensionRemoteUiPlacement::Editor)
            })
        else {
            return false;
        };
        let fullscreen =
            self.mounts[index].view.placement == ExtensionRemoteUiPlacement::Fullscreen;
        if !fullscreen && !replaying && self.defer_composer_event(index, shell, event) {
            return true;
        }
        if !fullscreen && !shell.slot_editor_takes_key(event, active) {
            // Host-reserved keys, the open slash menu, and every non-key event
            // keep their native owner. The slot editor sees only the remainder.
            return false;
        }
        if matches!(event, Event::Key(key) if matches!(key.code, KeyCode::Char('d' | 'D')) && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            // Only a fresh press requests exit; no kind of Ctrl+D is component data.
            return !matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press);
        }
        if !fullscreen {
            if let Event::Paste(text) = event {
                let mount = &self.mounts[index].view;
                let write = crate::tui::view::ComposerSlotWrite {
                    surface_id: mount.surface_id.clone(),
                    mount_id: mount.id.clone(),
                    text: Some(text.clone()),
                    paste: true,
                };
                self.deliver_editor_text(&write, shell);
                return true;
            }
        }
        if let Event::Key(key) = event {
            if fullscreen
                && matches!(key.code, KeyCode::Char('g' | 'G'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                if key.kind == KeyEventKind::Press {
                    if let Some(notice) = self.remove(index, "returned to octet with Ctrl+G", true)
                    {
                        shell.notice(notice);
                    }
                }
                return true;
            }
            if let Some(mut key) = normalized_key(&self.mounts[index].surface_id, key) {
                let mount = &mut self.mounts[index];
                if let Some(editor) = &mut mount.editor {
                    if editor.input_revision == MAX_EXTENSION_REMOTE_UI_REVISION {
                        if let Some(notice) =
                            self.remove(index, "editor input revision exhausted", true)
                        {
                            shell.notice(notice);
                        }
                        shell.error("Custom editor closed because its input revision was exhausted; the last key was not delivered.".into());
                        return true;
                    }
                    editor.input_revision += 1;
                    key.editor_input = Some(ExtensionRemoteUiEditorInput {
                        mount_id: mount.view.id.clone(),
                        input_revision: editor.input_revision,
                    });
                }
                if let Err(error) = self.mounts[index].process.notify_remote_ui_key(key) {
                    if let Some(notice) = self.remove(index, "remote UI key delivery failed", true)
                    {
                        shell.notice(notice);
                    }
                    shell.error(format!(
                        "remote UI closed because key delivery failed: {error}"
                    ));
                }
            }
            return true;
        }
        if !fullscreen {
            return false;
        }
        if let Event::Mouse(mouse) = event {
            let mount = &self.mounts[index];
            if mount.view.mouse_capture
                && mouse.column < mount.view.columns
                && mouse.row < mount.view.rows
            {
                if let Some(mouse) = normalized_mouse(&mount.surface_id, mouse) {
                    if let Err(error) = mount.process.notify_remote_ui_mouse(mouse) {
                        if let Some(notice) =
                            self.remove(index, "remote UI mouse delivery failed", true)
                        {
                            shell.notice(notice);
                        }
                        shell.error(format!(
                            "remote UI closed because mouse delivery failed: {error}"
                        ));
                    }
                }
            }
            return true;
        }
        // Unsupported component gestures never mutate the hidden composer or
        // silently pretend to have reached an editor checkpoint.
        if matches!(event, Event::Paste(_)) {
            shell.notice("Paste is not supported by this custom editor transport. Use the native editor to paste attachments.");
            return true;
        }
        false
    }
}

pub(crate) async fn notified(wake: &Option<Arc<tokio::sync::Notify>>) {
    match wake {
        Some(wake) => wake.notified().await,
        None => std::future::pending::<()>().await,
    }
}

pub(crate) fn normalized_key(
    surface_id: &str,
    key: &crossterm::event::KeyEvent,
) -> Option<ExtensionRemoteUiKey> {
    let name = match key.code {
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(character) if !character.is_control() => character.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Escape".into(),
        KeyCode::Tab | KeyCode::BackTab => "Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::Up => "ArrowUp".into(),
        KeyCode::Down => "ArrowDown".into(),
        KeyCode::Left => "ArrowLeft".into(),
        KeyCode::Right => "ArrowRight".into(),
        KeyCode::F(number) => format!("F{number}"),
        _ => return None,
    };
    let mut modifiers = normalized_modifiers(key.modifiers);
    if key.code == KeyCode::BackTab && !modifiers.contains(&ExtensionRemoteUiKeyModifier::Shift) {
        modifiers.insert(0, ExtensionRemoteUiKeyModifier::Shift);
    }
    Some(ExtensionRemoteUiKey {
        surface_id: surface_id.into(),
        key: name,
        kind: match key.kind {
            KeyEventKind::Press => ExtensionRemoteUiKeyKind::Press,
            KeyEventKind::Repeat => ExtensionRemoteUiKeyKind::Repeat,
            KeyEventKind::Release => ExtensionRemoteUiKeyKind::Release,
        },
        modifiers,
        editor_input: None,
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
        if flags.contains(flag) {
            modifiers.push(modifier);
        }
    }
    modifiers
}

pub(crate) fn normalized_mouse(
    surface_id: &str,
    mouse: &crossterm::event::MouseEvent,
) -> Option<ExtensionRemoteUiMouse> {
    use ExtensionRemoteUiMouseButton as Button;
    use ExtensionRemoteUiMouseKind as Kind;
    let button = |button| match button {
        MouseButton::Left => Button::Left,
        MouseButton::Middle => Button::Middle,
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
        surface_id: surface_id.into(),
        kind,
        button,
        // Crossterm already converts SGR mouse coordinates to zero-based cells.
        x: mouse.column,
        y: mouse.row,
        wheel_delta,
        modifiers: normalized_modifiers(mouse.modifiers),
    })
}

#[cfg(all(test, unix))]
mod tests;
