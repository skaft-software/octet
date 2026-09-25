#![allow(missing_docs)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::tui::terminal::TerminalInput as EventStream;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use octet_agent::extension_process::{ConfirmationRequest, ExtensionInputRequest};
use octet_agent::tool::ToolConfirmation;
use octet_ai::{ModelCatalog, ModelId};

use crate::app::{bootstrap::CodexContextNotes, App};
use crate::config::ThinkingLevel;
use crate::modes::interactive::run_blocking_lifecycle;
use crate::presentation::{
    compact_context_limit, format_token_rate_value, provider_status_name, ModelDisplayMetadata,
};
use crate::session_store::{SessionMeta, SessionStorageLifecycle, SessionStore};
use crate::tui::view::{
    ForkMessage, InteractiveShell, MessagePicker, OrdinarySurfaceLifecycle,
    OrdinarySurfaceMetadata, OrdinarySurfaceStatus, Panel, PanelAction, PanelRequest, PanelResult,
    PickerState, SubagentGroup, SubagentPanel,
};

const MAX_SECRET_INPUT_BYTES: usize = 4096;
#[cfg(not(test))]
const SUBAGENT_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(test)]
const SUBAGENT_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Default)]
pub(crate) struct SecretInputBuffer(Vec<u8>);

impl SecretInputBuffer {
    pub(crate) fn push(&mut self, character: char) {
        let mut encoded = [0; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        if self.0.len().saturating_add(bytes.len()) <= MAX_SECRET_INPUT_BYTES {
            self.0.extend_from_slice(bytes);
        }
        encoded.fill(0);
    }

    pub(crate) fn extend_paste(&mut self, pasted: &str) {
        let pasted = pasted.trim_end_matches(['\r', '\n']);
        let remaining = MAX_SECRET_INPUT_BYTES.saturating_sub(self.0.len());
        let mut end = pasted.len().min(remaining);
        while end > 0 && !pasted.is_char_boundary(end) {
            end -= 1;
        }
        self.0.extend_from_slice(&pasted.as_bytes()[..end]);
    }

    pub(crate) fn backspace(&mut self) {
        let Some((start, _)) = std::str::from_utf8(&self.0)
            .ok()
            .and_then(|text| text.char_indices().last())
        else {
            return;
        };
        self.0[start..].fill(0);
        self.0.truncate(start);
    }

    pub(crate) fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for SecretInputBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Give one extension command exclusive ownership of terminal input. Secret
/// answers never enter the ordinary editor or rendered frame; non-secret setup
/// values use the same temporary composer surface and are echoed while typed.
pub async fn extension_input_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    request: &ExtensionInputRequest,
) -> anyhow::Result<Option<String>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    shell.set_tool_input_prompt(Some(request.prompt.clone()));
    shell.render();
    let mut value = SecretInputBuffer::default();
    let mut overflowed = false;
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => None,
            next = input.next() => next,
        };
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                shell.set_tool_input_prompt(None);
                shell.render();
                return Err(error.into());
            }
            None => {
                shell.set_tool_input_prompt(None);
                shell.render();
                return Ok(None);
            }
        };
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.set_tool_input_prompt(None);
            shell.request_close();
            shell.render();
            return Ok(None);
        }
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter if overflowed => {
                        // Never return a silently truncated credential. Clear the
                        // rejected input, then let the user paste a fresh value.
                        value = SecretInputBuffer::default();
                        overflowed = false;
                    }
                    KeyCode::Enter => {
                        let bytes = value.take();
                        let answer = String::from_utf8(bytes)
                            .map_err(|_| anyhow::anyhow!("extension input was not valid UTF-8"))?;
                        shell.set_tool_input_prompt(None);
                        shell.render();
                        return Ok(Some(answer));
                    }
                    KeyCode::Esc => {
                        shell.set_tool_input_prompt(None);
                        shell.render();
                        return Ok(None);
                    }
                    KeyCode::Backspace => value.backspace(),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        shell.set_tool_input_prompt(None);
                        shell.render();
                        return Ok(None);
                    }
                    KeyCode::Char(character)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        overflowed |= value.0.len().saturating_add(character.len_utf8())
                            > MAX_SECRET_INPUT_BYTES;
                        value.push(character)
                    }
                    _ => {}
                }
            }
            Event::Paste(pasted) => {
                overflowed |= value
                    .0
                    .len()
                    .saturating_add(pasted.trim_end_matches(['\r', '\n']).len())
                    > MAX_SECRET_INPUT_BYTES;
                value.extend_paste(&pasted);
            }
            Event::Resize(columns, rows) => shell.set_size(columns, rows),
            _ => {}
        }
        let shown = if overflowed {
            format!(
                "{} [input exceeds 4 KiB; Enter to clear, Esc to cancel]",
                request.prompt
            )
        } else if request.secret {
            request.prompt.clone()
        } else {
            let entered = std::str::from_utf8(&value.0).unwrap_or_default();
            format!("{} {}", request.prompt, entered)
        };
        shell.set_tool_input_prompt(Some(shown));
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
    /// Declared state groups, aligned with `items` by index. Group headings are
    /// panel chrome and never selectable rows.
    pub groups: Vec<SubagentGroup>,
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
        action: PanelAction::SelectSubagent(SubagentPanel {
            node_ids: initial.node_ids,
            groups: initial.groups,
            // Terminal groups start collapsed so finished workers cannot bury
            // live ones; ctrl+t toggles them back on.
            collapsed: true,
            revealed_node: None,
            state_filter: None,
        }),
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
                        (PanelResult::Confirm(index), PanelAction::SelectSubagent(panel)) => {
                            panel.node_ids.get(index).cloned()
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
                    SubagentPanel {
                        node_ids: snapshot.node_ids,
                        groups: snapshot.groups,
                        collapsed: true,
                        revealed_node: None,
                        state_filter: None,
                    },
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
        text: crate::tui::view::sanitize_for_terminal(&text).into(),
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
        text: text.into(),
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
                PanelRequest::SearchEntries { query, paths } => {
                    let search_store = store.clone();
                    let search_query = query.clone();
                    let result = run_blocking_lifecycle(
                        shell,
                        input,
                        "searching session transcripts…",
                        move || search_picker_entries(&search_store, &search_query, &paths),
                    )
                    .await;
                    match result {
                        Ok(hits) => {
                            let count = hits.len();
                            shell.set_picker_entry_search(query, hits);
                            shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::success(
                                format!("{count} sessions matched (up to 200 entry hits; edit query for metadata search)"),
                                Instant::now() + Duration::from_secs(5),
                            ));
                        }
                        Err(error) => {
                            shell.set_picker_lifecycle(OrdinarySurfaceLifecycle::recoverable_error(
                                format!("transcript search: {error}"),
                                Instant::now() + Duration::from_secs(5),
                            ))
                        }
                    }
                    shell.render();
                }
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

