//! Pi-compatible same-file navigation. The idle owner commits only after all
//! dialogs succeed; the ordinary picker and temporary input retain draft ownership.

use super::*;
use octet_agent::{CancellationToken, TreeNavigationResult};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TreeNavigationChoice {
    pub target: EntryId,
    pub summarize: bool,
    pub custom_instructions: Option<String>,
}

/// Cancellation returns to the preceding dialog, retaining the selected ID.
/// No dialog mutates either the session or the ordinary composer.
pub(super) async fn choose_navigation<S>(
    session: &Session,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<Option<TreeNavigationChoice>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut initial = None;
    loop {
        let Some(target) =
            pickers::session_tree_picker(shell, input, session, initial.as_ref()).await?
        else {
            return Ok(None);
        };
        if session.head_ref() == Some(&target) {
            shell.notice("Already at this point");
            return Ok(None);
        }
        initial = Some(target.clone());
        loop {
            let Some(choice) = pick_list_with_preview(
                shell,
                input,
                OrdinarySurfaceMetadata::with_purpose(
                    "Summarize branch?",
                    "Carry context from the branch being left into the selected point",
                ),
                vec![
                    "No summary".into(),
                    "Summarize".into(),
                    "Summarize with custom prompt".into(),
                ],
                vec![None; 3],
                0,
                PanelAction::ReadOnlyDocument,
                |_, _| {},
            )
            .await?
            else {
                if shell.close_requested() {
                    return Ok(None);
                }
                break;
            };
            let custom_instructions = if choice == 2 {
                let Some(instructions) = extension_input_picker(
                    shell,
                    input,
                    &ExtensionInputRequest {
                        parent_request_id: 0,
                        prompt: "Custom summarization instructions".into(),
                        secret: false,
                    },
                )
                .await?
                else {
                    if shell.close_requested() {
                        return Ok(None);
                    }
                    continue;
                };
                Some(instructions)
            } else {
                None
            };
            return Ok(Some(TreeNavigationChoice {
                target,
                summarize: choice != 0,
                custom_instructions,
            }));
        }
    }
}

/// Keep input and reverse extension UI requests alive even during provider
/// backoff and cooperative cancellation. Never drop the owner at Ctrl-C.
async fn drive_navigation<F, S>(
    work: F,
    cancellation: CancellationToken,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    extensions: &mut crate::extensions::ExecutableExtensions,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> Result<TreeNavigationResult, AgentError>
where
    F: Future<Output = Result<TreeNavigationResult, AgentError>>,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    tokio::pin!(work);
    let mut input_open = true;
    let mut events_open = true;
    let remote_ui_wake = extensions.remote_ui_wake();
    let mut frontend_tick = tokio::time::interval(Duration::from_millis(50));
    frontend_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if apply_extension_background(shell, extensions) {
            shell.render();
        }
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal(), if !cancellation.is_cancelled() => {
                shell.request_close();
                cancellation.cancel();
            }
            event = input.next(), if input_open => match event {
                Some(Ok(event)) => {
                    if extensions.route_remote_ui_event(shell, &event, false) { continue; }
                    // Unlike the ordinary editor's Ctrl-C, this cancels the
                    // operation without clearing a nonempty retained draft.
                    if matches!(&event, Event::Key(key) if is_ctrl_c(key)) {
                        cancellation.cancel();
                        continue;
                    }
                    if shell.intercept_transcript_input(&event) { continue; }
                    if paste_clipboard_text(shell, &event).await { continue; }
                    if handle_cancellable_wait_input(shell, event) { cancellation.cancel(); }
                }
                Some(Err(error)) => {
                    shell.error(format!("terminal input failed: {error}"));
                    shell.request_close();
                    input_open = false;
                    cancellation.cancel();
                }
                None => {
                    input_open = false;
                    cancellation.cancel();
                }
            },
            result = &mut work => return result,
            event = events.recv(), if events_open => match event {
                Some(AgentEvent::ProviderOperationRetry { attempt, error, .. }) => {
                    shell.notice(format!("Branch summary retry {attempt}: {error}"));
                    shell.render();
                }
                Some(event) => shell.on_cache_warming_event(&event),
                None => events_open = false,
            },
            _ = crate::extensions::remote_ui::notified(&remote_ui_wake) => {},
            _ = frontend_tick.tick() => {},
        }
    }
}

