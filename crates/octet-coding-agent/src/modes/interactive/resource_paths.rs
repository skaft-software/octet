//! The existing input owner pumps reverse UI while the resource phase waits.
use super::*;

/// The user pressed the close key while the resource phase was waiting. That is
/// a normal exit, not a failure: the shell has already been asked to close, so
/// the public entry points report it as success instead of an error.
#[derive(Debug)]
struct ResourceDiscoveryClosed;
impl std::fmt::Display for ResourceDiscoveryClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("resource discovery closed")
    }
}
impl std::error::Error for ResourceDiscoveryClosed {}

pub(super) enum FrontendWake {
    Input(Option<std::io::Result<Event>>),
    RemoteUi,
    Tick,
}

pub(super) async fn next_frontend_wake<S>(
    input: &mut S,
    wake: &Option<Arc<tokio::sync::Notify>>,
    tick: &mut Interval,
) -> FrontendWake
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    tokio::select! {
        event = input.next() => FrontendWake::Input(event),
        _ = crate::extensions::remote_ui::notified(wake) => FrontendWake::RemoteUi,
        _ = tick.tick() => FrontendWake::Tick,
    }
}

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
    let mut completed = None;
    loop {
        // Resource readiness includes a released chrome handoff. Keep the same
        // input owner alive for its checkpoint/frame, or its one-second fallback,
        // instead of returning a ready session with its editor still hidden.
        if !app.executable_extensions.chrome_handoff_settling() {
            if let Some(result) = completed.take() {
                return result;
            }
        }
        tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => anyhow::bail!("resource discovery cancelled by shutdown"),
            result = &mut future, if completed.is_none() => {
                match result {
                    Ok(value) => completed = Some(Ok(value)),
                    Err(error) => return Err(error),
                }
            },
            event = next_frontend_wake(input, &wake, &mut tick) => {
                let event = match event {
                    FrontendWake::Input(Some(event)) => event?,
                    FrontendWake::Input(None) => anyhow::bail!("input closed during resource discovery"),
                    FrontendWake::RemoteUi | FrontendWake::Tick => {
                        apply_extension_background(shell, &mut app.executable_extensions);
                        request_extension_ui(shell, app);
                        shell.render();
                        continue;
                    }
                };
                if app.executable_extensions.route_remote_ui_event(shell, &event, false) { continue; }
                observe_extension_terminal_event(&mut app.executable_extensions, &event);
                match event {
                    Event::Key(key) if is_ctrl_c(&key) => {
                        crate::tui::terminal::request_coordinated_shutdown(RAW_CTRL_C_SIGNAL)?;
                        anyhow::bail!("resource discovery cancelled");
                    }
                    Event::Key(key) if keymap::is_close_key(&key) => {
                        shell.request_close();
                        return Err(anyhow::Error::new(ResourceDiscoveryClosed));
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

/// The resource phase at startup and after configuration changes: problems are
/// shown as one `reload failed: …` note; success is silent.
pub(super) async fn refresh_resource_paths<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<()>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let problems = refresh_resource_paths_reporting(app, shell, input).await?;
    shell.report_reload_outcome(&problems, false);
    Ok(())
}

/// The same phase, but the caller owns how problems are presented (an explicit
/// reload also announces success).
pub(super) async fn refresh_resource_paths_reporting<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<Vec<String>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    match refresh_resource_paths_until_closed(app, shell, input).await {
        Err(error) if error.is::<ResourceDiscoveryClosed>() => Ok(Vec::new()),
        result => result,
    }
}

async fn refresh_resource_paths_until_closed<S>(
    app: &mut App,
    shell: &mut InteractiveShell,
    input: &mut S,
) -> anyhow::Result<Vec<String>>
where
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let resources_pending = app.resource_paths_pending();
    if shell.close_requested()
        || (!resources_pending && !app.executable_extensions.has_pending_session_starts())
    {
        return Ok(Vec::new());
    }

    // Scheduling starts spawns work on other runtime workers. Publish the real
    // model facts before releasing them: pump()'s first UI refresh is too late
    // for a session_start callback that immediately reads or spreads Pi's ctx.
    app.executable_extensions.refresh_pi_model_catalog(
        &app.catalog,
        app.model_scope.as_deref(),
        &app.model,
    );
    let starts = match app.executable_extensions.resource_session_starts() {
        Ok(work) => pump(shell, input, app, async move { Ok(work.await) }).await?,
        Err(error) => Err(error),
    };
    // Deferred startup also belongs to ordinary factories. Settle it under
    // the existing UI/session pump, without inventing a resource-discovery pass.
    if !resources_pending {
        starts?;
        return Ok(Vec::new());
    }
    let candidate = match starts {
        Ok(lease) => match app.prepare_resource_paths(shell.theme().background(), lease) {
            Ok(work) => pump(shell, input, app, async move { Ok(work.await) }).await?,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    let (theme, diagnostics) = match candidate
        .and_then(|(loaded, lease)| app.apply_extension_resource_paths(loaded, lease))
    {
        Ok(published) => published,
        Err(error) => {
            let baseline = app.prepare_resource_withdrawal(shell.theme().background(), error)?;
            let (baseline, lease) = pump(shell, input, app, baseline).await?;
            match app.publish_withdrawal(baseline, lease)? {
                Some(published) => published,
                // The binding moved under the withdrawal (extension session replacement):
                // the next idle pass publishes for the settled binding.
                None => return Ok(Vec::new()),
            }
        }
    };
    // Publish once, then repaint once. A round that resolves the same theme an
    // earlier round already installed keeps the live frame instead of forcing
    // another complete replay of the transcript while startup is still in
    // flight; `theme.published` marks the one round that changed it.
    let repainted = shell.publish_theme(theme);
    shell.set_runtime_config(app.config.clone());
    shell.set_session_keybindings(&app.user_keybindings);
    app.executable_extensions
        .refresh_frontend_keybindings(shell);
    // The published palette reaches extensions before any chrome held during
    // this phase is handed over, so none of it paints with the replaced one.
    app.executable_extensions
        .set_chrome_handoff_pending(app.resource_paths_pending(), shell);
    update_status(shell, app);
    if repainted {
        shell.render();
    }
    pump(shell, input, app, std::future::ready(Ok(()))).await?;
    Ok(diagnostics)
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
    let results = match pump(shell, input, app, async move { Ok(work.await) }).await {
        Ok(results) => results,
        Err(error) if error.is::<ResourceDiscoveryClosed>() => {
            return Ok(crate::extensions::ExtensionReloadReport::default());
        }
        Err(error) => return Err(error),
    };
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[tokio::test]
    async fn continuously_ready_input_and_remote_ui_both_make_progress() {
        let input_polls = std::cell::Cell::new(0);
        let mut input = futures_util::stream::repeat_with(|| {
            input_polls.set(input_polls.get() + 1);
            Ok(Event::Resize(80, 24))
        });
        let wake = Arc::new(tokio::sync::Notify::new());
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        for _ in 0..32 {
            wake.notify_one();
            tokio::select! {
                biased;
                _ = wake.notified() => {}
                _ = tick.tick() => panic!("ready UI wake must win the old biased ordering"),
                _ = input.next() => panic!("old ordering unexpectedly polled input"),
            }
        }
        assert_eq!(input_polls.get(), 0);
        let mut input_seen = false;
        let mut ui_seen = false;
        for _ in 0..64 {
            wake.notify_one();
            match next_frontend_wake(&mut input, &Some(wake.clone()), &mut tick).await {
                FrontendWake::Input(Some(Ok(Event::Resize(80, 24)))) => input_seen = true,
                FrontendWake::RemoteUi => ui_seen = true,
                FrontendWake::Tick => {}
                _ => panic!("unexpected terminal input"),
            }
            if input_seen && ui_seen {
                return;
            }
        }
        panic!("frontend starvation: input={input_seen}, remote_ui={ui_seen}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resource_pump_routes_resize_and_draft_during_continuous_remote_ui_wakes() {
        use crate::extensions::{
            ExecutableExtensions, ExtensionProviderRuntime, ExtensionStartupTiming,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::Poll;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut app =
            crate::extensions::resource_paths::consumer_tests::configured_app(&root, true, false);
        let mut shell = InteractiveShell::test_shell();
        shell.finish_startup();
        let config = super::super::tests::terminal_theme_test_config(app.config.workspace.clone());
        let mut host = octet_agent::ExtensionHost::new();
        app.executable_extensions = tokio::task::block_in_place(|| {
            ExecutableExtensions::discover_and_start_for_selection(
                &config,
                app.agent.session(),
                Some((&app.model, &app.reasoning)),
                &app.sessions,
                &mut host,
                None,
                ExtensionProviderRuntime::default(),
                crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
                Some(&shell),
                "startup",
                ExtensionStartupTiming::AfterFirstFrame,
            )
        });
        let wake = app.executable_extensions.remote_ui_wake().unwrap();
        let consumed = Arc::new(AtomicUsize::new(0));
        let observed = consumed.clone();
        let mut events = VecDeque::from([
            Event::Resize(80, 24),
            Event::Paste("FAIR-RESOURCE-DRAFT".into()),
        ]);
        let mut input = futures_util::stream::poll_fn(move |_| match events.pop_front() {
            Some(event) => {
                observed.fetch_add(1, Ordering::SeqCst);
                Poll::Ready(Some(Ok(event)))
            }
            None => Poll::Pending,
        });
        let mut polls = 0;
        let work = futures_util::future::poll_fn(|_| {
            if consumed.load(Ordering::SeqCst) == 2 {
                return Poll::Ready(Ok(()));
            }
            polls += 1;
            if polls > 64 {
                return Poll::Ready(Err(anyhow::anyhow!("remote UI starved terminal input")));
            }
            wake.notify_one();
            Poll::Pending
        });
        tokio::time::timeout(
            Duration::from_secs(3),
            pump(&mut shell, &mut input, &mut app, work),
        )
        .await
        .expect("frontend pump must remain responsive")
        .unwrap();
        assert_eq!(consumed.load(Ordering::SeqCst), 2);
        assert_eq!(
            shell.extension_editor_snapshot().text,
            "FAIR-RESOURCE-DRAFT"
        );
        let frame = shell.dump_rendered_frame().await.unwrap();
        assert!(frame.len() <= 24);
        assert!(sexy_tui_rs::strip_terminal_sequences(&frame.join("\n"))
            .contains("FAIR-RESOURCE-DRAFT"));

        let mut input =
            futures_util::stream::poll_fn(|_| -> Poll<Option<std::io::Result<Event>>> {
                panic!("completed work must outrank terminal input")
            });
        pump(&mut shell, &mut input, &mut app, async { Ok(()) })
            .await
            .unwrap();
        app.executable_extensions.shutdown().await;
    }

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
    async fn closing_while_resource_discovery_waits_is_a_clean_exit_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (mut app, process, _) = fixture(&root).await;
        // The extension's session start never completes, so the phase is
        // still waiting when the user presses Ctrl+D.
        let (_hold, wait) = tokio::sync::oneshot::channel();
        hold_session_start(&mut app, wait);
        let mut shell = InteractiveShell::test_shell();
        let input = futures_util::stream::iter([Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        )))])
        .chain(futures_util::stream::pending());
        tokio::pin!(input);
        tokio::time::timeout(
            Duration::from_secs(10),
            refresh_resource_paths(&mut app, &mut shell, &mut input),
        )
        .await
        .expect("the phase must end when the user closes octet")
        .expect("closing during resource discovery is not an error");
        assert!(shell.close_requested());
        let reload = reload_resource_processes(&mut app, &mut shell, &mut input).await;
        assert!(reload.is_ok(), "{reload:?}");
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
        tokio::time::timeout(
            Duration::from_secs(10),
            refresh_resource_paths(&mut app, &mut shell, &mut input),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(app.prompts.contains("consumer-proof"));
        assert_eq!(shell.theme().glyph("prompt"), ":");
        let old_prompts = app.prompts.clone();
        // A replacement generation must answer the same real reverse requests
        // before its empty authoritative reply withdraws all old resources.
        std::fs::write(root.join("reply.json"), "{}").unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(10),
            reload_resource_processes(&mut app, &mut shell, &mut input),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(report.processes.iter().all(|(_, result)| result.is_ok()));
        assert!(!app.resource_paths_pending());
        assert!(!old_prompts.contains("consumer-proof"));
        assert!(!app.prompts.contains("consumer-proof"));
        assert!(shell.theme().source_path().is_none());
        let replies = std::fs::read_to_string(root.join("reverse-replies.jsonl")).unwrap();
        let replies = replies
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(replies.len(), 4);
        for (reply, hook) in replies.iter().zip([
            "session_start",
            "resources_discover",
            "session_start",
            "resources_discover",
        ]) {
            assert_eq!(reply["hook"], hook);
            let id = octet_agent::EntryId(
                reply["reply"]["result"]["entry_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            );
            let entry = app
                .agent
                .session()
                .extension_entry(&id, "consumer-peer")
                .unwrap();
            assert_eq!(entry.entry_type, "resource-phase-proof");
            assert_eq!(entry.data["hook"], hook);
        }
        app.executable_extensions.shutdown().await;
    }

    /// A palette-contributing peer that also installs a custom editor from
    /// `session_start`, the way a Pi extension calls `setEditorComponent`.
    /// Every frame names the palette it was rendered with, so a frame painted
    /// before the resource-selected palette reached the extension is visible
    /// as `palette=native`. Optional marker files select failure shapes.
    const HANDOFF_PEER: &str = r#"
import json, os, sys, time
root = sys.argv[1]
flag = lambda name: os.path.exists(os.path.join(root, name))
state = dict(theme=None, surface=None, text='', revision=0, checkpoint=0, parent=None, owner=None)
def send(value):
    print(json.dumps(dict(jsonrpc='2.0', **value)), flush=True)
def marker():
    path = (state['theme'] or {}).get('path')
    return os.path.basename(path) if path else 'native'
def frame():
    s = state['surface']
    if s is None:
        return
    send(dict(method='ui/frame', params=dict(resource_owner=state['owner'], surface_id='handoff-editor',
        revision=state['revision'], columns=s['columns'], rows=s['rows'],
        lines=['CUSTOM-EDITOR palette=' + marker() + ' text=' + state['text']])))
    state['revision'] += 1
for line in sys.stdin:
    m = json.loads(line)
    method, p = m.get('method'), m.get('params', {})
    with open(os.path.join(root, 'handoff-wire.log'), 'a') as log:
        log.write(str(method or m.get('id')) + (' ' + json.dumps(p) if method == 'ui/closed' else '') + ('' if 'error' not in m else ' error=' + json.dumps(m['error'])) + '\n')
    if method == 'initialize':
        features = p['protocol']['required_features'] + ['resource_paths_v1', 'remote_ui', 'composer']
        send(dict(id=m['id'], result=dict(api_version='0.4', tools=[], commands=[dict(name='release-start', description='unused')],
            protocol=dict(version='0.4', features=features, limits=dict(max_concurrent_requests=4)))))
    elif method == 'hook/run':
        host = p.get('context', {}).get('host') or {}
        if host.get('theme') is not None:
            state['theme'] = host['theme']
        if p['hook'] == 'session_start':
            state.update(parent=m['id'], owner=p['context']['resource_owner'])
            if flag('setup-dialog'):
                send(dict(id='open-dialog', method='ui/open', params=dict(parent_request_id=m['id'],
                    resource_owner=state['owner'], surface_id='setup-dialog', title='Setup', placement='fullscreen')))
            send(dict(id='open-editor', method='ui/open', params=dict(parent_request_id=m['id'],
                resource_owner=state['owner'], surface_id='handoff-editor', title='Handoff editor', placement='editor')))
        elif p['hook'] == 'resources_discover':
            if flag('hold-discover'):
                while not flag('release-discover'):
                    time.sleep(0.001)
            if flag('fail-discover'):
                send(dict(id=m['id'], error=dict(code=-32000, message='discovery failed')))
            else:
                send(dict(id=m['id'], result=dict(resource_paths=json.load(open(os.path.join(root, 'reply.json'))))))
        else:
            send(dict(id=m['id'], result=dict(disposition=dict(action='continue'))))
    elif m.get('id') == 'open-dialog':
        if 'result' in m:
            send(dict(method='ui/frame', params=dict(resource_owner=state['owner'], surface_id='setup-dialog',
                revision=0, columns=m['result']['columns'], rows=m['result']['rows'], lines=['SETUP-DIALOG'])))
    elif m.get('id') == 'open-editor':
        if 'error' in m:
            send(dict(id=state['parent'], error=m['error']))
            continue
        state['surface'] = m['result']
        frame()
        if flag('seed-text'):
            # A programmatic setText on the component, e.g. a restored draft.
            state['text'] = 'seeded by extension'
            state['checkpoint'] += 1
            send(dict(id='checkpoint-' + str(state['checkpoint']), method='composer/set', params=dict(
                parent_request_id=state['parent'], resource_owner=state['owner'], text=state['text'],
                editor_checkpoint=dict(surface_id='handoff-editor', mount_id=state['surface']['editor_mount_id'],
                    input_revision=0, checkpoint_revision=state['checkpoint']))))
        send(dict(id=state['parent'], result=dict(disposition=dict(action='continue'))))
    elif method == 'context/updated':
        if (p.get('host') or {}).get('theme') is not None:
            state['theme'] = p['host']['theme']
            if not flag('no-rerender'):
                frame()
    elif method == 'ui/editor-text':
        state['text'] = p['text']
        if flag('no-rerender'):
            continue
        state['checkpoint'] += 1
        send(dict(id='checkpoint-' + str(state['checkpoint']), method='composer/set', params=dict(
            parent_request_id=state['parent'], resource_owner=state['owner'], text=state['text'],
            editor_checkpoint=dict(surface_id='handoff-editor', mount_id=state['surface']['editor_mount_id'],
                input_revision=0, checkpoint_revision=state['checkpoint']))))
    elif isinstance(m.get('id'), str) and m['id'].startswith('checkpoint-'):
        # Like the Pi adapter: a frame never claims text its checkpoint has
        # not had acknowledged.
        frame()
    elif method == 'ui/resize':
        state['surface'].update(columns=p['columns'], rows=p['rows'])
        frame()
    elif method == 'shutdown':
        send(dict(id=m['id'], result={}))
        break
"#;

    struct Handoff {
        _temp: tempfile::TempDir,
        root: std::path::PathBuf,
        app: App,
        shell: InteractiveShell,
        process: octet_agent::extension_process::ExtensionProcess,
        published: std::path::PathBuf,
    }

    async fn handoff_fixture(markers: &[&str]) -> Handoff {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for marker in markers {
            std::fs::write(root.join(marker), "").unwrap();
        }
        let (app, process) = crate::extensions::resource_paths::consumer_tests::remote_ui_fixture(
            &root,
            HANDOFF_PEER,
        )
        .await;
        let published = root.join("pi-themes/published-pi.json");
        std::fs::create_dir_all(published.parent().unwrap()).unwrap();
        std::fs::write(
            &published,
            crate::extensions::resource_paths::pi_theme::fixture("Published Pi", "#2468ac")
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("reply.json"),
            serde_json::json!({"theme_paths": [published], "default_theme": published}).to_string(),
        )
        .unwrap();
        let mut shell = InteractiveShell::test_shell();
        shell.finish_startup();
        Handoff {
            _temp: temp,
            root,
            app,
            shell,
            process,
            published,
        }
    }

    async fn frame_text(shell: &mut InteractiveShell) -> String {
        shell.render();
        sexy_tui_rs::strip_terminal_sequences(
            &shell.dump_rendered_frame().await.unwrap().join("\n"),
        )
    }

    /// Run `session_start` exactly as the idle loop does (the frontend's own
    /// extension-UI consumer applies `ui/open` and frames), until the start has
    /// settled and its frame has been drained. Resource publication has not run.
    async fn settle_session_start(fixture: &mut Handoff) {
        assert!(fixture.app.resource_paths_pending());
        // Production publishes the foreground session before scheduling its
        // starts. Without this boundary a fast peer can open a surface before
        // the first UI pump, whose initial host-state update then retires it.
        fixture.app.executable_extensions.refresh_host_state(
            fixture.app.agent.session(),
            &fixture.app.model,
            &fixture.app.reasoning,
            &fixture.app.sessions,
        );
        let starts = tokio::spawn(
            fixture
                .app
                .executable_extensions
                .resource_session_starts()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while !starts.is_finished() {
                request_extension_ui(&mut fixture.shell, &mut fixture.app);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "session_start must settle while the frontend consumes its UI; peer saw: {}",
                std::fs::read_to_string(fixture.root.join("handoff-wire.log")).unwrap_or_default()
            )
        });
        starts.await.unwrap().unwrap();
        // The frame preceded the hook reply on the same pipe.
        request_extension_ui(&mut fixture.shell, &mut fixture.app);
    }

    /// Drain like the idle loop until the custom editor owns the composer
    /// slot. No intermediate frame may contain `forbidden`.
    async fn until_editor_visible(fixture: &mut Handoff, forbidden: Option<&str>) -> String {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                request_extension_ui(&mut fixture.shell, &mut fixture.app);
                let frame = frame_text(&mut fixture.shell).await;
                if let Some(forbidden) = forbidden {
                    assert!(
                        !frame.contains(forbidden),
                        "a custom editor frame painted with the replaced palette:\n{frame}"
                    );
                }
                if fixture.shell.composer_slot_mount().is_some() {
                    return frame;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the custom editor must take over once its palette is published: handoff_pending={} resources_pending={} running={} peer saw: {}",
                fixture.app.executable_extensions.chrome_handoff_pending(),
                fixture.app.resource_paths_pending(),
                fixture.process.is_running(),
                std::fs::read_to_string(fixture.root.join("handoff-wire.log")).unwrap_or_default(),
            )
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_custom_editor_first_paints_with_the_resource_selected_palette() {
        let mut fixture = handoff_fixture(&[]).await;
        settle_session_start(&mut fixture).await;
        // The editor exists and has painted, but the resource generation that
        // selects the palette has not been published: the native composer
        // keeps the slot and stays editable.
        assert!(fixture.app.executable_extensions.chrome_handoff_pending());
        assert!(fixture.shell.composer_slot_mount().is_none());
        let pending = frame_text(&mut fixture.shell).await;
        assert!(!pending.contains("CUSTOM-EDITOR"), "{pending}");
        let paste = Event::Paste("handoff draft".into());
        assert!(!fixture.app.executable_extensions.route_remote_ui_event(
            &mut fixture.shell,
            &paste,
            false
        ));
        let _ = handle_cancellable_wait_input(&mut fixture.shell, paste);
        assert_eq!(
            fixture.shell.extension_editor_snapshot().text,
            "handoff draft"
        );
        assert!(frame_text(&mut fixture.shell)
            .await
            .contains("handoff draft"));

        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        tokio::time::timeout(
            Duration::from_secs(10),
            refresh_resource_paths(&mut fixture.app, &mut fixture.shell, &mut input),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            fixture.shell.theme().source_path(),
            Some(fixture.published.as_path())
        );
        assert!(!fixture.app.executable_extensions.chrome_handoff_pending());
        // Publication itself never exposes the frame rendered before it.
        let published = frame_text(&mut fixture.shell).await;
        assert!(!published.contains("palette=native"), "{published}");
        // Only the frame answering the delivered draft carries that text, so
        // this is the checkpoint-fenced frame, not a deadline reveal.
        let visible = until_editor_visible(&mut fixture, Some("palette=native")).await;
        assert!(
            visible.contains("CUSTOM-EDITOR palette=published-pi.json text=handoff draft"),
            "{visible}"
        );
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn extension_free_startup_never_holds_chrome() {
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        let mut shell = InteractiveShell::test_shell();
        assert!(!app.resource_paths_pending());
        request_extension_ui(&mut shell, &mut app);
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        refresh_resource_paths(&mut app, &mut shell, &mut input)
            .await
            .unwrap();
        request_extension_ui(&mut shell, &mut app);
        assert!(!app.executable_extensions.chrome_handoff_pending());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn held_custom_editor_keeps_native_input_and_close_while_resources_are_pending() {
        let mut fixture = handoff_fixture(&["hold-discover"]).await;
        settle_session_start(&mut fixture).await;
        // Discovery never answers; the user types into the native composer
        // and then closes. The held editor takes neither event.
        let mut input = futures_util::stream::iter([
            Ok(Event::Paste("typed while pending".into())),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
            ))),
        ])
        .chain(futures_util::stream::pending());
        tokio::time::timeout(
            Duration::from_secs(10),
            refresh_resource_paths(&mut fixture.app, &mut fixture.shell, &mut input),
        )
        .await
        .expect("closing must end a phase that holds custom chrome")
        .expect("closing during the handoff is not an error");
        assert!(fixture.shell.close_requested());
        assert!(fixture.app.resource_paths_pending());
        assert!(fixture.shell.composer_slot_mount().is_none());
        assert_eq!(
            fixture.shell.extension_editor_snapshot().text,
            "typed while pending"
        );
        let frame = frame_text(&mut fixture.shell).await;
        assert!(!frame.contains("CUSTOM-EDITOR"), "{frame}");
        std::fs::write(fixture.root.join("release-discover"), "").unwrap();
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn held_editor_programmatic_draft_reaches_the_native_composer() {
        let mut fixture = handoff_fixture(&["seed-text"]).await;
        settle_session_start(&mut fixture).await;
        request_extension_ui(&mut fixture.shell, &mut fixture.app);
        // The component's own draft is not discarded because it is held: the
        // native composer, which owns input meanwhile, adopts it.
        assert!(fixture.shell.composer_slot_mount().is_none());
        assert_eq!(
            fixture.shell.extension_editor_snapshot().text,
            "seeded by extension"
        );
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        refresh_resource_paths(&mut fixture.app, &mut fixture.shell, &mut input)
            .await
            .unwrap();
        let visible = until_editor_visible(&mut fixture, Some("palette=native")).await;
        assert!(
            visible.contains("CUSTOM-EDITOR palette=published-pi.json text=seeded by extension"),
            "{visible}"
        );
        assert_eq!(
            fixture.shell.extension_editor_snapshot().text,
            "seeded by extension"
        );
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn setup_dialog_is_not_held_while_its_palette_owner_is_pending() {
        let mut fixture = handoff_fixture(&["setup-dialog"]).await;
        settle_session_start(&mut fixture).await;
        request_extension_ui(&mut fixture.shell, &mut fixture.app);
        assert!(fixture.app.resource_paths_pending());
        // A dialog is an interaction, not chrome: it is shown at once, even
        // though the editor opened by the same start is still held.
        assert!(fixture.shell.has_remote_fullscreen_mount());
        assert!(fixture.shell.composer_slot_mount().is_none());
        let frame = frame_text(&mut fixture.shell).await;
        assert!(frame.contains("SETUP-DIALOG"), "{frame}");
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_resource_discovery_still_hands_over_held_chrome() {
        let mut fixture = handoff_fixture(&["fail-discover"]).await;
        settle_session_start(&mut fixture).await;
        assert!(fixture.shell.composer_slot_mount().is_none());
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        tokio::time::timeout(
            Duration::from_secs(10),
            refresh_resource_paths(&mut fixture.app, &mut fixture.shell, &mut input),
        )
        .await
        .unwrap()
        .unwrap();
        // The withdrawn overlay is still a publication: readiness is not
        // stranded behind a generation that will never contribute a palette.
        assert!(!fixture.app.resource_paths_pending());
        let visible = until_editor_visible(&mut fixture, None).await;
        assert!(visible.contains("CUSTOM-EDITOR"), "{visible}");
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn published_editor_handoff_keeps_native_input_and_close_responsive() {
        let mut fixture = handoff_fixture(&["no-rerender"]).await;
        settle_session_start(&mut fixture).await;
        let wire = fixture.root.join("handoff-wire.log");
        // Wait for the actual post-publication draft fence, not a timing guess.
        let paste = futures_util::stream::once(async move {
            while !std::fs::read_to_string(&wire)
                .unwrap_or_default()
                .contains("ui/editor-text")
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok(Event::Paste("typed during draft handoff".into()))
        });
        let mut input = paste
            .chain(futures_util::stream::iter([Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
            )))]))
            .chain(futures_util::stream::pending())
            .boxed();
        tokio::time::timeout(
            Duration::from_secs(5),
            refresh_resource_paths(&mut fixture.app, &mut fixture.shell, &mut input),
        )
        .await
        .expect("published chrome handoff must keep pumping native input")
        .unwrap();
        assert!(!fixture.app.resource_paths_pending());
        assert!(fixture.shell.close_requested());
        assert!(fixture.shell.composer_slot_mount().is_none());
        assert_eq!(
            fixture.shell.extension_editor_snapshot().text,
            "typed during draft handoff"
        );
        assert!(!frame_text(&mut fixture.shell)
            .await
            .contains("CUSTOM-EDITOR"));
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn held_chrome_that_never_rerenders_is_revealed_at_the_bounded_deadline() {
        let mut fixture = handoff_fixture(&["no-rerender"]).await;
        settle_session_start(&mut fixture).await;
        assert!(fixture.shell.composer_slot_mount().is_none());
        let started = std::time::Instant::now();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        tokio::time::timeout(
            Duration::from_secs(5),
            refresh_resource_paths(&mut fixture.app, &mut fixture.shell, &mut input),
        )
        .await
        .expect("resource readiness must not wait indefinitely for a silent editor")
        .unwrap();
        // This component answers neither the palette nor the draft. Its only
        // frame predates the palette; resource readiness must pump all the way
        // to the fallback rather than claim the hidden editor is already ready.
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(fixture.shell.composer_slot_mount().is_some());
        let visible = frame_text(&mut fixture.shell).await;
        assert!(
            visible.contains("CUSTOM-EDITOR palette=native"),
            "{visible}"
        );
        fixture.process.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_interactive_publication_applies_and_withdraws_session_keybindings_and_context() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (mut app, process, _) = fixture(&root).await;
        std::fs::write(
            root.join("keybindings.json"),
            "{\"app.exit\": [\"ctrl+q\"]}",
        )
        .unwrap();
        std::fs::write(root.join("AGENTS.md"), "SESSION-CONTEXT-SENTINEL\n").unwrap();
        std::fs::write(
            root.join("reply.json"),
            serde_json::to_vec(&serde_json::json!({
                "skill_paths": [root.join("skills")],
                "prompt_paths": [root.join("prompts")],
                "theme_paths": [root.join("themes")],
                "keybindings_paths": [root.join("keybindings.json")],
                "context_paths": [root.join("AGENTS.md")],
            }))
            .unwrap(),
        )
        .unwrap();
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        refresh_resource_paths(&mut app, &mut shell, &mut input)
            .await
            .unwrap();
        assert_eq!(
            app.user_keybindings.get("app.exit"),
            Some(&vec!["ctrl+q".to_owned()])
        );
        assert_eq!(
            shell.extension_keybindings().get("app.exit"),
            Some(&vec!["ctrl+q".to_owned()]),
            "the live input owner must dispatch the contributed binding"
        );
        assert!(app
            .agent
            .system_prompt()
            .contains("SESSION-CONTEXT-SENTINEL"));
        // Withdrawal restores the user's own map and the baseline prompt.
        std::fs::write(root.join("reply.json"), "{}").unwrap();
        app.mark_resource_paths_reload();
        refresh_resource_paths(&mut app, &mut shell, &mut input)
            .await
            .unwrap();
        assert!(app.user_keybindings.is_empty());
        assert_ne!(
            shell.extension_keybindings().get("app.exit"),
            Some(&vec!["ctrl+q".to_owned()]),
            "a withdrawn session override must not survive"
        );
        assert!(!app
            .agent
            .system_prompt()
            .contains("SESSION-CONTEXT-SENTINEL"));
        process.shutdown().await;
    }
}
