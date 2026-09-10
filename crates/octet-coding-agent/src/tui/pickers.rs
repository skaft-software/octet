#![allow(missing_docs)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::tui::terminal::TerminalInput as EventStream;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use octet_agent::extension_process::{ConfirmationRequest, ExtensionInputRequest};
use octet_agent::tool::{ToolConfirmation, ToolInputRequest};
use octet_ai::{ModelCatalog, ModelId};

use crate::config::ThinkingLevel;
use crate::modes::interactive::run_blocking_lifecycle;
use crate::presentation::{
    compact_context_limit, format_token_rate_value, provider_status_name, ModelDisplayMetadata,
};
use crate::session_store::{SessionMeta, SessionStorageLifecycle, SessionStore};
use crate::tui::view::{
    ForkMessage, InteractiveShell, MessagePicker, OrdinarySurfaceLifecycle,
    OrdinarySurfaceMetadata, Panel, PanelAction, PanelRequest, PanelResult, PickerState,
};

const MAX_SECRET_INPUT_BYTES: usize = 4096;
#[cfg(not(test))]
const SUBAGENT_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(test)]
const SUBAGENT_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Default)]
struct SecretInputBuffer(Vec<u8>);

impl SecretInputBuffer {
    fn push(&mut self, character: char) -> bool {
        let mut encoded = [0; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        let fits = self.0.len().saturating_add(bytes.len()) <= MAX_SECRET_INPUT_BYTES;
        if fits {
            self.0.extend_from_slice(bytes);
        }
        encoded.fill(0);
        fits
    }

    fn extend_paste(&mut self, pasted: &str) -> bool {
        let pasted = pasted.trim_end_matches(['\r', '\n']);
        if pasted.len() > MAX_SECRET_INPUT_BYTES.saturating_sub(self.0.len()) {
            return false;
        }
        self.0.extend_from_slice(pasted.as_bytes());
        true
    }

    fn backspace(&mut self) {
        let Some((start, _)) = std::str::from_utf8(&self.0)
            .ok()
            .and_then(|text| text.char_indices().last())
        else {
            return;
        };
        self.0[start..].fill(0);
        self.0.truncate(start);
    }

    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for SecretInputBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Also clears private context if the owning picker future is dropped.
struct ToolInputSurface<'a> {
    shell: &'a mut InteractiveShell,
    request: Option<&'a ToolInputRequest>,
}

impl Drop for ToolInputSurface<'_> {
    fn drop(&mut self) {
        if let Some(request) = self.request {
            request.cancel();
        }
        self.shell.set_tool_input_prompt(None);
        self.shell.render();
    }
}

fn private_input_unavailable(shell: &mut InteractiveShell) {
    shell.error(
        "Private input cancelled: complete context must fit; terminal write logging must be off."
            .into(),
    );
}

/// Give one running tool exclusive ownership of terminal input. Complete
/// context stays in a transient panel; answers never enter the ordinary editor.
pub async fn tool_input_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    request: &ToolInputRequest,
) -> anyhow::Result<bool>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let surface = ToolInputSurface {
        shell,
        request: Some(request),
    };
    let shell = &mut *surface.shell;
    if !request.is_pending() {
        return Ok(false);
    }
    if !shell.set_tool_input_prompt(Some(request.prompt.clone())) {
        private_input_unavailable(shell);
        return Ok(false);
    }
    shell.render();
    let mut secret = SecretInputBuffer::default();
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => None,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if !request.is_pending() {
                    return Ok(false);
                }
                if !shell.tool_input_fits() {
                    private_input_unavailable(shell);
                    return Ok(false);
                }
                continue;
            }
            next = input.next() => next,
        };
        let Some(event) = next else {
            return Ok(false);
        };
        let event = event?;
        if !request.is_pending() {
            return Ok(false);
        }
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.request_close();
            return Ok(false);
        }
        if !shell.tool_input_fits() {
            private_input_unavailable(shell);
            return Ok(false);
        }
        let fits = match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter if key.kind == KeyEventKind::Press => {
                        if !shell.tool_input_was_rendered() {
                            shell.render();
                            continue;
                        }
                        request.respond(secret.take());
                        return Ok(true);
                    }
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Backspace => {
                        secret.backspace();
                        true
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(false);
                    }
                    KeyCode::Char(character)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        secret.push(character)
                    }
                    _ => true,
                }
            }
            Event::Paste(pasted) => {
                let fits = secret.extend_paste(&pasted);
                pasted.into_bytes().fill(0);
                fits
            }
            Event::Resize(columns, rows) => {
                shell.set_size(columns, rows);
                true
            }
            _ => true,
        };
        if !fits {
            shell.error("Private input cancelled: answer exceeds 4096 bytes.".into());
            return Ok(false);
        }
        if !shell.tool_input_fits() {
            private_input_unavailable(shell);
            return Ok(false);
        }
        // Secret bytes never influence frame contents, including their length.
        shell.render();
    }
}

/// Give one extension command exclusive ownership of terminal input. Secret
/// answers never enter the ordinary editor or rendered frame; non-secret setup
/// values are echoed only on the same temporary private surface.
pub async fn extension_input_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    request: &ExtensionInputRequest,
) -> anyhow::Result<Option<String>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let surface = ToolInputSurface {
        shell,
        request: None,
    };
    let shell = &mut *surface.shell;
    if !shell.set_tool_input_prompt(Some(request.prompt.clone())) {
        private_input_unavailable(shell);
        return Ok(None);
    }
    shell.render();
    let mut value = SecretInputBuffer::default();
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => None,
            next = input.next() => next,
        };
        let Some(event) = next else {
            return Ok(None);
        };
        let event = event?;
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.request_close();
            return Ok(None);
        }
        if !shell.tool_input_fits() {
            private_input_unavailable(shell);
            return Ok(None);
        }
        let fits = match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter if key.kind == KeyEventKind::Press => {
                        if !shell.tool_input_was_rendered() {
                            shell.render();
                            continue;
                        }
                        return Ok(Some(String::from_utf8(value.take()).map_err(|_| {
                            anyhow::anyhow!("extension input was not valid UTF-8")
                        })?));
                    }
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Backspace => {
                        value.backspace();
                        true
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    KeyCode::Char(character)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        value.push(character)
                    }
                    _ => true,
                }
            }
            Event::Paste(pasted) => {
                let fits = value.extend_paste(&pasted);
                pasted.into_bytes().fill(0);
                fits
            }
            Event::Resize(columns, rows) => {
                shell.set_size(columns, rows);
                true
            }
            _ => true,
        };
        if !fits {
            shell.error("Private input cancelled: answer exceeds 4096 bytes.".into());
            return Ok(None);
        }
        if !request.secret {
            let entered = std::str::from_utf8(&value.0).expect("UTF-8 input");
            if !shell.set_tool_input_prompt(Some(format!("{} {}", request.prompt, entered))) {
                private_input_unavailable(shell);
                return Ok(None);
            }
        }
        if !shell.tool_input_fits() {
            private_input_unavailable(shell);
            return Ok(None);
        }
        shell.render();
    }
}

