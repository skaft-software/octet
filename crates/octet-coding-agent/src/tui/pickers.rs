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
    pub(crate) fn byte_len(&self) -> usize {
        self.0.len()
    }

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
        if self.0.len().saturating_add(pasted.len()) <= MAX_SECRET_INPUT_BYTES {
            self.0.extend_from_slice(pasted.as_bytes());
        }
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

#[cfg(test)]
mod temporary_input_tests {
    use super::{SecretInputBuffer, MAX_SECRET_INPUT_BYTES};

    #[test]
    fn secret_over_limit_paste_is_rejected_atomically() {
        let mut value = SecretInputBuffer::default();
        value.extend_paste("original");
        value.extend_paste(&"🦀".repeat(MAX_SECRET_INPUT_BYTES));
        assert_eq!(value.take().as_slice(), b"original");
        value.extend_paste(&"a".repeat(MAX_SECRET_INPUT_BYTES));
        value.push('雪');
        assert_eq!(value.take().len(), MAX_SECRET_INPUT_BYTES);
    }
}

/// Give one extension command exclusive ownership of terminal input. Secrets
/// remain host-private; ordinary values use a separate bounded editor shared
/// by raw and native input without changing the parent draft or chips.
pub async fn extension_input_picker<S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    request: &ExtensionInputRequest,
) -> anyhow::Result<Option<String>>
where
    S: futures_util::Stream<Item = std::io::Result<Event>> + Unpin,
{
    use sexy_tui_rs::TextEditAction;

    shell.begin_tool_input(&request.prompt, request.secret);
    shell.render();
    let mut value = SecretInputBuffer::default();
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => None,
            next = input.next() => next,
        };
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                shell.end_tool_input();
                shell.render();
                return Err(error.into());
            }
            None => {
                shell.end_tool_input();
                shell.render();
                return Ok(None);
            }
        };
        let event = if request.secret {
            event
        } else {
            shell.tool_input_event(&event)
        };
        if matches!(&event, Event::Key(key) if crate::tui::keymap::is_close_key(key)) {
            shell.end_tool_input();
            shell.request_close();
            shell.render();
            return Ok(None);
        }
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter if shell.tool_input_overflowed() => {
                        // Rejected input is never submitted, even after a later edit.
                        value = SecretInputBuffer::default();
                        shell.clear_tool_input_value();
                    }
                    KeyCode::Enter => {
                        let answer = if request.secret {
                            // All buffer mutations accept valid UTF-8 scalars/paste.
                            String::from_utf8(value.take()).expect("secret input is valid UTF-8")
                        } else {
                            shell
                                .end_tool_input()
                                .expect("ordinary request owns an editor")
                        };
                        if request.secret {
                            shell.end_tool_input();
                        }
                        shell.render();
                        return Ok(Some(answer));
                    }
                    KeyCode::Esc => {
                        shell.end_tool_input();
                        shell.render();
                        return Ok(None);
                    }
                    KeyCode::Backspace if request.secret => value.backspace(),
                    KeyCode::Backspace => shell.edit_tool_input(TextEditAction::Backspace),
                    KeyCode::Delete if !request.secret => {
                        shell.edit_tool_input(TextEditAction::Delete)
                    }
                    KeyCode::Left if !request.secret => shell.edit_tool_input(TextEditAction::Left),
                    KeyCode::Right if !request.secret => {
                        shell.edit_tool_input(TextEditAction::Right)
                    }
                    KeyCode::Home if !request.secret => shell.edit_tool_input(TextEditAction::Home),
                    KeyCode::End if !request.secret => shell.edit_tool_input(TextEditAction::End),
                    KeyCode::Up if !request.secret => shell.edit_tool_input(TextEditAction::Up),
                    KeyCode::Down if !request.secret => shell.edit_tool_input(TextEditAction::Down),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        shell.end_tool_input();
                        shell.render();
                        return Ok(None);
                    }
                    KeyCode::Char(character)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        if request.secret {
                            if value.0.len().saturating_add(character.len_utf8())
                                > MAX_SECRET_INPUT_BYTES
                            {
                                shell.mark_tool_input_overflow();
                            }
                            value.push(character);
                        } else {
                            shell.edit_tool_input(TextEditAction::Char(character));
                        }
                    }
                    _ => {}
                }
            }
            Event::Paste(pasted) => {
                let pasted = pasted.trim_end_matches(['\r', '\n']);
                if request.secret {
                    if value.0.len().saturating_add(pasted.len()) > MAX_SECRET_INPUT_BYTES {
                        shell.mark_tool_input_overflow();
                    }
                    value.extend_paste(pasted);
                } else {
                    shell.edit_tool_input(TextEditAction::Paste(pasted.to_owned()));
                }
            }
            Event::Resize(columns, rows) => shell.set_size(columns, rows),
            _ => {}
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
        picker: PickerState::new(rows.clone(), current_session_path.clone()),
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
                            target_store.rename(&id, &name)
                        })
                        .await;
                    match result {
                        Ok(metadata) => {
                            if current_session_path.as_deref() == Some(path.as_path()) {
                                shell.set_session_name(metadata.name.as_deref());
                            }
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

/// Public presentation facts only; endpoint headers and model presets never enter TSP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelPickerDetail {
    pub(crate) name: String,
    pub(crate) context: u64,
    pub(crate) output: u64,
    pub(crate) price: String,
    pub(crate) cache_price: Option<String>,
    pub(crate) input: String,
    pub(crate) badges: Vec<String>,
    pub(crate) source: Vec<(String, String)>,
}

fn native_model_detail(model: &octet_ai::ModelSpec) -> ModelPickerDetail {
    use octet_ai::Modality;
    let caps = &model.capabilities;
    let mut badges = Vec::new();
    if caps.reasoning.is_some() {
        badges.push("reasoning".into());
    }
    if caps.input_modalities.contains(Modality::Image) {
        badges.push("vision".into());
    }
    if caps.input_modalities.contains(Modality::Audio) {
        badges.push("audio".into());
    }
    if caps.tools {
        badges.push("tools".into());
    }
    if caps.structured_output {
        badges.push("structured output".into());
    }
    let input = [(Modality::Image, "image"), (Modality::Audio, "audio")]
        .into_iter()
        .filter(|(modality, _)| caps.input_modalities.contains(*modality))
        .map(|(_, label)| label)
        .collect::<Vec<_>>()
        .join(" · ");
    let input = if input.is_empty() {
        "text".into()
    } else {
        format!("text · {input}")
    };
    let metadata =
        octet_ai::model_metadata::model_capability_metadata(&model.endpoint.0, &model.api_name);
    let source = ["knowledge", "release_date", "last_updated", "open_weights"]
        .into_iter()
        .filter_map(|key| {
            let value = metadata.as_ref()?.get(key)?;
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Bool(value) => value.to_string(),
                _ => return None,
            };
            Some((key.to_owned(), value))
        })
        .collect();
    ModelPickerDetail {
        name: model_label(model),
        context: model.limits.context_window,
        output: model.limits.max_output_tokens,
        price: model.pricing.as_ref().map_or_else(
            || "—".into(),
            |pricing| {
                format!(
                    "{} · {}",
                    compact_rate_value(pricing.input),
                    compact_rate_value(pricing.output)
                )
            },
        ),
        cache_price: model
            .pricing
            .as_ref()
            .map(|pricing| compact_rate_value(pricing.cache_read)),
        input,
        badges,
        source,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelPickerPresentation {
    pub(crate) ids: Vec<ModelId>,
    pub(crate) providers: Vec<String>,
    pub(crate) details: Vec<ModelPickerDetail>,
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
                native_model_detail(model),
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
        details: Vec::with_capacity(rows.len()),
        labels: Vec::with_capacity(rows.len()),
        descriptions: Vec::with_capacity(rows.len()),
    };
    for (provider, label, id, metadata, detail) in rows {
        presentation.details.push(detail);
        let media = if metadata.media.is_empty() {
            String::new()
        } else {
            format!("  {}", metadata.media)
        };
        // OAuth / subscription models report no pricing (`—`). Showing
        // `in — out —` on every such row is visual noise; keep the context
        // column stable with blank padding so priced rows stay tabular.
        let unknown_pricing = metadata.input_cost == "—" && metadata.output_cost == "—";
        let description = if unknown_pricing {
            format!(
                "{}{} ctx{media}",
                " ".repeat(11 + input_width + output_width),
                pad_visible_left(&metadata.context, context_width),
            )
        } else {
            format!(
                "in {}  out {}  {} ctx{media}",
                pad_visible_right(&metadata.input_cost, input_width),
                pad_visible_right(&metadata.output_cost, output_width),
                pad_visible_left(&metadata.context, context_width),
            )
        };
        presentation.ids.push(id);
        presentation.providers.push(provider);
        presentation.labels.push(label);
        presentation.descriptions.push(Some(description));
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
            details: presentation.details,
            scope: None,
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
                                presentation.details,
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
            details: presentation.details,
            scope: None,
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
mod deferred_model_picker_tests;
#[cfg(test)]
mod parity_session_search_tests;
#[cfg(test)]
mod tests;
