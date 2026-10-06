//! Real Rust shell + reviewed adapter acceptance for Pi dialogs and editor API.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_ui_app};
use super::support::test_reload;
use super::*;

// Execute the production resource/session-start phase under its sole native
// frontend pump, rather than starting a command while lifecycle is unbound.
async fn live_ui_app(factory: &str) -> (tempfile::TempDir, App, InteractiveShell) {
    let (directory, mut app, mut shell) = pi_ui_app(factory);
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .expect("native resource/lifecycle startup timed out")
    .unwrap();
    shell.finish_startup();
    (directory, app, shell)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_desktop_notification_requires_foreground_terminal_admission() {
    let (directory, mut app, mut shell) = live_ui_app(
        r#"
import { writeFileSync } from 'node:fs';
export default pi => pi.registerCommand('desktop-notify', {handler: () => {
  process.stdout.write('\x1b]777;notify;π;Native notification\x07');
  writeFileSync(TRACE, 'host-acknowledged');
}});
"#,
    )
    .await;
    shell.cede_terminal_input();
    let error = command(&mut app, &mut shell, "desktop-notify")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("terminal is ceded"), "{error}");
    assert!(!directory.path().join("trace.jsonl").exists());
    shell.release_terminal_input();
    app.executable_extensions.shutdown().await;
}

#[tokio::test]
async fn native_pi_test_shell_preserves_real_styles_and_complete_labels() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(37, 12);
    let run = shell.begin_run("offline-test");
    shell.render();
    let rows = shell.dump_rendered_frame().await.unwrap();
    let raw = rows.join("\n");
    assert!(raw.contains("\x1b[38;2;"), "truecolor styles lost: {raw}");
    assert!(
        !raw.contains("^["),
        "literal escaped ANSI in native frame: {raw}"
    );
    assert!(rows.iter().all(|row| sexy_tui_rs::visible_width(row) <= 37));
    let visible = sexy_tui_rs::strip_terminal_sequences(&raw);
    assert_eq!(visible.matches("Working").count(), 1, "{visible}");
    assert!(visible.contains("esc to interrupt"), "{visible}");
    shell.interrupt_run(run);
}

async fn rendered(shell: &mut InteractiveShell) -> String {
    shell.render();
    let frame = shell
        .dump_rendered_frame()
        .await
        .expect("live native renderer frame")
        .join("\n");
    // Test visible text across real SGR runs (the native loader styles each
    // character), but never mask the plain-profile escaping/truncation defect.
    assert!(
        !frame.contains("^["),
        "literal escaped ANSI in native frame: {frame}"
    );
    sexy_tui_rs::strip_terminal_sequences(&frame)
}