/// Drive a panel-based selection list. Owns the event loop while the panel is open.
async fn pick_list<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    surface: OrdinarySurfaceMetadata,
    items: Vec<String>,
    descriptions: Vec<Option<String>>,
    initial_selected: usize,
    action: PanelAction,
) -> anyhow::Result<Option<usize>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    pick_list_with_preview(
        shell,
        input,
        surface,
        items,
        descriptions,
        initial_selected,
        action,
        |_, _| {},
    )
    .await
}

/// Preview receives original item indices, including `None` for an empty filter
/// result. The caller owns rollback and any persistence after confirmation.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn pick_list_with_preview<S, F>(
    shell: &mut InteractiveShell,
    input: &mut S,
    surface: OrdinarySurfaceMetadata,
    items: Vec<String>,
    descriptions: Vec<Option<String>>,
    initial_selected: usize,
    action: PanelAction,
    mut preview: F,
) -> anyhow::Result<Option<usize>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
    F: FnMut(&mut InteractiveShell, Option<usize>),
{
    if items.is_empty() {
        shell.error("nothing is available to select".into());
        shell.render();
        return Ok(None);
    }

    let initial_selected = initial_selected.min(items.len().saturating_sub(1));
    shell.open_panel(Panel::SelectList {
        surface,
        items,
        descriptions,
        selected: initial_selected,
        filter: String::new(),
        action,
    });
    let mut highlighted = shell.highlighted_panel_index();
    preview(shell, highlighted);
    shell.render();

    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.close_panel();
                return Ok(None);
            }
            next = input.next() => next,
        };
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                shell.close_panel();
                return Err(error.into());
            }
            None => {
                shell.close_panel();
                return Ok(None);
            }
        };
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.close_panel();
            shell.request_close();
            shell.render();
            return Ok(None);
        }
        // Mouse events pass through to the shell for transcript scrolling.
        if matches!(event, Event::Mouse(_)) {
            continue;
        }
        if let Some((result, _action)) = shell.panel_input(&event) {
            shell.render();
            return Ok(match result {
                PanelResult::Confirm(index) => Some(index),
                PanelResult::Cancel => None,
                PanelResult::Select(_) => None,
            });
        }
        let next_highlighted = shell.highlighted_panel_index();
        if next_highlighted != highlighted {
            highlighted = next_highlighted;
            preview(shell, highlighted);
        }
        // Panel consumed the event; render updated state.
        shell.render();
    }
}

/// Select one installed executable extension. Enter confirms the highlighted
/// row; Escape closes the management view without changing configuration.
pub async fn extension_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    surface: OrdinarySurfaceMetadata,
    items: Vec<String>,
    descriptions: Vec<Option<String>>,
    initial_selected: usize,
) -> anyhow::Result<Option<usize>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let action_items = items.clone();
    pick_list(
        shell,
        input,
        surface,
        items,
        descriptions,
        initial_selected,
        PanelAction::SelectExtension(action_items),
    )
    .await
}

/// Select a single step in the guided provider-setup flow. This uses the
/// ordinary select-list surface and retains cancellation as a non-mutating
/// outcome for the caller.
pub async fn provider_setup_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    title: &str,
    items: Vec<String>,
    descriptions: Vec<Option<String>>,
    initial_selected: usize,
) -> anyhow::Result<Option<usize>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let action_items = items.clone();
    pick_list(
        shell,
        input,
        OrdinarySurfaceMetadata::new(title),
        items,
        descriptions,
        initial_selected,
        PanelAction::ProviderSetup(action_items),
    )
    .await
}

/// Complete live subagent list replacement supplied by the owning product loop.
pub struct SubagentPickerSnapshot {
    pub title: String,
    pub items: Vec<String>,
    pub descriptions: Vec<Option<String>>,
    pub node_ids: Vec<String>,
    pub notices: Vec<String>,
}

/// Select one subagent node while periodically refreshing presentation state.
/// Selection is preserved by stable node ID and the returned ID is revalidated
/// by the caller before opening a transcript.
pub async fn subagent_picker<S, C, F>(
    shell: &mut InteractiveShell,
    input: &mut S,
    initial: SubagentPickerSnapshot,
    initial_selected: usize,
    context: &mut C,
    mut refresh: F,
) -> anyhow::Result<Option<String>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
    F: for<'a> FnMut(&'a mut C) -> Pin<Box<dyn Future<Output = SubagentPickerSnapshot> + 'a>>,
{
    if initial.items.is_empty() {
        return Ok(None);
    }
    let selected = initial_selected.min(initial.items.len().saturating_sub(1));
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new(initial.title),
        items: initial.items,
        descriptions: initial.descriptions,
        selected,
        filter: String::new(),
        action: PanelAction::SelectSubagent(initial.node_ids),
    });
    for notice in initial.notices {
        shell.notice(notice);
    }
    shell.render();

    let mut refresh_tick = tokio::time::interval(SUBAGENT_REFRESH_INTERVAL);
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let _ = refresh_tick.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.close_panel();
                shell.request_close();
                shell.render();
                return Ok(None);
            }
            next = input.next() => {
                let event = match next {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        shell.close_panel();
                        return Err(error.into());
                    }
                    None => {
                        shell.close_panel();
                        return Ok(None);
                    }
                };
                if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
                    shell.close_panel();
                    shell.request_close();
                    shell.render();
                    return Ok(None);
                }
                if matches!(event, Event::Mouse(_)) {
                    continue;
                }
                if let Some((result, action)) = shell.panel_input(&event) {
                    shell.render();
                    return Ok(match (result, action) {
                        (PanelResult::Confirm(index), PanelAction::SelectSubagent(node_ids)) => {
                            node_ids.get(index).cloned()
                        }
                        (PanelResult::Cancel, _) => None,
                        _ => None,
                    });
                }
                shell.render();
            }
            _ = refresh_tick.tick() => {
                let snapshot = refresh(context).await;
                for notice in snapshot.notices {
                    shell.notice(notice);
                }
                shell.refresh_subagent_panel(
                    snapshot.title,
                    snapshot.items,
                    snapshot.descriptions,
                    snapshot.node_ids,
                );
                shell.render();
            }
        }
    }
}

