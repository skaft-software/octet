//! The existing input owner pumps reverse UI while the resource phase waits.
use super::*;

async fn pump<F, T, S>(
    shell: &mut InteractiveShell,
    input: &mut S,
    app: &mut App,
    future: F,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    tokio::pin!(future);
    let wake = app.executable_extensions.remote_ui_wake();
    // A drained session/append_entry, active-tools or system-prompt request
    // needs the live Agent, not just the shell queue. Keep the existing owner
    // applying it while a hook awaits the response.
    request_extension_ui(shell, app);
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => anyhow::bail!("resource discovery cancelled by shutdown"),
            result = &mut future => return result,
            _ = crate::extensions::remote_ui::notified(&wake) => { apply_extension_background(shell, &mut app.executable_extensions); request_extension_ui(shell, app); shell.render(); }
            _ = tick.tick() => { apply_extension_background(shell, &mut app.executable_extensions); request_extension_ui(shell, app); shell.render(); }
            event = input.next() => {
                let event = match event { Some(event) => event?, None => anyhow::bail!("input closed during resource discovery") };
                if app.executable_extensions.route_remote_ui_event(shell, &event) { continue; }
                observe_extension_terminal_event(&mut app.executable_extensions, &event);
                match event {
                    Event::Key(key) if is_ctrl_c(&key) => {
                        crate::tui::terminal::request_coordinated_shutdown(RAW_CTRL_C_SIGNAL)?;
                        anyhow::bail!("resource discovery cancelled");
                    }
                    Event::Key(key) if keymap::is_close_key(&key) => {
                        shell.request_close(); anyhow::bail!("resource discovery closed");
                    }
                    Event::Resize(columns, rows) => shell.set_size(columns, rows),
                    event => {
                        if !shell.intercept_transcript_input(&event) && !paste_clipboard_text(shell, &event).await {
                            let _ = handle_cancellable_wait_input(shell, event);
                        }
                    }
                }
                shell.render();
            }
        }
    }
}

pub(super) async fn refresh_resource_paths<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<()>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    if shell.close_requested() || !app.resource_paths_pending() {
        return Ok(());
    }

    let starts = match app.executable_extensions.resource_session_starts() {
        Ok(work) => {
            pump(shell, input, app, async move {
                Ok(work.await)
            })
            .await?
        }
        Err(error) => Err(error),
    };
    let candidate = match starts {
        Ok(lease) => match app.prepare_resource_paths(shell.theme().background(), lease) {
            Ok(work) => {
                pump(shell, input, app, async move {
                    Ok(work.await)
                })
                .await?
            }
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    let (theme, diagnostics) = match candidate.and_then(|(loaded, lease)| {
        app.apply_extension_resource_paths(loaded, lease)
    }) {
        Ok(published) => published,
        Err(error) => {
            let baseline = app.prepare_resource_withdrawal(shell.theme().background(), error)?;
            let (baseline, lease) = pump(shell, input, app, baseline).await?;
            app.apply_extension_resource_paths(baseline, lease)?
        }
    };
    shell.set_theme(theme);
    shell.set_runtime_config(app.config.clone());
    for diagnostic in diagnostics {
        shell.notice(diagnostic);
    }
    update_status(shell, app);
    Ok(())
}

pub(super) async fn reload_resource_processes<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<crate::extensions::ExtensionReloadReport>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let work = app.executable_extensions.prepare_resource_process_reload();
    let results = pump(shell, input, app, async move {
        Ok(work.await)
    })
    .await?;
    let report = app
        .executable_extensions
        .finish_resource_process_reload(results)
        .await;
    refresh_resource_paths(app, shell, input).await?;
    Ok(report)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::extensions::resource_paths::consumer_tests::{fixture, hold_session_start};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn already_chosen_close_does_not_dispatch_resource_work() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (mut app, process, _) = fixture(&root).await;
        let mut shell = InteractiveShell::test_shell();
        shell.request_close();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        refresh_resource_paths(&mut app, &mut shell, &mut input)
            .await
            .unwrap();
        assert!(!root.join("consumer-calls.jsonl").exists());
        assert!(app.resource_paths_pending());
        process.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_interactive_phase_pumps_resize_before_deferred_start_then_publishes_native_theme()
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (mut app, process, _) = fixture(&root).await;
        let (release, wait) = tokio::sync::oneshot::channel();
        hold_session_start(&mut app, wait);
        let mut shell = InteractiveShell::test_shell();
        let observed = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = observed.clone();
        let mut first = true;
        let mut input = futures_util::stream::poll_fn(move |_| {
            if first {
                first = false;
                signal.notify_one();
                std::task::Poll::Ready(Some(Ok(Event::Resize(100, 30))))
            } else {
                std::task::Poll::Pending
            }
        });
        let phase = refresh_resource_paths(&mut app, &mut shell, &mut input);
        let driver = async {
            observed.notified().await;
            let calls =
                std::fs::read_to_string(root.join("consumer-calls.jsonl")).unwrap_or_default();
            assert!(!calls.contains("resources_discover"));
            release.send(()).unwrap();
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let (result, ()) = tokio::join!(phase, driver);
            result.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(shell.theme().glyph("prompt"), ":");
        assert!(app.prompts.contains("consumer-proof"));
        assert!(app.agent.system_prompt().contains("CATALOG-SENTINEL"));
        let calls = std::fs::read_to_string(root.join("consumer-calls.jsonl")).unwrap();
        assert!(calls.find("session_start").unwrap() < calls.find("resources_discover").unwrap());
        process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resource_start_and_reload_apply_durable_reverse_requests_before_withdrawal() {
        use crate::extensions::resource_paths::consumer_tests::configured_app;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("reverse-requests"), "interactive").unwrap();
        let mut app = configured_app(&root, true, true);
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        tokio::time::timeout(Duration::from_secs(10), refresh_resource_paths(&mut app, &mut shell, &mut input))
            .await.unwrap().unwrap();
        assert!(app.prompts.contains("consumer-proof"));
        assert_eq!(shell.theme().glyph("prompt"), ":");
        let old_prompts = app.prompts.clone();
        // A replacement generation must answer the same real reverse requests
        // before its empty authoritative reply withdraws all old resources.
        std::fs::write(root.join("reply.json"), "{}").unwrap();
        let report = tokio::time::timeout(Duration::from_secs(10), reload_resource_processes(&mut app, &mut shell, &mut input))
            .await.unwrap().unwrap();
        assert!(report.processes.iter().all(|(_, result)| result.is_ok()));
        assert!(!app.resource_paths_pending());
        assert!(!old_prompts.contains("consumer-proof"));
        assert!(!app.prompts.contains("consumer-proof"));
        assert!(shell.theme().source_path().is_none());
        let replies = std::fs::read_to_string(root.join("reverse-replies.jsonl")).unwrap();
        let replies = replies.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
        assert_eq!(replies.len(), 4);
        for (reply, hook) in replies.iter().zip(["session_start", "resources_discover", "session_start", "resources_discover"]) {
            assert_eq!(reply["hook"], hook);
            let id = octet_agent::EntryId(reply["reply"]["result"]["entry_id"].as_str().unwrap().into());
            let entry = app.agent.session().extension_entry(&id, "consumer-peer").unwrap();
            assert_eq!(entry.entry_type, "resource-phase-proof");
            assert_eq!(entry.data["hook"], hook);
        }
        app.executable_extensions.shutdown().await;
    }
}
