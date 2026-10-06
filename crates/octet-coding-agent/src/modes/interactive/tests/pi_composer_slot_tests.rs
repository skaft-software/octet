//! U17: one terminal owner for the composer slot.
//!
//! A Pi custom editor is a composer-slot projection, not a second TUI: Octet's
//! chrome, slash popup, queue indicators and input policy stay authoritative,
//! and only the reserved grammar (submit, follow-up, clear, close) is the
//! host's. These tests drive the real native loops with the real adapter.
#![cfg(unix)]

use super::pi_contract_support::{command, pi_ui_app_with_native_command};
use super::*;

/// A Pi custom editor that marks its own rendered rows, plus a footer, a Pi
/// command and a shared command name for the single registry.
const SLOT_FACTORY: &str = r#"
import { CustomEditor } from '@earendil-works/pi-coding-agent';
import { appendFileSync } from 'node:fs';
class SlotEditor extends CustomEditor {
  render(width) { return [...super.render(width), 'NATIVE-SLOT-EDITOR']; }
}
export default pi => {
  let editor;
  pi.registerCommand('inspect-editor', {handler: () => appendFileSync(TRACE, JSON.stringify({
    text: editor.getText(), expanded: editor.getExpandedText(), pastes: [...editor.pastes], undo: editor.undoStack,
  }) + '\n')});
  pi.registerCommand('fixture-command', {handler: () => {}});
  pi.registerCommand('retire-editor', {handler: (_args, ctx) => ctx.ui.setEditorComponent(undefined)});
  pi.registerCommand('listen-all', {handler: (_args, ctx) => ctx.ui.onTerminalInput(data => {
    appendFileSync(TRACE, JSON.stringify({input: data}) + '\n'); return {consume: true};
  })});
  pi.registerCommand('seed-slot', {handler: (_, ctx) => ctx.ui.setEditorText('host draft')});
  pi.registerCommand('seed-slash', {handler: (_, ctx) => ctx.ui.setEditorText('/fixture-comm')});
  pi.registerCommand('shared', {handler: () => {}});
  pi.on('session_start', (_, ctx) => {
    ctx.ui.setEditorComponent((t, theme, keys) => editor = new SlotEditor(t, theme, keys));
    ctx.ui.setFooter(() => ({render: () => ['NATIVE-SLOT-FOOTER'], invalidate() {}}));
  });
};
"#;

async fn live_slot_app() -> (tempfile::TempDir, App, InteractiveShell) {
    let (directory, mut app, mut shell) =
        pi_ui_app_with_native_command(SLOT_FACTORY, "first-owner", "shared");
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

async fn rendered(shell: &mut InteractiveShell) -> String {
    shell.render();
    let frame = shell
        .dump_rendered_frame()
        .await
        .expect("live native renderer frame")
        .join("\n");
    assert!(
        !frame.contains("^["),
        "literal escaped ANSI in native frame: {frame}"
    );
    sexy_tui_rs::strip_terminal_sequences(&frame)
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

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(crossterm::event::KeyEvent::new(code, modifiers))
}

/// Deliver events on a real timer so the production loop can drain the adapter
/// between two keystrokes exactly as a user's typing would.
fn scripted(
    events: Vec<(u64, Event)>,
) -> tokio_stream::wrappers::ReceiverStream<std::io::Result<Event>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        for (delay_ms, event) in events {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if sender.send(Ok(event)).await.is_err() {
                return;
            }
        }
        // Keep the stream open so the loop does not read EOF before asserting.
        sender.closed().await;
    });
    tokio_stream::wrappers::ReceiverStream::new(receiver)
}