fn component_key(app: &mut App, shell: &mut InteractiveShell, code: KeyCode) {
    let event = Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
    assert!(
        app.executable_extensions
            .route_remote_ui_event(shell, &event, false),
        "native focused surface must own its input"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_dialog_keys_resolve_real_values_without_editing_native_draft() {
    let (directory, mut app, mut shell) = live_ui_app(r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  const record = (type, result) => appendFileSync(TRACE, JSON.stringify({type, result}) + '\n');
  pi.registerCommand('select', {handler: async (_, ctx) => record('select', await ctx.ui.select('NATIVE-SELECT', ['One', '前😀Two'], {timeout: 10000}))});
  pi.registerCommand('input', {handler: async (_, ctx) => record('input', await ctx.ui.input('NATIVE-INPUT', 'not-prefilled', {timeout: 10000}))});
  pi.registerCommand('confirm', {handler: async (_, ctx) => record('confirm', await ctx.ui.confirm('NATIVE-CONFIRM', 'Question', {timeout: 10000}))});
  pi.registerCommand('seed', {handler: (_, ctx) => ctx.ui.setEditorText('retained-native-draft')});
};
"#).await;
    shell.set_size(31, 14);
    command(&mut app, &mut shell, "seed").await.unwrap();
    let trace = directory.path().join("trace.jsonl");
    command(&mut app, &mut shell, "select").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SELECT")
    })
    .await;
    component_key(&mut app, &mut shell, KeyCode::Down);
    component_key(&mut app, &mut shell, KeyCode::Enter);
    pump_until(&mut app, &mut shell, |_| trace.exists()).await;
    command(&mut app, &mut shell, "input").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| frame.contains("NATIVE-INPUT")).await;
    assert!(!rendered(&mut shell).await.contains("not-prefilled"));
    for character in "hé😀".chars() {
        component_key(&mut app, &mut shell, KeyCode::Char(character));
    }
    component_key(&mut app, &mut shell, KeyCode::Enter);
    pump_until(&mut app, &mut shell, |_| {
        std::fs::read_to_string(&trace).unwrap().lines().count() == 2
    })
    .await;
    command(&mut app, &mut shell, "confirm").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-CONFIRM")
    })
    .await;
    component_key(&mut app, &mut shell, KeyCode::Esc);
    pump_until(&mut app, &mut shell, |_| {
        std::fs::read_to_string(&trace).unwrap().lines().count() == 3
    })
    .await;
    pump_until(&mut app, &mut shell, |frame| {
        !frame.contains("NATIVE-CONFIRM")
    })
    .await;
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            serde_json::json!({"type":"select", "result":"前😀Two"}),
            serde_json::json!({"type":"input", "result":"hé😀"}),
            serde_json::json!({"type":"confirm", "result":false})
        ]
    );
    assert_eq!(
        shell.extension_editor_snapshot().text,
        "retained-native-draft"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_overlay_on_handle_resize_done_and_dispose() {
    let (directory, mut app, mut shell) = live_ui_app(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => pi.registerCommand('overlay', {handler: async (_, ctx) => {
  const result = await ctx.ui.custom((_tui, theme, _keys, done) => ({
    render: width => [theme.fg('success', `NATIVE-OVERLAY width=${width}`)],
    handleInput: key => {if (key === 'x') done({actual: 'input'});},
    dispose: () => appendFileSync(TRACE, JSON.stringify({type:'disposed'}) + '\n'),
  }), {overlay: true, overlayOptions: {width: '100%'}, onHandle: handle => {
    if (!handle.isFocused()) throw new Error('overlay not focused');
    handle.setHidden(true); if (!handle.isHidden()) throw new Error('overlay not hidden');
    handle.setHidden(false); if (!handle.isFocused()) throw new Error('overlay did not refocus');
    appendFileSync(TRACE, JSON.stringify({type:'handle'}) + '\n');
  }});
  appendFileSync(TRACE, JSON.stringify({type:'done', result}) + '\n');
}});
"#,
    )
    .await;
    shell.set_size(37, 12);
    command(&mut app, &mut shell, "overlay").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-OVERLAY width=37")
    })
    .await;
    let resize = Event::Resize(29, 12);
    app.executable_extensions
        .route_remote_ui_event(&mut shell, &resize, false);
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-OVERLAY width=29")
    })
    .await;
    component_key(&mut app, &mut shell, KeyCode::Char('x'));
    let trace = directory.path().join("trace.jsonl");
    pump_until(&mut app, &mut shell, |_| {
        std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .contains("\"done\"")
    })
    .await;
    pump_until(&mut app, &mut shell, |frame| {
        !frame.contains("NATIVE-OVERLAY")
    })
    .await;
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0]["type"], "handle");
    assert_eq!(
        rows.iter().filter(|row| row["type"] == "disposed").count(),
        1
    );
    assert_eq!(
        rows.last().unwrap(),
        &serde_json::json!({"type":"done", "result":{"actual":"input"}})
    );
    app.executable_extensions.shutdown().await;
}