/// Reuse the incremental, bounded session-entry index instead of reopening
/// transcripts on the render/input thread. Only currently offered picker paths
/// can become results. The aggregate hit and workspace budgets are explicit.
fn search_picker_entries(
    store: &SessionStore,
    query: &str,
    paths: &[PathBuf],
) -> anyhow::Result<std::collections::HashMap<PathBuf, String>> {
    anyhow::ensure!(query.len() <= 1024, "query exceeds 1024 bytes");
    let mut directories = std::collections::BTreeSet::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            directories.insert(parent.to_path_buf());
        }
    }
    anyhow::ensure!(
        directories.len() <= 256,
        "search is limited to 256 workspace stores"
    );
    let offered = paths
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut hits = std::collections::HashMap::new();
    let mut remaining = 200;
    for directory in directories {
        if remaining == 0 {
            break;
        }
        let scope = SessionStore::for_directory(&directory, store.root());
        let result = scope.search_entries(query, remaining)?;
        remaining = remaining.saturating_sub(result.hits.len());
        for hit in result.hits {
            let path = directory.join(format!("{}.jsonl", hit.session_id));
            if offered.contains(&path) {
                hits.entry(path).or_insert(hit.text);
            }
        }
    }
    Ok(hits)
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
///
/// On a Codex route the effort menu also carries one trailing
/// `Codex context window…` row. It is appended after the effort levels so every
/// level keeps its existing index, and it is never a level: choosing it opens the
/// read-only Codex context-window surface.
pub async fn thinking_picker(
    shell: &mut InteractiveShell,
    input: &mut EventStream,
    levels: &[ThinkingLevel],
    codex_context: Option<&crate::commands::CodexContextSurface>,
) -> anyhow::Result<Option<ThinkingLevel>> {
    let mut items: Vec<String> = levels.iter().map(|l| l.label().into()).collect();
    if let Some(surface) = codex_context {
        items.push(codex_context_menu_row(surface));
    }
    loop {
        let mut items = items.clone();
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
            vec![None; levels.len() + usize::from(codex_context.is_some())],
            initial,
            PanelAction::SelectThinking(action_levels),
        )
        .await?
        else {
            return Ok(None);
        };
        let Some(selected) = levels.get(index).copied() else {
            // The trailing Codex context-window row: report the facts and the
            // fail-closed raise path, then return to the effort list.
            if let Some(surface) = codex_context {
                codex_context_menu(shell, input, surface).await?;
                continue;
            }
            return Ok(None);
        };
        // Selection is not acceptance: the owning idle dispatcher validates
        // session-dependent controls before persisting the preference.
        return Ok(Some(selected));
    }
}