/// Wait for each real checkpoint, not an arbitrary typing delay.
async fn type_into_slot(app: &mut App, shell: &mut InteractiveShell, text: &str) {
    for character in text.chars() {
        assert!(app.executable_extensions.route_remote_ui_event(
            shell,
            &key(KeyCode::Char(character), KeyModifiers::NONE),
            false
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            let expected = format!("{}{character}", shell.extension_editor_snapshot().text);
            loop {
                app.executable_extensions.drain_events_for_shell(shell);
                if shell.extension_editor_snapshot().text == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("slot checkpoint did not arrive");
    }
}

struct IdleDriver<'a> {
    shell: &'a mut InteractiveShell,
    extensions: &'a mut crate::extensions::ExecutableExtensions,
    agent: &'a mut octet_agent::Agent,
    scroll: tokio::time::Interval,
    ticks: tokio::time::Interval,
    reload_watcher: crate::reload::ReloadWatcher,
    reload: crate::reload::ReloadSupervisor,
    reload_tick: tokio::time::Interval,
}

impl IdleDriver<'_> {
    async fn next_idle<S>(&mut self, input: &mut S) -> Idle
    where
        S: Stream<Item = std::io::Result<Event>> + Unpin,
    {
        tokio::time::timeout(
            Duration::from_secs(8),
            wait_for_prompt(
                self.shell,
                input,
                &mut self.scroll,
                &mut self.ticks,
                self.extensions,
                None,
                &mut self.reload_tick,
                &self.reload_watcher,
                &mut self.reload,
                Some(self.agent),
            ),
        )
        .await
        .expect("custom editor idle acceptance timed out")
        .unwrap()
    }
}

async fn idle_driver<'a>(
    shell: &'a mut InteractiveShell,
    extensions: &'a mut crate::extensions::ExecutableExtensions,
    agent: &'a mut octet_agent::Agent,
) -> IdleDriver<'a> {
    let (reload_watcher, reload, reload_tick) = super::support::test_reload();
    IdleDriver {
        shell,
        extensions,
        agent,
        scroll: tokio::time::interval(Duration::from_millis(16)),
        ticks: tokio::time::interval(Duration::from_millis(10)),
        reload_watcher,
        reload,
        reload_tick,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_keeps_octet_chrome_and_one_slash_popup() {
    let (_directory, mut app, mut shell) = live_slot_app().await;
    shell.set_size(96, 24);
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-FOOTER")
    })
    .await;

    let mut input = scripted(vec![
        (200, key(KeyCode::Char('/'), KeyModifiers::NONE)),
        (400, key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
    ]);
    let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await
        .next_idle(&mut input)
        .await;
    assert!(matches!(idle, Idle::Quit), "Ctrl+D must still close Octet");

    let frame = rendered(&mut shell).await;
    // The slot still renders the Pi component's own rows.
    assert!(frame.contains("NATIVE-SLOT-EDITOR"), "{frame}");
    // Octet chrome stays around the slot.
    assert!(frame.contains("NATIVE-SLOT-FOOTER"), "{frame}");
    // One popup, listing native, native-extension and Pi commands.
    assert!(frame.contains("/model"), "{frame}");
    let registry = native_slash_command_suggestions(&app.executable_extensions);
    assert!(registry.iter().any(|(name, _)| name == "shared"));
    assert!(registry.iter().any(|(name, _)| name == "fixture-command"));
    let popups = frame.matches("navigate").count();
    assert_eq!(popups, 1, "exactly one composer popup: {frame}");
    let pi_rows = frame.matches("/model").count();
    assert_eq!(
        pi_rows, 1,
        "the Pi editor's own list must not duplicate the host popup: {frame}"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_admits_commands_and_text_through_the_slot() {
    let (_directory, mut app, mut shell) = live_slot_app().await;
    shell.set_size(96, 24);
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    let owner = app
        .executable_extensions
        .command_owner("shared")
        .expect("shared command owner");
    for text in ["/model", "/fixture-command", "/shared", "ordinary prompt"] {
        type_into_slot(&mut app, &mut shell, text).await;
        let mut input = scripted(vec![(10, key(KeyCode::Enter, KeyModifiers::NONE))]);
        let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
            .await
            .next_idle(&mut input)
            .await;
        match idle {
            Idle::Command(command) if text.starts_with('/') => assert_eq!(command.trim_end(), text),
            Idle::Submit(input) if !text.starts_with('/') => assert_eq!(input.display_text, text),
            _ => panic!("slot editor bypassed native admission for {text}"),
        }
        // The draft left the slot only after native admission consumed it.
        pump_until(&mut app, &mut shell, |frame| !frame.contains(text)).await;
        assert!(
            shell.extension_editor_snapshot().text.is_empty(),
            "{text} was not consumed"
        );
    }
    assert!(
        app.executable_extensions.command_owner("shared").as_deref() == Some(owner.as_str()),
        "the shared command kept its owner"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_ctrl_g_never_unmounts_the_composer_slot() {
    let (_directory, mut app, mut shell) = live_slot_app().await;
    shell.set_size(96, 24);
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;

    // Ctrl+G is the editor's own key while a custom editor owns the slot.
    let mut input = scripted(vec![
        (100, key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        (300, key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
    ]);
    let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await
        .next_idle(&mut input)
        .await;
    assert!(matches!(idle, Idle::Quit));
    let frame = rendered(&mut shell).await;
    assert!(
        frame.contains("NATIVE-SLOT-EDITOR"),
        "Ctrl+G unmounted the composer slot: {frame}"
    );
    assert!(shell.extension_editor_owns_composer());
    app.executable_extensions.shutdown().await;

    // A genuine full-screen extension view keeps its explicit rescue.
    let factory = r#"
export default pi => {
  pi.registerCommand('listen', {handler: (_, ctx) => ctx.ui.onTerminalInput(() => ({consume: true}))});
  pi.registerCommand('fullscreen', {handler: async (_, ctx) => {
  await ctx.ui.custom((tui, theme, keys, done) => ({
    render: () => ['NATIVE-FULLSCREEN-VIEW'],
    handleInput: () => {},
    invalidate: () => {},
  }));
  }});
};
"#;
    let (_directory, mut app, mut shell) = {
        let (directory, mut app, mut shell) =
            pi_ui_app_with_native_command(factory, "first-owner", "shared");
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
    };
    shell.set_size(96, 24);
    command(&mut app, &mut shell, "listen").await.unwrap();
    command(&mut app, &mut shell, "fullscreen").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-FULLSCREEN-VIEW")
    })
    .await;
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"x\x07\x04");
    let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await
        .next_idle(&mut input)
        .await;
    assert!(matches!(idle, Idle::Quit));
    let frame = rendered(&mut shell).await;
    assert!(
        !frame.contains("NATIVE-FULLSCREEN-VIEW"),
        "Ctrl+G must return from a full-screen extension view: {frame}"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_registry_namespaces_a_shared_command_name() {
    let (_directory, mut app, mut shell) = live_slot_app().await;
    shell.set_size(96, 24);
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    let registry = app.executable_extensions.command_registry();
    let shared: Vec<_> = registry
        .iter()
        .filter(|entry| entry.public_name.ends_with("shared"))
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 2, "both owners stay registered: {registry:?}");
    let plain = shared
        .iter()
        .find(|entry| entry.public_name == "shared")
        .expect("one owner keeps the plain name");
    let namespaced = shared
        .iter()
        .find(|entry| entry.public_name != "shared")
        .expect("the other owner is namespaced");
    assert_eq!(
        namespaced.public_name,
        format!("{}:shared", namespaced.owner),
        "collision namespace is owner-qualified"
    );
    for entry in [plain, namespaced] {
        assert_eq!(
            app.executable_extensions
                .command_owner(&entry.public_name)
                .as_deref(),
            Some(entry.owner.as_str()),
            "{}",
            entry.public_name
        );
        assert_eq!(
            app.executable_extensions
                .registered_command_name(&entry.owner, &entry.public_name)
                .as_deref(),
            Some("shared"),
            "the extension still receives its own registered name"
        );
    }
    let public_name = namespaced.public_name.clone();
    type_into_slot(&mut app, &mut shell, &format!("/{public_name}")).await;
    let mut input = scripted(vec![(400, key(KeyCode::Char('d'), KeyModifiers::CONTROL))]);
    let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await
        .next_idle(&mut input)
        .await;
    assert!(matches!(idle, Idle::Quit));
    let frame = rendered(&mut shell).await;
    assert!(frame.contains(&format!("/{public_name}")), "{frame}");
    app.executable_extensions.shutdown().await;
}

/// The same admission contract while a run owns the agent: Enter steers,
/// Ctrl+S queues a follow-up, and a refused follow-up keeps the slot draft.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_busy_slot_steers_queues_and_keeps_refused_draft() {
    use super::support::{scripted_agent_with_delay, test_run_inspection};
    for text in [
        "/model",
        "/fixture-command",
        "steer text",
        "follow-up text",
        "refused text",
    ] {
        let (_directory, mut app, mut shell) = live_slot_app().await;
        shell.set_size(96, 24);
        pump_until(&mut app, &mut shell, |frame| {
            frame.contains("NATIVE-SLOT-EDITOR")
        })
        .await;
        let follow_up = text == "follow-up text" || text == "refused text";
        if text == "refused text" {
            for _ in 0..MAX_PENDING_IDLE_ACTIONS {
                shell.queue_follow_up(ComposedInput::from_text("occupied".into()));
            }
        }
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(600)).await;
        type_into_slot(&mut app, &mut shell, text).await;
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut input = scripted(vec![(
            10,
            key(
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
            ),
        )]);
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        let mut pending = VecDeque::new();
        let ended = tokio::time::timeout(
            Duration::from_secs(10),
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
            "/model" => assert!(pending.is_empty()),
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
        if text == "refused text" {
            // A refused submission keeps the genuine slot draft.
            assert_eq!(shell.extension_editor_snapshot().text, text);
            assert!(rendered(&mut shell).await.contains(text));
        } else {
            // Acceptance, not the keystroke, clears the slot.
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
            .expect("accepted editor did not clear after admission");
        }
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_burst_enter_waits_for_its_completed_checkpoint() {
    let (_directory, mut app, mut shell) = live_slot_app().await;
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    let events = "burst prompt"
        .chars()
        .map(|c| (0, key(KeyCode::Char(c), KeyModifiers::NONE)))
        .chain([(0, key(KeyCode::Enter, KeyModifiers::NONE))])
        .collect();
    let mut input = scripted(events);
    let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await
        .next_idle(&mut input)
        .await;
    assert!(
        matches!(idle, Idle::Submit(ref composed) if composed.display_text == "burst prompt"),
        "Enter must admit its whole completed draft, never an empty or partial checkpoint"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_refused_paste_retains_payload_and_undo() {
    use super::support::{scripted_agent_with_delay, test_run_inspection};
    let (directory, mut app, mut shell) = live_slot_app().await;
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    type_into_slot(&mut app, &mut shell, "prefix").await;
    let paste = "pasted line\n".repeat(50);
    assert!(app.executable_extensions.route_remote_ui_event(
        &mut shell,
        &Event::Paste(paste.clone()),
        false
    ));
    let expanded = format!("prefix{paste}");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            app.executable_extensions.drain_events_for_shell(&mut shell);
            if shell.pending() == expanded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    command(&mut app, &mut shell, "inspect-editor")
        .await
        .unwrap();
    for _ in 0..MAX_PENDING_IDLE_ACTIONS {
        shell.queue_follow_up(ComposedInput::from_text("occupied".into()));
    }
    let (_server, _workspace, mut agent) =
        scripted_agent_with_delay(Duration::from_millis(500)).await;
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut input = scripted(vec![(10, key(KeyCode::Char('s'), KeyModifiers::CONTROL))]);
    let mut ticker = tokio::time::interval(Duration::from_millis(5));
    tokio::time::timeout(
        Duration::from_secs(8),
        drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut VecDeque::new(),
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
    .unwrap()
    .unwrap();
    drop(run);
    assert_eq!(shell.pending(), expanded);
    command(&mut app, &mut shell, "inspect-editor")
        .await
        .unwrap();
    let states: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(states.len(), 2);
    assert_ne!(
        states[0]["text"], states[0]["expanded"],
        "real compressed paste"
    );
    assert_eq!(
        states[0], states[1],
        "refusal preserves draft, payloads and undo exactly"
    );
    assert!(app.executable_extensions.route_remote_ui_event(
        &mut shell,
        &key(KeyCode::Char('-'), KeyModifiers::CONTROL),
        false
    ));
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("prefix") && !frame.contains("paste #")
    })
    .await;
    assert_eq!(
        shell.pending(),
        "prefix",
        "undo restores the pre-paste draft"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_buffer_is_bounded_and_retirement_discards_it() {
    let (_directory, mut app, mut shell) = live_slot_app().await;
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    assert!(app.executable_extensions.route_remote_ui_event(
        &mut shell,
        &key(KeyCode::Char('x'), KeyModifiers::NONE),
        false
    ));
    assert!(app.executable_extensions.route_remote_ui_event(
        &mut shell,
        &key(KeyCode::Enter, KeyModifiers::NONE),
        false
    ));
    assert!(
        app.executable_extensions
            .take_ready_composer_event(&mut shell)
            .is_none(),
        "Enter must wait for the outstanding input checkpoint"
    );
    for _ in 0..128 {
        assert!(app.executable_extensions.route_remote_ui_event(
            &mut shell,
            &Event::Paste("buffered".into()),
            false
        ));
    }
    assert!(shell
        .debug_error()
        .as_deref()
        .is_some_and(|error| error.contains("input queue is full")));
    command(&mut app, &mut shell, "retire-editor")
        .await
        .unwrap();
    pump_until(&mut app, &mut shell, |frame| {
        !frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    assert!(
        app.executable_extensions
            .take_ready_composer_event(&mut shell)
            .is_none(),
        "retired input cannot submit or edit the native replacement"
    );
    assert_eq!(shell.pending(), "x");
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_host_keys_precede_terminal_consumers() {
    let (directory, mut app, mut shell) = live_slot_app().await;
    pump_until(&mut app, &mut shell, |frame| {
        frame.contains("NATIVE-SLOT-EDITOR")
    })
    .await;
    command(&mut app, &mut shell, "listen-all").await.unwrap();
    command(&mut app, &mut shell, "seed-slot").await.unwrap();
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"x\x07\r");
    let idle = idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await
        .next_idle(&mut input)
        .await;
    assert!(
        matches!(idle, Idle::Submit(input) if input.display_text == "host draft"),
        "a terminal consumer swallowed host submit"
    );
    pump_until(&mut app, &mut shell, |frame| !frame.contains("host draft")).await;

    command(&mut app, &mut shell, "seed-slash").await.unwrap();
    pump_until(&mut app, &mut shell, |frame| frame.contains("navigate")).await;
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"\t\x1b[Z");
    assert!(matches!(
        idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
            .await
            .next_idle(&mut input)
            .await,
        Idle::CycleThinking
    ));
    assert_eq!(
        shell.pending(),
        "/fixture-command ",
        "native slash completion precedes consumers"
    );
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"\x03\x1b[Z");
    assert!(matches!(
        idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
            .await
            .next_idle(&mut input)
            .await,
        Idle::CycleThinking
    ));
    assert!(
        shell.pending().is_empty(),
        "native clear precedes consumers"
    );
    pump_until(&mut app, &mut shell, |frame| !frame.contains("/model")).await;

    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert("tui.input.submit".into(), vec!["alt+s".into()]);
    shell.set_session_keybindings(&overrides);
    command(&mut app, &mut shell, "seed-slot").await.unwrap();
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"\x1b[115;3u");
    assert!(
        matches!(idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
        .await.next_idle(&mut input).await, Idle::Submit(input) if input.display_text == "host draft"),
        "the host's live user override precedes consumers"
    );
    pump_until(&mut app, &mut shell, |frame| !frame.contains("host draft")).await;
    command(&mut app, &mut shell, "seed-slot").await.unwrap();
    // Native transcript search owns its query, not the slot or raw consumers.
    // Ctrl+C closes that query without discarding the slot's original draft.
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"\x1b[102;6uquery\x03\x1b[Z");
    assert!(matches!(
        idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
            .await
            .next_idle(&mut input)
            .await,
        Idle::CycleThinking
    ));
    assert_eq!(shell.pending(), "host draft");
    assert!(!shell.transcript_search_active());
    let inputs: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(
        inputs,
        [
            serde_json::json!({"input": "x"}),
            serde_json::json!({"input": "\u{7}"})
        ],
        "only ordinary editing reaches the consumer"
    );
    let mut input = super::pi_ui_contract_tests::raw_input(&shell, b"\x04");
    assert!(matches!(
        idle_driver(&mut shell, &mut app.executable_extensions, &mut app.agent)
            .await
            .next_idle(&mut input)
            .await,
        Idle::Quit
    ));
    assert!(
        shell.close_requested(),
        "coordinated close precedes consumers"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_editor_busy_admission_precedes_terminal_consumers() {
    use super::support::{scripted_agent_with_delay, test_run_inspection};
    for (bytes, follow_up) in [(b"\r".as_slice(), false), (b"\x13".as_slice(), true)] {
        let (_directory, mut app, mut shell) = live_slot_app().await;
        pump_until(&mut app, &mut shell, |frame| {
            frame.contains("NATIVE-SLOT-EDITOR")
        })
        .await;
        command(&mut app, &mut shell, "listen-all").await.unwrap();
        command(&mut app, &mut shell, "seed-slot").await.unwrap();
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(500)).await;
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut input = super::pi_ui_contract_tests::raw_input(&shell, bytes);
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        tokio::time::timeout(
            Duration::from_secs(8),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut VecDeque::new(),
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
        .unwrap()
        .unwrap();
        drop(run);
        let context = format!("{:?}", agent.session().context().unwrap());
        if follow_up {
            assert_eq!(shell.queued_follow_up_len(), 1);
            assert!(
                !context.contains("host draft"),
                "follow-up leaked to the active provider"
            );
        } else {
            assert!(
                context.contains("host draft"),
                "native steering was swallowed by the terminal consumer: {context}"
            );
        }
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
        .expect("accepted editor did not clear after admission");
        assert!(shell.pending().is_empty());
        app.executable_extensions.shutdown().await;
    }
}