async fn pump_until(
    app: &mut App,
    shell: &mut InteractiveShell,
    mut ready: impl FnMut(&str) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            for message in app.executable_extensions.drain_events_for_shell(shell) {
                shell.notice(message);
            }
            let frame = rendered(shell).await;
            if ready(&frame) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real adapter UI did not reach its acceptance barrier");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_dialog_options_render_countdown_and_restore_on_timeout() {
    let (directory, mut app, mut shell) = live_ui_app(r#"
import { appendFileSync } from 'node:fs';
export default function(pi) {
  pi.registerCommand('probe', {handler: async (_, ctx) => {
    const stopped = new AbortController(); stopped.abort();
    if (await ctx.ui.confirm('Must not mount', 'Question', {signal: stopped.signal}) !== false) throw new Error('confirm cancel');
    if (await ctx.ui.select('Must not mount', ['One'], {signal: stopped.signal}) !== undefined) throw new Error('select cancel');
    const result = await ctx.ui.input('NATIVE-PI-INPUT', 'not-a-prefill', {timeout: 1});
    appendFileSync(TRACE, JSON.stringify({cancelled: result === undefined}) + '\n');
  }});
}
"#).await;
    shell.set_size(37, 12);
    command(&mut app, &mut shell, "probe").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-PI-INPUT")
    })
    .await;
    let snapshot = rendered(&mut shell).await;
    assert!(snapshot.contains("(1s)"), "{snapshot}");
    assert!(
        !snapshot.contains("not-a-prefill"),
        "Pi accepts but does not display placeholder"
    );
    let trace = directory.path().join("trace.jsonl");
    pump_until(&mut app, &mut shell, |_| trace.exists()).await;
    assert_eq!(
        std::fs::read_to_string(trace).unwrap().trim(),
        r#"{"cancelled":true}"#
    );
    pump_until(&mut app, &mut shell, |frame| {
        !frame.contains("NATIVE-PI-INPUT")
    })
    .await;
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_editor_factory_identity_and_composer_checkpoints() {
    let (_directory, mut app, mut shell) = live_ui_app(r#"
import { Editor } from '@earendil-works/pi-tui';
let configured;
export default function(pi) {
  pi.registerCommand('mount', {handler: async (_, ctx) => {
    if (ctx.ui.getEditorComponent() !== undefined) throw new Error('default factory');
    configured = (tui, theme) => new Editor(tui, theme);
    ctx.ui.setEditorComponent(configured);
    if (ctx.ui.getEditorComponent() !== configured) throw new Error('factory identity');
    ctx.ui.addAutocompleteProvider(current => ({...current, getSuggestions: () => null}));
  }});
  pi.registerCommand('edit', {handler: async (_, ctx) => {
    if (ctx.ui.getEditorComponent() !== configured) throw new Error('cross-context factory identity');
    ctx.ui.setEditorText('native-draft');
    ctx.ui.pasteToEditor('!');
  }});
  pi.registerCommand('clear', {handler: async (_, ctx) => {
    ctx.ui.setEditorComponent(undefined);
    if (ctx.ui.getEditorComponent() !== undefined) throw new Error('factory clear');
  }});
}
"#).await;
    command(&mut app, &mut shell, "mount").await.unwrap();
    command(&mut app, &mut shell, "edit").await.unwrap();
    assert_eq!(shell.extension_editor_snapshot().text, "native-draft!");
    assert!(rendered(&mut shell).await.contains("native-draft!"));
    command(&mut app, &mut shell, "clear").await.unwrap();
    assert_eq!(shell.extension_editor_snapshot().text, "native-draft!");
    assert!(!rendered(&mut shell).await.contains("Ctrl+G restore editor"));
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_chrome_controls_existing_loader_title_and_tool_disclosure() {
    let (directory, mut app, mut shell) = live_ui_app(
        r#"
import { appendFileSync } from 'node:fs';
export default function(pi) {
  pi.registerCommand('chrome', {handler: (_, ctx) => {
    ctx.ui.setTitle('NATIVE-PI-TITLE');
    ctx.ui.setWorkingMessage('NATIVE-PI-WORKING');
    ctx.ui.setWorkingIndicator({frames: ['S'], intervalMs: 17});
    ctx.ui.setWorkingVisible(true);
    ctx.ui.setHiddenThinkingLabel('NATIVE-PI-HIDDEN');
    ctx.ui.setToolsExpanded(false);
    if (ctx.ui.getToolsExpanded() !== false) throw new Error('committed expansion getter');
  }});
  pi.registerCommand('hide', {handler: (_, ctx) => {ctx.ui.setWorkingVisible(false);}});
  pi.registerCommand('read', {handler: (_, ctx) => {
    appendFileSync(TRACE, JSON.stringify({expanded: ctx.ui.getToolsExpanded()}) + '\n');
  }});
  pi.registerCommand('reset', {handler: (_, ctx) => {
    ctx.ui.setWorkingMessage(); ctx.ui.setWorkingIndicator();
    ctx.ui.setWorkingVisible(true); ctx.ui.setHiddenThinkingLabel();
  }});
}
"#,
    )
    .await;
    shell.set_size(80, 24);
    let run = shell.begin_run("offline-test");
    command(&mut app, &mut shell, "chrome").await.unwrap();
    assert_eq!(shell.extension_window_title(), "NATIVE-PI-TITLE");
    let visible = rendered(&mut shell).await;
    assert_eq!(
        visible.matches("NATIVE-PI-WORKING").count(),
        1,
        "loader was duplicated: {visible}"
    );
    assert!(!shell.verbose_tools());
    command(&mut app, &mut shell, "hide").await.unwrap();
    assert!(!rendered(&mut shell).await.contains("NATIVE-PI-WORKING"));
    shell.set_verbose_tools(true); // Native Ctrl+O must remain authoritative.
    command(&mut app, &mut shell, "read").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .trim(),
        r#"{"expanded":true}"#
    );
    shell.set_verbose_tools(false);
    command(&mut app, &mut shell, "reset").await.unwrap();
    let reset = rendered(&mut shell).await;
    assert!(
        !reset.contains("NATIVE-PI-WORKING") && !reset.contains("NATIVE-PI-HIDDEN"),
        "{reset}"
    );
    assert!(
        reset.contains("Working"),
        "native loader default was not restored: {reset}"
    );
    shell.interrupt_run(run);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_submits_through_native_composer() {
    let (_directory, mut app, mut shell) = live_ui_app(r#"
import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => {
  pi.on('session_start', (_, ctx) => ctx.ui.setEditorComponent((t, theme, keys) => new CustomEditor(t, theme, keys)));
  pi.registerCommand('fixture-command', {handler: () => {}});
  for (const [i, text] of ['/model', '/fixture-command', 'ordinary prompt'].entries())
    pi.registerCommand('seed-' + i, {handler: async (_, ctx) => { await ctx.ui.setEditorText(text); }});
};
"#).await;
    for (i, text) in ["/model", "/fixture-command", "ordinary prompt"]
        .into_iter()
        .enumerate()
    {
        command(&mut app, &mut shell, &format!("seed-{i}"))
            .await
            .unwrap();
        let mut input = futures_util::stream::iter([Ok(Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ))])
        .chain(futures_util::stream::pending());
        let mut scroll = tokio::time::interval(Duration::from_millis(16));
        let mut extensions = tokio::time::interval(Duration::from_millis(10));
        let (watcher, mut reload, mut tick) = super::support::test_reload();
        let idle = tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_prompt(
                &mut shell,
                &mut input,
                &mut scroll,
                &mut extensions,
                &mut app.executable_extensions,
                None,
                &mut tick,
                &watcher,
                &mut reload,
                Some(&mut app.agent),
            ),
        )
        .await
        .expect("custom editor submission did not reach the native composer")
        .unwrap();
        match idle {
            Idle::Command(command) if text.starts_with('/') => assert_eq!(command.trim_end(), text),
            Idle::Submit(input) if !text.starts_with('/') => assert_eq!(input.display_text, text),
            _ => panic!("custom editor bypassed native admission for {text}"),
        }
    }
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_terminal_input_consumes_transforms_and_passes_before_composer() {
    let (_directory, mut app, mut shell) = live_ui_app(r#"
export default pi => pi.registerCommand('listen', {handler: (_, ctx) => {
  ctx.ui.onTerminalInput(data => data === 'x' ? {consume: true} : data === 'a' ? {data: 'b'} : undefined);
  ctx.ui.onTerminalInput(data => data === 'b' ? {data: 'Z'} : data === '\x1bOA' ? {data: '^'} : undefined);
}});
"#).await;
    command(&mut app, &mut shell, "listen").await.unwrap();
    let mut input = raw_input(&shell, b"xaq\x1bOA\x04");
    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
    let (watcher, mut reload, mut reload_tick) = test_reload();
    wait_for_prompt(
        &mut shell,
        &mut input,
        &mut scroll_tick,
        &mut extension_tick,
        &mut app.executable_extensions,
        None,
        &mut reload_tick,
        &watcher,
        &mut reload,
        None,
    )
    .await
    .unwrap();
    assert_eq!(shell.extension_editor_snapshot().text, "Zq^");
    app.executable_extensions.shutdown().await;
}

// Feed bytes to the production foreground reader/decoder, not reconstructed
// crossterm keys or a handwritten protocol peer. The same owner serves all UI.
pub(super) fn raw_input(shell: &InteractiveShell, bytes: &[u8]) -> EventStream {
    use std::io::Write;
    use std::os::fd::OwnedFd;
    let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    writer.write_all(bytes).unwrap();
    EventStream::from_terminal_file(std::fs::File::from(OwnedFd::from(reader)))
        .with_interceptors(shell.terminal_input_interceptors())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_terminal_input_hung_handler_fails_open_once_without_freezing_typing() {
    let (_directory, mut app, mut shell) = live_ui_app(
        r#"
export default pi => pi.registerCommand('listen', {handler: (_, ctx) => {
  ctx.ui.onTerminalInput(() => { while (true) {} });
}});
"#,
    )
    .await;
    command(&mut app, &mut shell, "listen").await.unwrap();
    let draft = "hanging handler must not slow subsequent typing";
    let mut input = raw_input(&shell, format!("{draft}\x04").as_bytes());
    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    let mut extension_tick = tokio::time::interval(Duration::from_millis(10));
    let (watcher, mut reload, mut reload_tick) = test_reload();
    tokio::time::timeout(
        Duration::from_millis(500),
        wait_for_prompt(
            &mut shell,
            &mut input,
            &mut scroll_tick,
            &mut extension_tick,
            &mut app.executable_extensions,
            None,
            &mut reload_tick,
            &watcher,
            &mut reload,
            None,
        ),
    )
    .await
    .expect("hung extension froze native input")
    .unwrap();
    assert_eq!(shell.extension_editor_snapshot().text, draft);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_busy_commands_steering_follow_up_and_refusal() {
    use super::support::{scripted_agent_with_delay, test_run_inspection};
    for text in [
        "/model",
        "/fixture-command",
        "steer text",
        "follow-up text",
        "refused text",
    ] {
        let factory = r#"
import { CustomEditor } from '@earendil-works/pi-coding-agent';
export default pi => {
  pi.on('session_start', (_, ctx) => ctx.ui.setEditorComponent((t, theme, keys) => new CustomEditor(t, theme, keys)));
  pi.registerCommand('fixture-command', {handler: () => {}});
  pi.registerCommand('seed', {handler: async (_, ctx) => { await ctx.ui.setEditorText(TEXT); }});
};
"#.replace("TEXT", &serde_json::to_string(text).unwrap());
        let (_directory, mut app, mut shell) = live_ui_app(&factory).await;
        command(&mut app, &mut shell, "seed").await.unwrap();
        let follow_up = text == "follow-up text" || text == "refused text";
        if text == "refused text" {
            for _ in 0..MAX_PENDING_IDLE_ACTIONS {
                shell.queue_follow_up(ComposedInput::from_text("occupied".into()));
            }
        }
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(500)).await;
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let event = Event::Key(crossterm::event::KeyEvent::new(
            if follow_up {
                KeyCode::Char('s')
            } else {
                KeyCode::Enter
            },
            if follow_up {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
        ));
        let mut input =
            futures_util::stream::iter([Ok(event)]).chain(futures_util::stream::pending());
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        let mut pending = VecDeque::new();
        let ended = tokio::time::timeout(
            Duration::from_secs(8),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut pending,
                &mut false,
                None,
                None,
                &mut app.executable_extensions,
                &mut false,
                test_run_inspection(),
                &mut None,
            ),
        )
        .await
        .expect("custom-editor active run stalled")
        .unwrap();
        drop(run);
        assert_eq!(ended, HostRunOutcome::Completed);
        let context = format!("{:?}", agent.session().context().unwrap());
        match text {
            "/model" => {
                // RunFinished closes the live native picker. The command must
                // neither become provider input nor a queued prompt/command.
                assert!(pending.is_empty());
                assert_eq!(shell.queued_follow_up_len(), 0);
            }
            "/fixture-command" => assert!(
                matches!(pending.front(), Some(PendingIdleAction::ExtensionCommand {name, ..}) if name == "fixture-command")
            ),
            "steer text" => assert!(
                context.contains(text),
                "busy Enter did not steer: {context}"
            ),
            "follow-up text" => assert_eq!(shell.queued_follow_up_len(), 1),
            "refused text" => assert_eq!(shell.queued_follow_up_len(), MAX_PENDING_IDLE_ACTIONS),
            _ => unreachable!(),
        }
        if text != "steer text" {
            assert!(
                !context.contains(text),
                "command/follow-up leaked to the active provider"
            );
        }
        shell.close_panel();
        // The clear checkpoint follows admission, not the submit keystroke.
        if text != "refused text" {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    app.executable_extensions.drain_events_for_shell(&mut shell);
                    if shell.extension_editor_snapshot().text.is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("accepted editor did not clear after ACK");
        } else {
            assert_eq!(shell.extension_editor_snapshot().text, text);
            assert!(rendered(&mut shell).await.contains(text));
        }
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_terminal_input_consume_all_leaves_composer_empty_but_not_host_rescue() {
    let (_directory, mut app, mut shell) = live_ui_app(
        r#"
export default pi => pi.registerCommand('listen', {handler: (_, ctx) => {
  ctx.ui.onTerminalInput(() => ({consume: true}));
}});
"#,
    )
    .await;
    command(&mut app, &mut shell, "listen").await.unwrap();
    let mut input = raw_input(&shell, b"typed text is consumed\x1bOA\x04");
    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    let mut extension_tick = tokio::time::interval(Duration::from_millis(10));
    let (watcher, mut reload, mut reload_tick) = test_reload();
    let idle = tokio::time::timeout(
        Duration::from_secs(8),
        wait_for_prompt(
            &mut shell,
            &mut input,
            &mut scroll_tick,
            &mut extension_tick,
            &mut app.executable_extensions,
            None,
            &mut reload_tick,
            &watcher,
            &mut reload,
            None,
        ),
    )
    .await
    .expect("consuming extension stalled the input owner")
    .unwrap();
    assert!(matches!(idle, Idle::Quit), "Ctrl+D must remain host-owned");
    assert_eq!(shell.extension_editor_snapshot().text, "");
    app.executable_extensions.shutdown().await;
}