/// Show a bounded read-only document. Arrow and page keys scroll; Escape or
/// Left returns to the owning list instead of closing the octet session.
pub async fn read_only_document<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    title: impl Into<String>,
    text: String,
) -> anyhow::Result<()>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    shell.open_panel(Panel::ReadOnlyDocument {
        title: title.into(),
        text: crate::tui::view::sanitize_for_terminal(&text),
        styled: false,
        scroll_from_bottom: 0,
    });
    shell.render();
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.close_panel();
                shell.request_close();
                return Ok(());
            }
            next = input.next() => next,
        };
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                shell.close_panel();
                return Err(error.into());
            }
            None => {
                shell.close_panel();
                shell.request_close();
                return Ok(());
            }
        };
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.close_panel();
            shell.request_close();
            shell.render();
            return Ok(());
        }
        if matches!(event, Event::Mouse(_)) {
            continue;
        }
        if shell.panel_input(&event).is_some() {
            shell.render();
            return Ok(());
        }
        shell.render();
    }
}

/// Styled variant of [`read_only_document`]: the producer sanitizes its
/// content once and applies trusted theme ANSI, which rendering preserves. The
/// refresh callback receives the current panel content width and is rerun
/// immediately after a resize so pre-laid-out transcript surfaces stay exact.
pub async fn read_only_document_live_styled<S, F, Fut>(
    shell: &mut InteractiveShell,
    input: &mut S,
    title: impl Into<String>,
    text: String,
    mut refresh: F,
) -> anyhow::Result<()>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
    F: FnMut(u16) -> Fut,
    Fut: Future<Output = anyhow::Result<Option<String>>>,
{
    shell.open_panel(Panel::ReadOnlyDocument {
        title: title.into(),
        text,
        styled: true,
        scroll_from_bottom: 0,
    });
    shell.render();
    let mut refresh_tick = tokio::time::interval(SUBAGENT_REFRESH_INTERVAL);
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.close_panel();
                shell.request_close();
                return Ok(());
            }
            next = input.next() => {
                let event = match next {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        shell.close_panel();
                        return Err(error.into());
                    }
                    None => {
                        shell.close_panel();
                        shell.request_close();
                        return Ok(());
                    }
                };
                if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
                    shell.close_panel();
                    shell.render();
                    return Ok(());
                }
                if matches!(event, Event::Mouse(_)) {
                    continue;
                }
                let resized = matches!(&event, Event::Resize(_, _));
                if shell.panel_input(&event).is_some() {
                    shell.render();
                    return Ok(());
                }
                if resized {
                    let width = shell.read_only_document_width();
                    if let Ok(Some(text)) = refresh(width).await {
                        shell.update_read_only_document_styled(text);
                    }
                }
                shell.render();
            }
            _ = refresh_tick.tick() => {
                let width = shell.read_only_document_width();
                if let Ok(Some(text)) = refresh(width).await {
                    shell.update_read_only_document_styled(text);
                    shell.render();
                }
            }
        }
    }
}

/// Hide recoverably trashed sessions from resume/fork browsing.
fn picker_rows(sessions: impl Iterator<Item = SessionMeta>) -> Vec<SessionMeta> {
    sessions
        .filter(|session| session.trashed_at_ms.is_none())
        .collect()
}

/// Ask the user to select a stored session from a precomputed snapshot.
pub async fn session_picker(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    sessions: &[SessionMeta],
    store: &SessionStore,
    current_session_path: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    let mut rows = picker_rows(sessions.iter().cloned());
    if rows.is_empty() {
        shell.error(format!("no sessions in {}", store.dir().display()));
        shell.render();
        return Ok(None);
    }

    let current_session_path = current_session_path.map(Path::to_owned);
    let mut all_rows = None;
    shell.open_panel(Panel::SessionPicker {
        picker: PickerState::new(rows.clone(), current_session_path),
    });
    shell.render();

    loop {
        let requests = shell.drain_panel_requests();
        for request in requests {
            match request {
                PanelRequest::LoadAll => {
                    let discovery_store = store.clone();
                    let discovered = match run_blocking_lifecycle(
                        shell,
                        input,
                        "discovering sessions in all workspaces…",
                        move || Ok(discovery_store.list_all()),
                    )
                    .await
                    {
                        Ok(discovered) => discovered,
                        Err(error) => {
                            shell.set_picker_lifecycle(
                                OrdinarySurfaceLifecycle::recoverable_error(
                                    format!("to load all workspaces: {error}"),
                                    Instant::now() + Duration::from_secs(3),
                                ),
                            );
                            shell.render();
                            return Err(error);
                        }
                    };
                    all_rows = Some(picker_rows(discovered.into_iter()));
                    shell.refresh_panel_sessions(rows.clone(), all_rows.clone());
                    shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::success(
                        "all workspaces loaded",
                        Instant::now() + Duration::from_secs(2),
                    ));
                    shell.render();
                }
                PanelRequest::TrashSession { path, .. } => {
                    let Some(id) = session_id_from_path(&path) else {
                        shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::recoverable_error(
                            "to trash session: path has no valid id",
                            Instant::now() + Duration::from_secs(3),
                        ));
                        shell.render();
                        continue;
                    };
                    let target_store = store_for_session_path(store, &path);
                    let changed_at_ms = unix_now_ms();
                    let result = run_blocking_lifecycle(
                        shell,
                        input,
                        "moving session to trash…",
                        move || {
                            target_store
                                .set_lifecycle(&id, SessionStorageLifecycle::Trash, changed_at_ms)
                                .map(|_| ())
                        },
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            let (next_rows, next_all) =
                                refresh_session_rows(shell, input, store, all_rows.is_some())
                                    .await?;
                            rows = next_rows;
                            all_rows = next_all;
                            shell.refresh_panel_sessions(rows.clone(), all_rows.clone());
                            shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::success(
                                "session moved to trash",
                                Instant::now() + Duration::from_secs(2),
                            ));
                        }
                        Err(error) => {
                            shell.set_picker_lifecycle(
                                OrdinarySurfaceLifecycle::recoverable_error(
                                    format!("to trash session: {error}"),
                                    Instant::now() + Duration::from_secs(3),
                                ),
                            );
                        }
                    }
                    shell.render();
                }
                PanelRequest::RenameSession { path, name, .. } => {
                    let Some(id) = session_id_from_path(&path) else {
                        shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::recoverable_error(
                            "to rename session: path has no valid id",
                            Instant::now() + Duration::from_secs(3),
                        ));
                        shell.render();
                        continue;
                    };
                    let target_store = store_for_session_path(store, &path);
                    let result =
                        run_blocking_lifecycle(shell, input, "renaming session…", move || {
                            target_store.rename(&id, &name).map(|_| ())
                        })
                        .await;
                    match result {
                        Ok(()) => {
                            let (next_rows, next_all) =
                                refresh_session_rows(shell, input, store, all_rows.is_some())
                                    .await?;
                            rows = next_rows;
                            all_rows = next_all;
                            shell.refresh_panel_sessions(rows.clone(), all_rows.clone());
                            shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::success(
                                "session renamed",
                                Instant::now() + Duration::from_secs(2),
                            ));
                        }
                        Err(error) => {
                            shell.set_picker_lifecycle(
                                OrdinarySurfaceLifecycle::recoverable_error(
                                    format!("to rename session: {error}"),
                                    Instant::now() + Duration::from_secs(3),
                                ),
                            );
                        }
                    }
                    shell.render();
                }
            }
        }

        if shell.close_requested() {
            shell.close_panel();
            return Ok(None);
        }
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.close_panel();
                return Ok(None);
            }
            next = input.next() => next,
        };
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                shell.close_panel();
                return Err(error.into());
            }
            None => {
                shell.close_panel();
                shell.request_close();
                return Ok(None);
            }
        };
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.close_panel();
            shell.request_close();
            shell.render();
            return Ok(None);
        }
        if matches!(event, Event::Mouse(_)) {
            continue;
        }
        if let Some((result, _action)) = shell.panel_input(&event) {
            shell.render();
            match result {
                PanelResult::Cancel => return Ok(None),
                PanelResult::Select(_) => {
                    let Some((id, path)) = shell.take_picker_selection() else {
                        return Ok(None);
                    };
                    if !selection_is_in_current_workspace(store, &rows, &all_rows, &id, &path) {
                        shell.notice_error("cannot resume a session from another workspace");
                        shell.render();
                        return Ok(None);
                    }
                    return Ok(Some(path));
                }
                PanelResult::Confirm(_) => {}
            }
        }
        shell.render();
    }
}