/// One-line Codex context-window row for the effort menu.
pub(crate) fn codex_context_menu_row(surface: &crate::commands::CodexContextSurface) -> String {
    format!(
        "Codex context window… (currently {} tokens{})",
        surface.effective_window(),
        if surface.has_uncertain_usage() {
            ", cost/usage UNCERTAIN"
        } else {
            ""
        }
    )
}

/// Present the read-only Codex context-window facts and, when the plan is
/// entitled, one explicit raise target.
///
/// Raising is deliberately a two-step, fail-closed interaction: the
/// acknowledgement wording must be accepted before the request is validated,
/// and a refused request names the exact reason and changes nothing.
pub(crate) async fn codex_context_menu<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    surface: &crate::commands::CodexContextSurface,
) -> anyhow::Result<()>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let facts = surface.summary_lines();
    let target = surface.raise_target();
    let mut items: Vec<String> = vec![format!(
        "Keep the deliberate {} window",
        surface.effective_window()
    )];
    if let Some(target) = target {
        items.push(format!(
            "Acknowledge and raise to {target} tokens (double-priced; websocket risk)"
        ));
    }
    let descriptions: Vec<Option<String>> = items
        .iter()
        .enumerate()
        .map(|(index, _)| {
            (index == 1 && target.is_some())
                .then(|| crate::codex_context::CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING.to_owned())
                .or_else(|| (index == 0).then(|| "no change".to_owned()))
                .or_else(|| Some(facts.join(" · ")))
        })
        .collect();
    let purpose = facts.join(" · ");
    let Some(index) = pick_list_with_preview(
        shell,
        input,
        OrdinarySurfaceMetadata::with_purpose("Codex context window", purpose),
        items,
        descriptions,
        0,
        PanelAction::ReadOnlyDocument,
        |_, _| {},
    )
    .await?
    else {
        return Ok(());
    };
    let Some(target) = target else {
        return Ok(());
    };
    if index != 1 {
        return Ok(());
    }
    // The acknowledgement stage names both consequences before the raise is
    // validated; the raise itself still fails closed on any refused gate.
    shell.notice(format!(
        "{} Set acknowledged for the launch boundary.",
        crate::codex_context::CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING
    ));
    match surface.raise(target, true) {
        Ok(window) => shell.notice(format!(
            "Codex context window raise to {window} tokens accepted: {}",
            surface.raise_instruction(target)
        )),
        Err(reason) => shell.error(format!(
            "Codex context window unchanged at {} tokens: {reason}",
            surface.effective_window()
        )),
    }
    shell.render();
    Ok(())
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
    )
    .await
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
    let prompt = format!("{extension}: {}", request.prompt);
    confirmation_prompt_picker(
        shell,
        input,
        &prompt,
        request.detail.as_deref(),
        request.destructive,
        request.default,
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
    let title = if destructive {
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
        PanelAction::Confirmation,
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
pub(crate) struct ModelPickerPresentation {
    pub(crate) ids: Vec<ModelId>,
    pub(crate) providers: Vec<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) descriptions: Vec<Option<String>>,
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

pub(crate) fn model_picker_presentation(catalog: &ModelCatalog) -> ModelPickerPresentation {
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

/// A deferred full inventory belongs to the idle picker owner. Dropping the
/// handle on cancel never applies its result to a later app or selection.
pub(crate) type DeferredModelCatalog =
    tokio::task::JoinHandle<anyhow::Result<(ModelCatalog, CodexContextNotes)>>;

/// Open the launch catalog immediately, then refresh the same panel if the
/// deferred fleet inventory succeeds. Terminal input owns confirmation and
/// cancellation even while provider discovery is still running.
pub(crate) async fn optional_model_picker_live<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    app: &mut App,
    mut pending: Option<DeferredModelCatalog>,
) -> anyhow::Result<Option<ModelId>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let expected_model = app.model.spec.id.clone();
    let mut presentation = model_picker_presentation(&app.catalog);
    if presentation.ids.is_empty() {
        shell.error("nothing is available to select".into());
        shell.render();
        return Ok(None);
    }
    mark_current_choice(
        &mut presentation.labels,
        presentation.ids.iter().position(|id| *id == expected_model),
    );
    let mut surface = OrdinarySurfaceMetadata::with_purpose(
        "Select model",
        "Choose the model for subsequent prompts and the startup default",
    );
    if pending.is_some() {
        surface.lifecycle = OrdinarySurfaceLifecycle::Loading(OrdinarySurfaceStatus::persistent(
            "loading other providers",
        ));
    }
    shell.open_panel(Panel::SelectList {
        surface,
        items: presentation.labels,
        descriptions: presentation.descriptions,
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectGroupedModel {
            models: presentation.ids,
            providers: presentation.providers,
        },
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
            result = async { pending.as_mut().expect("pending catalog").await }, if pending.is_some() => {
                pending = None;
                match result {
                    Ok(Ok((catalog, notes))) => match app.apply_picker_catalog(&expected_model, catalog, notes) {
                        Ok(true) => {
                            shell.set_model_cycle(app.model_cycle());
                            let mut presentation = model_picker_presentation(&app.catalog);
                            mark_current_choice(
                                &mut presentation.labels,
                                presentation.ids.iter().position(|id| *id == expected_model),
                            );
                            shell.refresh_panel_models(
                                presentation.labels,
                                presentation.descriptions,
                                presentation.ids,
                                presentation.providers,
                            );
                        }
                        Ok(false) => {} // The launch identity is no longer current.
                        Err(error) => shell.set_model_picker_lifecycle(
                            OrdinarySurfaceLifecycle::RecoverableError(
                                OrdinarySurfaceStatus::persistent(format!(
                                    "could not load every provider: {error}"
                                )),
                            ),
                        ),
                    },
                    Ok(Err(error)) => shell.set_model_picker_lifecycle(
                        OrdinarySurfaceLifecycle::RecoverableError(
                            OrdinarySurfaceStatus::persistent(format!(
                                "could not load every provider: {error}; showing current routes"
                            )),
                        ),
                    ),
                    Err(error) => shell.set_model_picker_lifecycle(
                        OrdinarySurfaceLifecycle::RecoverableError(
                            OrdinarySurfaceStatus::persistent(format!(
                                "provider discovery stopped: {error}; showing current routes"
                            )),
                        ),
                    ),
                }
                shell.render();
                continue;
            }
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
        if matches!(event, Event::Mouse(_)) {
            continue;
        }
        if let Some((result, action)) = shell.panel_input(&event) {
            shell.render();
            let selected = match (result, action) {
                (PanelResult::Confirm(index), PanelAction::SelectGroupedModel { models, .. }) => {
                    models.get(index).cloned()
                }
                _ => None,
            };
            if let Some(id) = &selected {
                if let Err(error) = crate::cli::persist_model(&id.0) {
                    shell.error(format!("failed to save model preference: {error}"));
                }
            }
            return Ok(selected);
        }
        shell.render();
    }
}

/// Ask the user to select one model, preserving cancellation for workflows
/// such as `/logout` that must not mutate credentials until a replacement model
/// has been chosen.
pub async fn optional_model_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    catalog: &ModelCatalog,
) -> anyhow::Result<Option<ModelId>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
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
pub async fn model_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    catalog: &ModelCatalog,
) -> anyhow::Result<ModelId>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    optional_model_picker(shell, input, catalog)
        .await?
        .ok_or_else(|| anyhow::anyhow!("model selection cancelled"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    #[tokio::test]
    async fn secret_input_paste_never_enters_the_composer_or_transcript() {
        let mut shell = InteractiveShell::test_shell();
        shell.extension_set_editor("draft kept intact".into());
        let request = ExtensionInputRequest {
            parent_request_id: 0,
            prompt: "API key (input hidden)".into(),
            secret: true,
        };
        let mut input = futures_util::stream::iter([
            Ok(Event::Paste("synthetic-private-key\r\n".into())),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        ]);
        let answer = extension_input_picker(&mut shell, &mut input, &request)
            .await
            .unwrap();
        assert_eq!(answer.as_deref(), Some("synthetic-private-key"));
        assert_eq!(shell.pending(), "draft kept intact");
        let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
        assert!(!frame.contains("synthetic-private-key"));
        assert!(!frame.contains("API key (input hidden)"));
    }

    #[tokio::test]
    async fn secret_input_cancel_and_input_error_discard_the_answer_and_restore_editor() {
        for end in [
            Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
            Err(std::io::Error::other("synthetic input failure")),
        ] {
            let failed = end.is_err();
            let mut shell = InteractiveShell::test_shell();
            shell.extension_set_editor("original draft".into());
            let request = ExtensionInputRequest {
                parent_request_id: 0,
                prompt: "API key (input hidden)".into(),
                secret: true,
            };
            let mut input =
                futures_util::stream::iter([Ok(Event::Paste("synthetic-private-key".into())), end]);
            let answer = extension_input_picker(&mut shell, &mut input, &request).await;
            if failed {
                assert!(answer.is_err());
            } else {
                assert_eq!(answer.unwrap(), None);
            }
            assert_eq!(shell.pending(), "original draft");
            let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
            assert!(!frame.contains("synthetic-private-key"));
            assert!(!frame.contains("API key (input hidden)"));
        }
    }

    #[tokio::test]
    async fn oversized_secret_input_cannot_submit_a_truncated_key() {
        for oversized in [
            vec![Ok(Event::Paste("x".repeat(MAX_SECRET_INPUT_BYTES + 1)))],
            vec![
                Ok(Event::Paste("x".repeat(MAX_SECRET_INPUT_BYTES))),
                Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char('y'),
                    KeyModifiers::NONE,
                ))),
            ],
        ] {
            let mut shell = InteractiveShell::test_shell();
            let request = ExtensionInputRequest {
                parent_request_id: 0,
                prompt: "API key (input hidden)".into(),
                secret: true,
            };
            let mut events = oversized;
            events.extend([
                Ok(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))),
                Ok(Event::Paste("replacement-key".into())),
                Ok(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))),
            ]);
            let answer = extension_input_picker(
                &mut shell,
                &mut futures_util::stream::iter(events),
                &request,
            )
            .await
            .unwrap();
            assert_eq!(answer.as_deref(), Some("replacement-key"));
            assert!(shell.pending_is_empty());
        }
    }

    #[test]
    fn secret_input_is_utf8_bounded_and_backspace_removes_one_character() {
        let mut value = SecretInputBuffer::default();
        value.extend_paste(&"x".repeat(MAX_SECRET_INPUT_BYTES - 1));
        value.push('é');
        assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES - 1);
        value.push('!');
        assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES);
        value.backspace();
        value.backspace();
        value.extend_paste("é\r\n");
        assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES);
        assert!(std::str::from_utf8(&value.0).unwrap().ends_with('é'));
        value.backspace();
        assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES - 2);
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
        refreshed: Option<tokio::sync::oneshot::Sender<()>>,
    }

    fn refresh_live_picker(
        context: &mut LivePickerRefresh,
    ) -> Pin<Box<dyn Future<Output = SubagentPickerSnapshot> + '_>> {
        Box::pin(async move {
            context.calls += 1;
            if let Some(refreshed) = context.refreshed.take() {
                let _ = refreshed.send(());
            }
            SubagentPickerSnapshot {
                title: "Subagents · refreshed".into(),
                items: vec!["beta".into(), "gamma".into()],
                descriptions: vec![Some("done".into()), Some("running".into())],
                node_ids: vec!["node-b".into(), "node-c".into()],
                groups: vec![
                    SubagentGroup {
                        label: "Running".into(),
                        indices: vec![1],
                        collapsible: false,
                    },
                    SubagentGroup {
                        label: "Done".into(),
                        indices: vec![0],
                        collapsible: true,
                    },
                ],
                notices: Vec::new(),
            }
        })
    }

    #[tokio::test]
    async fn live_subagent_picker_refreshes_and_keeps_the_stable_selection() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let (refreshed_tx, refreshed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            refreshed_rx
                .await
                .expect("picker refreshed before confirmation");
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
        let mut refresh = LivePickerRefresh {
            calls: 0,
            refreshed: Some(refreshed_tx),
        };
        let selected = subagent_picker(
            &mut shell,
            &mut input,
            SubagentPickerSnapshot {
                title: "Subagents".into(),
                items: vec!["alpha".into(), "beta".into()],
                descriptions: vec![Some("running".into()), Some("running".into())],
                node_ids: vec!["node-a".into(), "node-b".into()],
                groups: vec![SubagentGroup {
                    label: "Running".into(),
                    indices: vec![0, 1],
                    collapsible: false,
                }],
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
            preset: Default::default(),
            id: ModelId("my-custom".into()),
            endpoint: octet_ai::EndpointId("local".into()),
            api_name: "llama-3.1-8b-instruct".into(),
            display_name: Some("Llama 3.1 8B".into()),
            protocol: octet_ai::Protocol::OpenAiChat,
            capabilities: octet_ai::Capabilities {
                responses_features: Default::default(),
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
            preset: Default::default(),
            id: ModelId("custom/Intel/Qwen3.6-27B-int4-AutoRound".into()),
            endpoint: octet_ai::EndpointId("custom-openai".into()),
            api_name: "Intel/Qwen3.6-27B-int4-AutoRound".into(),
            display_name: None,
            protocol: octet_ai::Protocol::OpenAiChat,
            capabilities: octet_ai::Capabilities {
                responses_features: Default::default(),
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

#[cfg(test)]
mod parity_session_search_tests {
    use super::*;
    use octet_agent::{EntryValue, Session};
    use octet_ai::{Message, UserMessage, UserPart};

    #[test]
    fn resume_transcript_search_dispatch_uses_index_and_returns_original_session() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        for id in ["one", "two"] {
            let mut session = Session::create(store.dir().join(format!("{id}.jsonl"))).unwrap();
            for text in [
                "ordinary first prompt",
                if id == "two" {
                    "deep hidden needle"
                } else {
                    "other later text"
                },
            ] {
                session
                    .append(EntryValue::Message(Message::User(UserMessage {
                        content: vec![UserPart::Text(text.into())],
                    })))
                    .unwrap();
            }
        }
        let rows = store.list();
        assert_eq!(rows.len(), 2);
        let expected = store.dir().join("two.jsonl");
        let mut shell = InteractiveShell::test_shell();
        shell.open_panel(Panel::SessionPicker {
            picker: PickerState::new(rows, None),
        });
        for character in "needle".chars() {
            shell.panel_input(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
        }
        shell.panel_input(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        let mut requests = shell.drain_panel_requests();
        assert_eq!(requests.len(), 1);
        let PanelRequest::SearchEntries { query, paths } = requests.remove(0) else {
            panic!("search request");
        };
        let hits = search_picker_entries(&store, &query, &paths).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits.get(&expected).unwrap().contains("deep hidden needle"));
        shell.set_picker_entry_search(query.clone(), hits);
        let repeated = search_picker_entries(&store, &query, &paths).unwrap();
        assert_eq!(repeated.len(), 1);
        let selected = shell.panel_input(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(selected, Some((PanelResult::Select(ref id), _)) if id == "two"));
        assert_eq!(
            shell.take_picker_selection(),
            Some(("two".into(), expected))
        );
        assert!(search_picker_entries(&store, &"x".repeat(1025), &paths).is_err());
    }
}

#[cfg(test)]
mod deferred_model_picker_tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    #[tokio::test]
    async fn deferred_model_picker_applies_ready_catalog_before_later_input() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let active = app.model.spec.id.clone();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        let (catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
            app.config.offline,
            &crate::app::bootstrap::CatalogReadiness::Fleet,
        )
        .unwrap();
        let pending = tokio::spawn(async move { Ok((catalog, notes)) });
        while !pending.is_finished() {
            tokio::task::yield_now().await;
        }
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();
        {
            let future =
                optional_model_picker_live(&mut shell, &mut input, &mut app, Some(pending));
            tokio::pin!(future);
            assert!(matches!(
                futures_util::poll!(future.as_mut()),
                std::task::Poll::Pending
            ));
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
            assert!(future.await.unwrap().is_none());
        }
        assert!(app.readiness.is_fleet());
        assert_eq!(app.model.spec.id, active);
        assert!(!shell.has_panel());
    }

    #[tokio::test]
    async fn deferred_model_picker_accepts_input_and_escape_before_inventory_finishes() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let initial = app.catalog.models().count();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        let pending = tokio::spawn(async move {
            waiting.await.unwrap();
            crate::app::bootstrap::model_catalog_for_readiness(
                true,
                &crate::app::bootstrap::CatalogReadiness::Fleet,
            )
        });
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('g'),
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(500),
            optional_model_picker_live(&mut shell, &mut input, &mut app, Some(pending)),
        )
        .await
        .expect("picker input must not wait for discovery")
        .unwrap()
        .is_none());
        assert!(!app.readiness.is_fleet());
        assert_eq!(app.catalog.models().count(), initial);
        assert!(!shell.has_panel());
        release.send(()).unwrap();
    }

    #[tokio::test]
    async fn deferred_model_picker_failure_keeps_current_routes_and_stays_cancellable() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let initial = app.catalog.models().count();
        app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
        let pending = tokio::spawn(async { anyhow::bail!("inventory unavailable") });
        while !pending.is_finished() {
            tokio::task::yield_now().await;
        }
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut input = ReceiverStream::new(receiver);
        let mut shell = InteractiveShell::test_shell();
        {
            let future =
                optional_model_picker_live(&mut shell, &mut input, &mut app, Some(pending));
            tokio::pin!(future);
            assert!(matches!(
                futures_util::poll!(future.as_mut()),
                std::task::Poll::Pending
            ));
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
            assert!(future.await.unwrap().is_none());
        }
        assert!(!app.readiness.is_fleet());
        assert_eq!(app.catalog.models().count(), initial);
        assert!(!shell.has_panel());
    }
}