pub(super) async fn navigate<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<()>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let choice = present_host_dialog(
        &dialogs,
        "session_tree",
        choose_navigation(app.agent.session(), shell, input),
    )
    .await?;
    if let Some(choice) = choice {
        apply_navigation(app, shell, input, choice).await?;
    }
    Ok(())
}

pub(super) async fn apply_navigation<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
    choice: TreeNavigationChoice,
) -> anyhow::Result<()>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    if choice.summarize {
        if let Some(message) = cost_limit_message(app) {
            shell.error(message);
            return Ok(());
        }
    }
    let old_head = app.agent.session().head();
    shell.set_run_label(if choice.summarize {
        "summarizing branch…"
    } else {
        "navigating session…"
    });
    shell.render();
    let cancellation = CancellationToken::default();
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    let work = app.agent.navigate_session_tree_with_summary(
        choice.target,
        choice.summarize,
        choice.custom_instructions.as_deref(),
        cancellation.clone(),
        move |event| {
            let _ = sender.send(event);
        },
    );
    let result = drive_navigation(
        work,
        cancellation,
        &mut events,
        &mut app.executable_extensions,
        shell,
        input,
    )
    .await;
    shell.set_run_label("idle");
    // An after-hook can fail after the atomic durable commit. Hydrate the real
    // head on that error too, never pretend the old branch is still selected.
    if app.agent.session().head() != old_head {
        shell.hydrate(app.agent.session())?;
        app.executable_extensions.notify_session_info_changed_all();
    }
    match result {
        Ok(result) => {
            if let Some(text) = result.editor_text {
                shell.prefill_empty_editor(text);
            }
            shell.notice(if result.summary_entry.is_some() {
                "Navigated to selected point in this session with a branch summary"
            } else {
                "Navigated to selected point in this session"
            });
        }
        Err(AgentError::Cancelled) => shell.notice("Tree navigation cancelled"),
        Err(error) => shell.error(format!("tree navigation: {error}")),
    }
    // No rebuild or session replacement: keep model, cache policy, draft/chips
    // and extension processes. Refresh their context snapshots at the new head.
    request_extension_ui(shell, app);
    update_status(shell, app);
    shell.render();
    Ok(())
}

/// Portability operations settle their owned worker even after cancellation:
/// importing can already have published a local copy, and gh must be reaped.
pub(super) async fn drive_portability<F, T, S>(
    work: F,
    cancelled: &std::sync::atomic::AtomicBool,
    extensions: &mut crate::extensions::ExecutableExtensions,
    shell: &mut InteractiveShell,
    input: &mut S,
    label: &str,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    use std::sync::atomic::Ordering;
    tokio::pin!(work);
    let mut input_open = true;
    let remote_ui_wake = extensions.remote_ui_wake();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    shell.set_run_label(label);
    shell.render();
    loop {
        if apply_extension_background(shell, extensions) {
            shell.render();
        }
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal(), if !cancelled.load(Ordering::Acquire) => {
                shell.request_close();
                cancelled.store(true, Ordering::Release);
            }
            event = input.next(), if input_open => match event {
                Some(Ok(event)) => {
                    if matches!(&event, Event::Key(key) if is_ctrl_c(key)) {
                        cancelled.store(true, Ordering::Release);
                        continue;
                    }
                    if extensions.route_remote_ui_event(shell, &event, false) { continue; }
                    if shell.intercept_transcript_input(&event) { continue; }
                    if paste_clipboard_text(shell, &event).await { continue; }
                    if handle_cancellable_wait_input(shell, event) {
                        cancelled.store(true, Ordering::Release);
                    }
                }
                Some(Err(error)) => {
                    shell.error(format!("terminal input failed: {error}"));
                    shell.request_close();
                    input_open = false;
                    cancelled.store(true, Ordering::Release);
                }
                None => {
                    input_open = false;
                    cancelled.store(true, Ordering::Release);
                }
            },
            result = &mut work => {
                shell.set_run_label("idle");
                shell.render();
                return result;
            }
            _ = crate::extensions::remote_ui::notified(&remote_ui_wake) => {},
            _ = tick.tick() => {},
        }
    }
}