fn session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn store_for_session_path(base: &SessionStore, path: &Path) -> SessionStore {
    let Some(directory) = path.parent() else {
        return base.clone();
    };
    if directory == base.dir() {
        base.clone()
    } else {
        SessionStore::for_directory(directory, base.root())
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn refresh_session_rows(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    store: &SessionStore,
    include_all: bool,
) -> anyhow::Result<(Vec<SessionMeta>, Option<Vec<SessionMeta>>)> {
    let store = store.clone();
    run_blocking_lifecycle(shell, input, "refreshing sessions…", move || {
        let rows = picker_rows(store.list().into_iter());
        let all = include_all.then(|| picker_rows(store.list_all().into_iter()));
        Ok((rows, all))
    })
    .await
}

fn selection_is_in_current_workspace(
    store: &SessionStore,
    rows: &[SessionMeta],
    all_rows: &Option<Vec<SessionMeta>>,
    id: &str,
    path: &Path,
) -> bool {
    let meta = rows
        .iter()
        .chain(all_rows.as_deref().unwrap_or(&[]).iter())
        .find(|meta| meta.id == id && meta.path == path);
    match meta.and_then(|meta| meta.workspace.as_deref()) {
        Some(workspace) => store.workspace() == Some(workspace),
        None => path.parent() == Some(store.dir()),
    }
}

/// Ask the user to choose a message boundary for `/fork`.
pub async fn message_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    messages: Vec<ForkMessage>,
) -> anyhow::Result<Option<(String, String)>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    if messages.is_empty() {
        return Ok(None);
    }
    shell.open_panel(Panel::MessagePicker {
        picker: MessagePicker::new(messages),
    });
    shell.render();
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                shell.close_panel();
                return Ok(None);
            }
            next = input.next() => next,
        };
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                shell.close_panel();
                return Err(error.into());
            }
            None => {
                shell.close_panel();
                shell.request_close();
                return Ok(None);
            }
        };
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.close_panel();
            shell.request_close();
            shell.render();
            return Ok(None);
        }
        if matches!(event, Event::Mouse(_)) {
            continue;
        }
        if shell.close_requested() {
            shell.close_panel();
            return Ok(None);
        }
        if let Some((result, _action)) = shell.panel_input(&event) {
            shell.render();
            match result {
                PanelResult::Cancel => return Ok(None),
                PanelResult::Select(_) => return Ok(shell.take_message_picker_selection()),
                PanelResult::Confirm(_) => {}
            }
        }
        shell.render();
    }
}

/// Ask the user to select a capability-supported thinking level.
pub async fn thinking_picker(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    levels: &[ThinkingLevel],
) -> anyhow::Result<Option<ThinkingLevel>> {
    let mut items: Vec<String> = levels.iter().map(|l| l.label().into()).collect();
    let (_, current) = shell.selected_identity();
    let initial = mark_current_choice(
        &mut items,
        levels.iter().position(|level| level.label() == current),
    );
    let action_levels = levels.to_vec();
    let Some(index) = pick_list(
        shell,
        input,
        OrdinarySurfaceMetadata::with_purpose(
            "Select thinking level",
            "Choose effort for subsequent prompts and the startup default",
        ),
        items,
        vec![None; levels.len()],
        initial,
        PanelAction::SelectThinking(action_levels),
    )
    .await?
    else {
        return Ok(None);
    };
    let selected = levels[index];
    if let Err(e) = crate::cli::persist_reasoning(selected.label()) {
        shell.error(format!("failed to save thinking preference: {e}"));
    }
    Ok(Some(selected))
}

/// Ask the user to approve a typed tool request. Escape and input
/// closure are denials; approval is never inferred from a missing frontend.
pub async fn confirmation_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    request: &ToolConfirmation,
) -> anyhow::Result<bool>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    confirmation_prompt_picker(
        shell,
        input,
        &request.prompt,
        request.detail.as_deref(),
        request.destructive,
        request.default,
        request.require_complete_preview,
    )
    .await
}

/// Extension command settlement can drop its confirmation future independently
/// of terminal input. Keep that ordinary panel ephemeral on the drop path too.
struct ExtensionConfirmationSurface<'a>(&'a mut InteractiveShell);

impl Drop for ExtensionConfirmationSurface<'_> {
    fn drop(&mut self) {
        self.0.close_panel();
        self.0.render();
    }
}

pub async fn extension_confirmation_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    extension: &str,
    request: &ConfirmationRequest,
) -> anyhow::Result<bool>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let surface = ExtensionConfirmationSurface(shell);
    let prompt = format!("{extension}: {}", request.prompt);
    confirmation_prompt_picker(
        &mut *surface.0,
        input,
        &prompt,
        request.detail.as_deref(),
        request.destructive,
        request.default,
        false,
    )
    .await
}

async fn confirmation_prompt_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    prompt: &str,
    detail: Option<&str>,
    destructive: bool,
    default: bool,
    require_complete_preview: bool,
) -> anyhow::Result<bool>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let (items, decisions) = if default {
        (vec!["Approve".to_owned(), "Deny".to_owned()], [true, false])
    } else {
        (vec!["Deny".to_owned(), "Approve".to_owned()], [false, true])
    };
    // The detail is shared approval evidence, not per-choice metadata. The
    // panel renderer displays one bounded copy while keeping the two actions
    // independently selectable.
    let shared_detail = detail.map(str::to_owned);
    let descriptions = vec![shared_detail.clone(), shared_detail];
    let title = if destructive && !require_complete_preview {
        format!("Action requires approval · {prompt}")
    } else {
        prompt.to_owned()
    };
    let selected = pick_list(
        shell,
        input,
        OrdinarySurfaceMetadata::new(title),
        items,
        descriptions,
        0,
        if require_complete_preview {
            PanelAction::CompleteConfirmation {
                approve_index: usize::from(!default),
            }
        } else {
            PanelAction::Confirmation
        },
    )
    .await?;
    Ok(selected.map(|index| decisions[index]).unwrap_or(false))
}

/// Build a human-facing label from the same cached metadata boundary used by
/// the footer. Provider identity is rendered once as a non-selectable group
/// heading, never repeated in each model row.
fn model_label(model: &octet_ai::ModelSpec) -> String {
    ModelDisplayMetadata::resolve(model).name
}

fn compact_rate_value(rate: octet_ai::TokenRate) -> String {
    let value = format_token_rate_value(rate);
    let Some((whole, fraction)) = value
        .strip_prefix('$')
        .and_then(|value| value.split_once('.'))
    else {
        return value;
    };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("${whole}")
    } else {
        format!("${whole}.{fraction}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelPickerMetadata {
    input_cost: String,
    output_cost: String,
    context: String,
    media: String,
}

fn model_picker_metadata(model: &octet_ai::ModelSpec) -> ModelPickerMetadata {
    let (input_cost, output_cost) = model.pricing.as_ref().map_or_else(
        || ("—".to_owned(), "—".to_owned()),
        |pricing| {
            (
                format!("{}/M", compact_rate_value(pricing.input)),
                format!("{}/M", compact_rate_value(pricing.output)),
            )
        },
    );
    let vision = model
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image)
        || model
            .capabilities
            .output_modalities
            .contains(octet_ai::Modality::Image);
    let audio = model
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Audio)
        || model
            .capabilities
            .output_modalities
            .contains(octet_ai::Modality::Audio);
    let media = match (vision, audio) {
        (true, true) => "vision + audio",
        (true, false) => "vision",
        (false, true) => "audio",
        // Text is the baseline, not a capability badge. Repeating it on most
        // rows would recreate the same visual noise provider grouping removes.
        (false, false) => "",
    }
    .to_owned();
    ModelPickerMetadata {
        input_cost,
        output_cost,
        context: compact_context_limit(model.limits.context_window),
        media,
    }
}

fn model_provider_heading(catalog: &ModelCatalog, model: &octet_ai::ModelSpec) -> String {
    let heading = catalog
        .endpoint_label(&model.endpoint)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| provider_status_name(&model.endpoint.0));
    if heading == "local endpoint" {
        "Local Endpoint".to_owned()
    } else {
        heading
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelPickerPresentation {
    ids: Vec<ModelId>,
    providers: Vec<String>,
    labels: Vec<String>,
    descriptions: Vec<Option<String>>,
}

fn pad_visible_right(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(sexy_tui_rs::visible_width(value)))
    )
}

fn pad_visible_left(value: &str, width: usize) -> String {
    format!(
        "{}{value}",
        " ".repeat(width.saturating_sub(sexy_tui_rs::visible_width(value)))
    )
}

fn model_picker_presentation(catalog: &ModelCatalog) -> ModelPickerPresentation {
    let mut rows = catalog
        .models()
        .map(|model| {
            (
                model_provider_heading(catalog, model),
                model_label(model),
                model.id.clone(),
                model_picker_metadata(model),
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.0
            .to_lowercase()
            .cmp(&right.0.to_lowercase())
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.to_lowercase().cmp(&right.1.to_lowercase()))
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2 .0.cmp(&right.2 .0))
    });

    let input_width = rows
        .iter()
        .map(|row| sexy_tui_rs::visible_width(&row.3.input_cost))
        .max()
        .unwrap_or(1);
    let output_width = rows
        .iter()
        .map(|row| sexy_tui_rs::visible_width(&row.3.output_cost))
        .max()
        .unwrap_or(1);
    let context_width = rows
        .iter()
        .map(|row| sexy_tui_rs::visible_width(&row.3.context))
        .max()
        .unwrap_or(1);

    let mut presentation = ModelPickerPresentation {
        ids: Vec::with_capacity(rows.len()),
        providers: Vec::with_capacity(rows.len()),
        labels: Vec::with_capacity(rows.len()),
        descriptions: Vec::with_capacity(rows.len()),
    };
    for (provider, label, id, metadata) in rows {
        let media = if metadata.media.is_empty() {
            String::new()
        } else {
            format!("  {}", metadata.media)
        };
        presentation.ids.push(id);
        presentation.providers.push(provider);
        presentation.labels.push(label);
        presentation.descriptions.push(Some(format!(
            "in {}  out {}  {} ctx{media}",
            pad_visible_right(&metadata.input_cost, input_width),
            pad_visible_right(&metadata.output_cost, output_width),
            pad_visible_left(&metadata.context, context_width),
        )));
    }
    presentation
}

fn mark_current_choice(labels: &mut [String], current: Option<usize>) -> usize {
    if let Some(index) = current {
        labels[index].push_str(" (current)");
        index
    } else {
        0
    }
}

/// Ask the user to select one model, preserving cancellation for workflows
/// such as `/logout` that must not mutate credentials until a replacement model
/// has been chosen.
pub async fn optional_model_picker(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    catalog: &ModelCatalog,
) -> anyhow::Result<Option<ModelId>> {
    let selected = pick_model_choice(shell, input, catalog).await?;
    if let Some(id) = &selected {
        if let Err(e) = crate::cli::persist_model(&id.0) {
            shell.error(format!("failed to save model preference: {e}"));
        }
    }
    Ok(selected)
}