pub(super) async fn confirm_import<S>(
    app: &App,
    shell: &mut InteractiveShell,
    input: &mut S,
    source: &std::path::Path,
) -> anyhow::Result<bool>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let source = if source.is_absolute() {
        source.to_owned()
    } else {
        app.config.invocation_cwd.join(source)
    };
    let request = octet_agent::extension_process::ConfirmationRequest {
        parent_request_id: None,
        prompt: "Import this file and switch to a new private session?".into(),
        detail: Some(format!(
            "Source: {}\nDestination directory: {}\nA new session path and identity will be created. The source and current session are preserved. Unsupported semantics are refused.",
            source.display(), app.sessions.dir().display()
        )),
        destructive: false,
        default: false,
    };
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    present_host_dialog(
        &dialogs,
        "session_import",
        extension_confirmation_picker(shell, input, "octet", &request),
    )
    .await
}

/// Returns a destination only after import settles, never sends model input.
pub(super) async fn import<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
    source: PathBuf,
) -> anyhow::Result<Option<PathBuf>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    if !confirm_import(app, shell, input, &source).await? || shell.close_requested() {
        shell.notice("Import cancelled; original sessions unchanged");
        return Ok(None);
    }
    let store = app.sessions.clone();
    let cwd = app.config.invocation_cwd.clone();
    let task = tokio::task::spawn_blocking(move || {
        crate::session_commands::import_session(&store, &source, &cwd)
    });
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let result: anyhow::Result<crate::session_commands::SessionImportReport> = drive_portability(
        async move {
            task.await
                .map_err(|error| anyhow::anyhow!("import worker failed: {error}"))?
        },
        &cancelled,
        &mut app.executable_extensions,
        shell,
        input,
        "importing session…",
    )
    .await;
    match result {
        Ok(report) => {
            shell.notice(format!(
                "Imported {} as {} ({})",
                report.source_format,
                report.id,
                report.destination.display()
            ));
            for warning in report.warnings {
                shell.notice(warning);
            }
            if cancelled.load(std::sync::atomic::Ordering::Acquire) || shell.close_requested() {
                shell.notice(
                    "Switch cancelled; the new imported copy remains available via /resume",
                );
                Ok(None)
            } else {
                Ok(Some(report.destination))
            }
        }
        Err(error) => {
            shell.error(format!("import: {error}"));
            Ok(None)
        }
    }
}

pub(super) async fn confirm_share<S>(
    app: &App,
    shell: &mut InteractiveShell,
    input: &mut S,
    prepared: &crate::session_commands::PreparedShare,
) -> anyhow::Result<bool>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let request = octet_agent::extension_process::ConfirmationRequest {
        parent_request_id: None,
        prompt: "Publish this exact snapshot as an UNLISTED GitHub gist?".into(),
        detail: Some(format!(
            "{}\nReview file: {}\nSHA-256: {}\nRedacted values: {}",
            prepared.warning(),
            prepared.package_path().display(),
            prepared.sha256(),
            prepared.redaction_count()
        )),
        destructive: false,
        default: false,
    };
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    present_host_dialog(
        &dialogs,
        "session_share",
        extension_confirmation_picker(shell, input, "octet", &request),
    )
    .await
}

/// Busy dispatch queues only Share, not a snapshot or approval. This runs at
/// the fresh idle boundary against the then-current session and exact bytes.
pub(super) async fn share<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<()>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let id = app
        .agent
        .session()
        .path()
        .file_stem()
        .and_then(|id| id.to_str())
        .ok_or_else(|| anyhow::anyhow!("current session has no valid id"))?
        .to_owned();
    let store = app.sessions.clone();
    let task =
        tokio::task::spawn_blocking(move || crate::session_commands::prepare_share(&store, &id));
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let prepared = match drive_portability(
        async move {
            task.await
                .map_err(|error| anyhow::anyhow!("share preparation failed: {error}"))?
        },
        &cancelled,
        &mut app.executable_extensions,
        shell,
        input,
        "preparing redacted snapshot…",
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            shell.error(format!("share: {error}"));
            return Ok(());
        }
    };
    if cancelled.load(std::sync::atomic::Ordering::Acquire) || shell.close_requested() {
        shell.notice("Share cancelled; nothing was uploaded");
        return Ok(());
    }
    if !confirm_share(app, shell, input, &prepared).await? || shell.close_requested() {
        shell.notice("Share cancelled; nothing was uploaded");
        return Ok(());
    }
    match drive_portability(
        crate::session_commands::publish_share(prepared, true, &cancelled),
        &cancelled,
        &mut app.executable_extensions,
        shell,
        input,
        "publishing unlisted snapshot…",
    )
    .await
    {
        Ok(url) => shell.notice(format!(
            "Unlisted gist (anyone with the link can read): {url}"
        )),
        Err(error) => shell.error(format!("share: {error}")),
    }
    Ok(())
}