async fn pick_model_choice<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    catalog: &ModelCatalog,
) -> anyhow::Result<Option<ModelId>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let (current, _) = shell.selected_identity();
    let mut presentation = model_picker_presentation(catalog);
    mark_current_choice(
        &mut presentation.labels,
        presentation.ids.iter().position(|id| id.0 == current),
    );

    let Some(index) = pick_list(
        shell,
        input,
        OrdinarySurfaceMetadata::with_purpose(
            "Select model",
            "Choose the model for subsequent prompts and the startup default",
        ),
        presentation.labels,
        presentation.descriptions,
        // Current is an annotation, not the initial viewport or keyboard focus.
        0,
        PanelAction::SelectGroupedModel {
            models: presentation.ids.clone(),
            providers: presentation.providers,
        },
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(Some(presentation.ids[index].clone()))
}

/// Ask the user to select one model from the active catalog.
pub async fn model_picker(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    catalog: &ModelCatalog,
) -> anyhow::Result<ModelId> {
    optional_model_picker(shell, input, catalog)
        .await?
        .ok_or_else(|| anyhow::anyhow!("model selection cancelled"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    #[test]
    fn private_input_buffer_accepts_exact_bound_without_truncating_paste_or_unicode() {
        let mut buffer = SecretInputBuffer::default();
        assert!(buffer.extend_paste(&"x".repeat(MAX_SECRET_INPUT_BYTES - 2)));
        assert!(buffer.push('é'));
        assert_eq!(buffer.0.len(), MAX_SECRET_INPUT_BYTES);
        assert!(!buffer.push('x'));
        assert!(!buffer.extend_paste("tail"));
        assert_eq!(buffer.0.len(), MAX_SECRET_INPUT_BYTES);
        buffer.backspace();
        assert_eq!(buffer.0.len(), MAX_SECRET_INPUT_BYTES - 2);
        assert!(buffer.extend_paste("ok\r\n"));
        assert_eq!(buffer.take().len(), MAX_SECRET_INPUT_BYTES);
        assert!(buffer.0.is_empty());
    }

    #[tokio::test]
    async fn complete_confirmation_picker_denies_clipped_preview_but_allows_complete_normal_width()
    {
        for (padding, width, height, expected) in [
            (5 * 1024, 80, 24, false),
            (400, 80, 24, true),
            (5 * 1024, 240, 50, true),
            (0, 46, 24, false),
            (0, 80, 6, false),
        ] {
            let (sink, mut progress) = octet_agent::ToolProgressSink::bounded_channel();
            let detail = serde_json::json!({"a_padding": "x".repeat(padding), "z_consequence": "delete-production"}).to_string();
            let ask = tokio::spawn(async move {
                sink.confirmation_with_complete_preview(
                    octet_agent::extension_policy::EXTENSION_TOOL_APPROVAL_PROMPT.into(),
                    detail,
                    true,
                    false,
                )
                .await
            });
            let octet_agent::ToolProgress::Confirmation(request) = progress.recv().await.unwrap()
            else {
                panic!("confirmation")
            };
            assert!(request.require_complete_preview);
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, height);
            let mut input = futures_util::stream::iter(
                [KeyCode::Down, KeyCode::Enter, KeyCode::Esc]
                    .map(|code| Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))),
            );
            let allowed = confirmation_picker(&mut shell, &mut input, &request)
                .await
                .unwrap();
            request.respond(allowed);
            assert_eq!(allowed, expected, "{width}x{height}, {padding} bytes");
            assert_eq!(ask.await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn complete_confirmation_picker_rechecks_resize_and_never_infers_consent() {
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        for (default, events, expected) in [
            (false, vec![key(KeyCode::Enter)], false),
            (false, vec![], false),
            (false, vec![key(KeyCode::Esc)], false),
            (
                false,
                vec![
                    key(KeyCode::Down),
                    Event::Resize(80, 6),
                    key(KeyCode::Enter),
                ],
                false,
            ),
            (
                false,
                vec![
                    Event::Resize(46, 24),
                    key(KeyCode::Down),
                    key(KeyCode::Enter),
                ],
                false,
            ),
            (
                false,
                vec![
                    Event::Resize(46, 24),
                    key(KeyCode::Down),
                    key(KeyCode::Enter),
                    Event::Resize(80, 24),
                    key(KeyCode::Enter),
                ],
                true,
            ),
            (
                true,
                vec![Event::Resize(46, 24), key(KeyCode::Enter)],
                false,
            ),
            (true, vec![key(KeyCode::Enter)], true),
            (
                true,
                vec![
                    Event::Resize(46, 24),
                    key(KeyCode::Down),
                    key(KeyCode::Enter),
                ],
                false,
            ),
        ] {
            let (sink, mut progress) = octet_agent::ToolProgressSink::bounded_channel();
            let ask = tokio::spawn(async move {
                sink.confirmation_with_complete_preview(
                    octet_agent::extension_policy::EXTENSION_TOOL_APPROVAL_PROMPT.into(),
                    "Untrusted tool data (complete JSON):\n{\"tool\":\"fixture\",\"arguments\":{}}"
                        .into(),
                    true,
                    default,
                )
                .await
            });
            let octet_agent::ToolProgress::Confirmation(request) = progress.recv().await.unwrap()
            else {
                panic!("confirmation")
            };
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(80, 24);
            let mut input = futures_util::stream::iter(events.into_iter().map(Ok));
            let allowed = confirmation_picker(&mut shell, &mut input, &request)
                .await
                .unwrap();
            request.respond(allowed);
            assert_eq!(allowed, expected);
            assert_eq!(ask.await.unwrap(), expected);
        }
    }

    #[test]
    fn active_choice_is_focused_and_marked_without_reordering() {
        let mut labels = vec!["off".into(), "high".into(), "max".into()];
        assert_eq!(mark_current_choice(&mut labels, Some(2)), 2);
        assert_eq!(labels, ["off", "high", "max (current)"]);
        let mut empty = Vec::new();
        assert_eq!(mark_current_choice(&mut empty, None), 0);
    }

    #[tokio::test]
    async fn preview_follows_navigation_and_filtered_original_indices_before_next_input() {
        use crate::tui::theme::{test_theme_for, TerminalBackground};
        use std::cell::RefCell;
        use std::task::Poll;

        let mut shell = InteractiveShell::test_shell();
        let original = shell.theme();
        let backgrounds = [
            TerminalBackground::Unknown,
            TerminalBackground::Light,
            TerminalBackground::Dark,
        ];
        let themes =
            backgrounds.map(|background| test_theme_for(background, original.capabilities()));
        let observed = RefCell::new(Vec::new());
        // Each expectation is checked when the stream is polled for the NEXT
        // event: the previous navigation must have already changed the theme.
        let mut script = [
            (Some(0), KeyCode::Down),
            (Some(1), KeyCode::Down),
            (Some(2), KeyCode::Up),
            (Some(1), KeyCode::Home),
            (Some(0), KeyCode::End),
            (Some(2), KeyCode::Char('t')),
            (Some(0), KeyCode::Char('e')),
            (Some(1), KeyCode::Down), // "te" matches Light and Dark terminal.
            (Some(2), KeyCode::Char('x')),
            (None, KeyCode::Enter), // Empty results cannot be confirmed.
            (None, KeyCode::Backspace),
            (Some(1), KeyCode::Down),
            (Some(2), KeyCode::Enter),
        ]
        .into_iter();
        let mut input = futures_util::stream::poll_fn(|_| {
            let Some((expected, code)) = script.next() else {
                return Poll::Ready(None);
            };
            let background = expected.map_or(original.background(), |index| backgrounds[index]);
            assert_eq!(observed.borrow().last(), Some(&(expected, background)));
            Poll::Ready(Some(Ok(Event::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )))))
        });
        let items = vec![
            "Auto (recommended)".into(),
            "Light terminal".into(),
            "Dark terminal".into(),
        ];
        let action = PanelAction::ProviderSetup(items.clone());
        let selected = pick_list_with_preview(
            &mut shell,
            &mut input,
            OrdinarySurfaceMetadata::new("Terminal appearance"),
            items,
            vec![
                Some("neutral".into()),
                Some("daytime".into()),
                Some("nighttime".into()),
            ],
            0,
            action,
            |shell, index| {
                assert_eq!(shell.highlighted_panel_index(), index);
                shell.set_theme(index.map_or(&original, |index| &themes[index]).clone());
                observed
                    .borrow_mut()
                    .push((index, shell.theme().background()));
            },
        )
        .await
        .unwrap();

        assert_eq!(selected, Some(2));
        assert_eq!(shell.theme().background(), TerminalBackground::Dark);
        assert!(!shell.has_panel());
        assert_eq!(observed.borrow().len(), 12);
    }

    #[tokio::test]
    async fn ordinary_provider_picker_navigation_does_not_change_theme() {
        let mut shell = InteractiveShell::test_shell();
        let original = shell.theme();
        let mut input = tokio_stream::iter([
            Ok(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        ]);
        let selected = provider_setup_picker(
            &mut shell,
            &mut input,
            "Provider setup",
            vec!["one".into(), "two".into()],
            vec![None, None],
            0,
        )
        .await
        .unwrap();
        assert_eq!(selected, Some(1));
        assert_eq!(shell.theme().background(), original.background());
        assert_eq!(shell.theme().capabilities(), original.capabilities());
    }

    #[tokio::test]
    async fn model_choice_starts_at_first_result_on_every_open_and_keeps_current_marker() {
        let catalog = ModelCatalog::builtin().unwrap();
        let presentation = model_picker_presentation(&catalog);
        let current = presentation.ids.last().unwrap().clone();
        assert_ne!(current, presentation.ids[0]);
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("test", &current.0, "high");

        // Exercise the real model driver, without the user-config persistence
        // boundary. Provider headings never occupy a selectable index.
        for size in [(46, 8), (80, 24), (120, 40)] {
            shell.set_size(size.0, size.1);
            for (keys, expected) in [
                (vec![KeyCode::Enter], Some(presentation.ids[0].clone())),
                (
                    vec![KeyCode::Down, KeyCode::Enter],
                    Some(presentation.ids[1].clone()),
                ),
                (
                    "(current)"
                        .chars()
                        .map(KeyCode::Char)
                        .chain([KeyCode::Enter])
                        .collect(),
                    Some(current.clone()),
                ),
                (vec![KeyCode::Esc], None),
                // Reopening clears the previous filter and navigation state.
                (vec![KeyCode::Enter], Some(presentation.ids[0].clone())),
            ] {
                let mut input = tokio_stream::iter(
                    keys.into_iter()
                        .map(|key| Ok(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))),
                );
                assert_eq!(
                    pick_model_choice(&mut shell, &mut input, &catalog)
                        .await
                        .unwrap(),
                    expected,
                    "model selection at {size:?}"
                );
                assert!(!shell.has_panel());
                assert_eq!(
                    shell.selected_identity(),
                    (current.0.clone(), "high".into())
                );
                assert!(shell.debug_snapshot().is_empty());
                assert_eq!(shell.debug_error(), None);
            }
        }
    }

    #[tokio::test]
    async fn empty_model_choice_retains_the_availability_error() {
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending();
        assert_eq!(
            pick_model_choice(&mut shell, &mut input, &ModelCatalog::default())
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            shell.debug_error().as_deref(),
            Some("nothing is available to select")
        );
        assert!(!shell.has_panel());
    }

    #[tokio::test]
    async fn live_styled_document_rerenders_at_panel_content_width_after_resize() {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender.send(Ok(Event::Resize(44, 16))).await.unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        drop(sender);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 20);
        let widths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = std::sync::Arc::clone(&widths);

        read_only_document_live_styled(
            &mut shell,
            &mut input,
            "worker transcript",
            "initial".into(),
            move |width| {
                observed.lock().unwrap().push(width);
                std::future::ready(Ok(Some(format!("rendered at {width}"))))
            },
        )
        .await
        .unwrap();

        assert!(widths.lock().unwrap().contains(&44));
        assert!(!shell.has_panel());
    }

    #[tokio::test]
    async fn ctrl_d_closes_a_picker_and_propagates_the_close_request() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        drop(sender);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();

        let selected = pick_list(
            &mut shell,
            &mut input,
            OrdinarySurfaceMetadata::new("Choose"),
            vec!["one".into()],
            vec![None],
            0,
            PanelAction::SelectModel(vec![ModelId("one".into())]),
        )
        .await
        .unwrap();

        assert_eq!(selected, None);
        assert!(!shell.has_panel());
        assert!(shell.close_requested());
    }

    #[tokio::test]
    async fn message_picker_driver_returns_the_selected_message() {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Up,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        drop(sender);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();
        let selected = message_picker(
            &mut shell,
            &mut input,
            vec![
                ForkMessage {
                    entry_id: "entry-a".into(),
                    text: "first".into(),
                    whole_conversation: false,
                },
                ForkMessage {
                    entry_id: "entry-b".into(),
                    text: "second".into(),
                    whole_conversation: false,
                },
            ],
        )
        .await
        .unwrap();

        assert_eq!(selected, Some(("entry-a".into(), "first".into())));
        assert!(!shell.has_panel());
    }

    struct LivePickerRefresh {
        calls: usize,
    }

    fn refresh_live_picker(
        context: &mut LivePickerRefresh,
    ) -> Pin<Box<dyn Future<Output = SubagentPickerSnapshot> + '_>> {
        Box::pin(async move {
            context.calls += 1;
            SubagentPickerSnapshot {
                title: "Subagents · refreshed".into(),
                items: vec!["beta".into(), "gamma".into()],
                descriptions: vec![Some("done".into()), Some("running".into())],
                node_ids: vec!["node-b".into(), "node-c".into()],
                notices: Vec::new(),
            }
        })
    }

    #[tokio::test]
    async fn live_subagent_picker_refreshes_and_keeps_the_stable_selection() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        });
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();
        let mut refresh = LivePickerRefresh { calls: 0 };
        let selected = subagent_picker(
            &mut shell,
            &mut input,
            SubagentPickerSnapshot {
                title: "Subagents".into(),
                items: vec!["alpha".into(), "beta".into()],
                descriptions: vec![Some("running".into()), Some("running".into())],
                node_ids: vec!["node-a".into(), "node-b".into()],
                notices: Vec::new(),
            },
            1,
            &mut refresh,
            refresh_live_picker,
        )
        .await
        .unwrap();

        assert_eq!(selected.as_deref(), Some("node-b"));
        assert!(refresh.calls >= 1);
    }

    #[test]
    fn model_label_uses_friendly_metadata_without_wire_id_noise() {
        let spec = octet_ai::ModelSpec {
            id: ModelId("my-custom".into()),
            endpoint: octet_ai::EndpointId("local".into()),
            api_name: "llama-3.1-8b-instruct".into(),
            display_name: Some("Llama 3.1 8B".into()),
            protocol: octet_ai::Protocol::OpenAiChat,
            capabilities: octet_ai::Capabilities {
                input_modalities: octet_ai::ModalitySet::none(),
                output_modalities: octet_ai::ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: octet_ai::ModelLimits {
                context_window: 131072,
                max_output_tokens: 8192,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
        };
        assert_eq!(model_label(&spec), "Llama 3.1 8B");
        assert_eq!(model_picker_metadata(&spec).input_cost, "—");

        let mut priced = spec.clone();
        priced.pricing = Some(octet_ai::Pricing {
            input: octet_ai::TokenRate(1_000_000),
            output: octet_ai::TokenRate(6_000_000),
            cache_read: octet_ai::TokenRate(100_000),
            cache_write_5m: octet_ai::TokenRate(1_250_000),
            cache_write_1h: None,
            reasoning: None,
            tiers: Vec::new(),
        });
        assert_eq!(compact_rate_value(octet_ai::TokenRate(0)), "$0");
        assert_eq!(compact_rate_value(octet_ai::TokenRate(100_000_000)), "$100");
        assert_eq!(compact_context_limit(1_500_000), "1.5M");
        let metadata = model_picker_metadata(&priced);
        assert_eq!(metadata.input_cost, "$1/M");
        assert_eq!(metadata.output_cost, "$6/M");
        assert_eq!(metadata.context, "131K");
        assert_eq!(metadata.media, "");
    }

    #[test]
    fn custom_model_label_removes_provider_repository_and_quantization_noise() {
        let mut spec = octet_ai::ModelSpec {
            id: ModelId("custom/Intel/Qwen3.6-27B-int4-AutoRound".into()),
            endpoint: octet_ai::EndpointId("custom-openai".into()),
            api_name: "Intel/Qwen3.6-27B-int4-AutoRound".into(),
            display_name: None,
            protocol: octet_ai::Protocol::OpenAiChat,
            capabilities: octet_ai::Capabilities {
                input_modalities: octet_ai::ModalitySet::none(),
                output_modalities: octet_ai::ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: true,
                deferred_tool_loading: false,
            },
            limits: octet_ai::ModelLimits {
                context_window: 128000,
                max_output_tokens: 16384,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
        };
        assert_eq!(model_label(&spec), "Qwen3.6 27B");

        spec.capabilities.input_modalities = octet_ai::ModalitySet::none()
            .with(octet_ai::Modality::Image)
            .with(octet_ai::Modality::Audio);

        let metadata = model_picker_metadata(&spec);
        assert_eq!(metadata.media, "vision + audio");
    }

    #[test]
    fn model_picker_groups_and_sorts_models_with_stable_metadata_columns() {
        let catalog = ModelCatalog::builtin().unwrap();
        let presentation = model_picker_presentation(&catalog);
        let groups =
            presentation
                .providers
                .iter()
                .fold(Vec::<&str>::new(), |mut groups, provider| {
                    if groups.last().copied() != Some(provider.as_str()) {
                        groups.push(provider);
                    }
                    groups
                });
        assert_eq!(groups, vec!["Anthropic", "OpenAI"]);

        for provider in &groups {
            let labels = presentation
                .labels
                .iter()
                .zip(&presentation.providers)
                .filter(|(_, row_provider)| row_provider.as_str() == *provider)
                .map(|(label, _)| label.to_lowercase())
                .collect::<Vec<_>>();
            assert!(
                labels.windows(2).all(|pair| pair[0] <= pair[1]),
                "{provider} models were not alphabetized: {labels:?}"
            );
        }

        let descriptions = presentation
            .descriptions
            .iter()
            .map(|description| description.as_deref().unwrap())
            .collect::<Vec<_>>();
        let out_columns = descriptions
            .iter()
            .map(|description| {
                sexy_tui_rs::visible_width(&description[..description.find("out ").unwrap()])
            })
            .collect::<std::collections::BTreeSet<_>>();
        let context_columns = descriptions
            .iter()
            .map(|description| {
                sexy_tui_rs::visible_width(&description[..description.find(" ctx").unwrap()])
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(out_columns.len(), 1, "input costs are not one fixed column");
        assert_eq!(
            context_columns.len(),
            1,
            "context windows are not one fixed column"
        );
        assert!(descriptions
            .iter()
            .any(|description| description.contains("audio")));
        assert!(descriptions
            .iter()
            .any(|description| description.contains("vision")));

        let astra_index = presentation
            .ids
            .iter()
            .position(|id| id.0 == "gpt-6-astra")
            .expect("built-in Astra should use the generic model picker path");
        assert_eq!(presentation.providers[astra_index], "OpenAI");
        assert_eq!(presentation.labels[astra_index], "GPT-6 Astra");
        let astra_description = descriptions[astra_index];
        assert!(astra_description.contains("$10/M"));
        assert!(astra_description.contains("$50/M"));
        assert!(astra_description.contains("1.1M ctx"));
        assert!(astra_description.contains("vision"));

        assert!(descriptions.iter().all(|description| {
            !description.contains("tools")
                && !description.contains("reasoning")
                && !description.contains("Anthropic")
                && !description.contains("OpenAI")
        }));
    }
}
