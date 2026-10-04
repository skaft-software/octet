use super::super::{ExecutableExtensions, ExtensionConfirmationHandler};
use super::*;
use std::collections::VecDeque;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use octet_agent::extension_process::{
    ConfirmationRequest, DiscoveredExtension, ExtensionActivation, ExtensionManifest,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};

const FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
surfaces, opens, closes, hook_outputs = {}, {}, {}, set()
counter = 0

def send(value):
    print(json.dumps(dict(jsonrpc='2.0', **value)), flush=True)

def result(id, value):
    send(dict(id=id, result=value))

def output(id):
    if id in hook_outputs:
        result(id, dict(disposition=dict(action='continue'), notifications=[], context=[]))
    else:
        result(id, dict(text='', notifications=[], context=[]))

def frame(s):
    send(dict(method='ui/frame', params=dict(resource_owner=s['owner'], surface_id=s['id'],
        revision=s['revision'], columns=s['columns'], rows=s['rows'],
        lines=['\x1b[38;2;20;80;200mREMOTE FRAME\x1b[0m'])))
    s['revision'] += 1

def checkpoint(s, event, text):
    revision = event['editor_input']['input_revision']
    s['last_checkpoint_id'] = 'checkpoint-' + str(s['parent']) + '-' + str(revision)
    send(dict(id=s['last_checkpoint_id'], method='composer/set',
        params=dict(parent_request_id=s['parent'], resource_owner=s['owner'], text=text,
            editor_checkpoint=dict(surface_id=s['id'], mount_id=s['editor_mount_id'],
                input_revision=revision, checkpoint_revision=revision))))

for line in sys.stdin:
    m = json.loads(line)
    with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'remote-ui-wire.jsonl'), 'a') as log:
        log.write(line)
    method, p = m.get('method'), m.get('params', {})
    if method == 'initialize':
        result(m['id'], dict(api_version='0.4', tools=[], commands=[dict(name='mount', description='Remote fixture')],
            protocol=dict(version='0.4', features=['request_cancellation', 'content_parts', 'remote_ui', 'composer'],
                limits=dict(max_concurrent_requests=4))))
    elif method == 'hook/run':
        hook_outputs.add(m['id'])
        if p['hook'] == 'session_start':
            s = dict(id='session-footer', parent=m['id'], owner=p['context']['resource_owner'],
                hold=False, revision=0)
            opens['open-hook'] = s
            send(dict(id='open-hook', method='ui/open', params=dict(parent_request_id=m['id'],
                resource_owner=s['owner'], surface_id=s['id'], title='Session footer', placement='footer')))
        else:
            output(m['id'])
    elif method == 'command/execute':
        counter += 1
        args = p.get('arguments', [])
        s = dict(id='shared-editor' if 'same' in args else 'screen-' + str(counter),
            parent=m['id'], owner=p['context']['resource_owner'],
            hold='hold' in args, checkpoint='checkpoint' in args, barriers='barriers' in args, revision=0)
        opens['open-' + str(counter)] = s
        send(dict(id='open-' + str(counter), method='ui/open', params=dict(parent_request_id=m['id'],
            resource_owner=s['owner'], surface_id=s['id'], title='Remote fixture',
            placement=args[0] if args else 'fullscreen', mouse_capture='mouse' in args)))
    elif method == 'ui/key' and surfaces[p['surface_id']].get('barriers') and p['key'] == 's':
        s = surfaces[p['surface_id']]
        s['hold'] = False
        output(s['parent'])
    elif method == 'ui/key' and surfaces[p['surface_id']].get('barriers') and p['key'] == 'z':
        s = surfaces[p['surface_id']]
        send(dict(method='$/cancelRequest', params=dict(id=s['last_checkpoint_id'])))
        send(dict(method='notification', params=dict(message='checkpoint-cancelled')))
    elif method == 'ui/key' and p.get('key') in ('p', 'v'):
        s = surfaces[p['surface_id']]
        if p['key'] == 'v':
            id = 'close-' + s['id']
            closes[id] = s
            send(dict(id=id, method='ui/close', params=dict(parent_request_id=s['parent'],
                resource_owner=s['owner'], surface_id=s['id'])))
        send(dict(id='paste-' + s['id'], method='composer/insert',
            params=dict(parent_request_id=s['parent'], resource_owner=s['owner'], text='-inserted')))
    elif method == 'ui/key' and p.get('key') == 'q':
        s = surfaces[p['surface_id']]
        id = 'close-' + s['id']
        closes[id] = s
        send(dict(id=id, method='ui/close', params=dict(parent_request_id=s['parent'],
            resource_owner=s['owner'], surface_id=s['id'])))
    elif method == 'ui/key' and p.get('key') == 'h':
        workspace = os.environ['OCTET_WORKSPACE']
        with open(os.path.join(workspace, 'editor-hung'), 'w') as barrier:
            barrier.write('not servicing stdin or checkpoints')
        while not os.path.exists(os.path.join(workspace, 'editor-release')):
            time.sleep(0.001)
    elif method == 'ui/key' and surfaces[p['surface_id']].get('checkpoint'):
        s = surfaces[p['surface_id']]
        if p['key'] == 'c': checkpoint(s, p, 'acknowledged')
        elif p['key'] == 'x': s['late_checkpoint'] = p
    elif method == 'ui/resize':
        s = surfaces[p['surface_id']]
        s.update(columns=p['columns'], rows=p['rows'])
        s['revision'] += 1
        frame(s)
    elif method == 'ui/closed':
        s = surfaces.pop(p['surface_id'], None)
        if s and s.get('late_checkpoint'): checkpoint(s, s['late_checkpoint'], 'late overwrite')
        if s and s['hold']: output(s['parent'])
    elif method == '$/cancelRequest':
        for s in surfaces.values():
            if s['parent'] == p['id']:
                s['hold'] = False
                send(dict(id=p['id'], error=dict(code=-32800, message='cancelled')))
                break
    elif method == 'shutdown':
        result(m['id'], {})
        break
    elif 'id' in m and m['id'] in opens:
        s = opens.pop(m['id'])
        if 'error' in m:
            send(dict(id=s['parent'], error=m['error']))
        else:
            s.update(m['result'])
            surfaces[s['id']] = s
            frame(s)
            if not s['hold']: output(s['parent'])
    elif 'id' in m and m['id'] in closes:
        s = closes.pop(m['id'])
        if 'error' in m:
            send(dict(id=s['parent'], error=m['error']))
        else:
            surfaces.pop(s['id'], None)
            if s['hold']: output(s['parent'])
"#;

async fn fixture(temp: &tempfile::TempDir) -> (ExecutableExtensions, PathBuf) {
    let script = temp.path().join("remote-ui-fixture.py");
    std::fs::write(&script, FIXTURE).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest::parse(
        r#"
name = "remote-ui-fixture"
version = "0.4.0"
api_version = "0.4"
[entrypoint]
command = "remote-ui-fixture.py"
[contributes]
commands = ["mount"]
hooks = ["session_start", "session_end"]
notifications = true
"#,
    )
    .unwrap();
    let wake = Arc::new(tokio::sync::Notify::new());
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.host_state.session_id = Some("remote-test-session".into());
    runtime.remote_ui = Some(wake.clone());
    runtime.request_timeout = Duration::from_secs(3);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process);
    extensions.remote_ui_wake = Some(wake);
    extensions.resource_owner = Some("remote-test-session".into());
    extensions.session_id = extensions.resource_owner.clone();
    (extensions, temp.path().join("remote-ui-wire.jsonl"))
}

struct Frontend {
    shell: InteractiveShell,
    events: VecDeque<Event>,
    frames: Vec<Vec<String>>,
    fullscreen_before: bool,
    fullscreen_admitted: bool,
}

impl Frontend {
    fn new(events: impl IntoIterator<Item = Event>) -> Self {
        // Color assertions require an interactive terminal fixture, not
        // TestTerminal's ambient non-TTY capability detection.
        let (mut shell, _) =
            crate::tui::view::tests::emulated_shell(crate::tui::theme::test_theme(), 120, 40);
        shell.prefill_editor("untouched draft".into());
        let fullscreen_before = shell.has_remote_fullscreen_mount();
        Self {
            shell,
            events: events.into_iter().collect(),
            frames: Vec::new(),
            fullscreen_before,
            fullscreen_admitted: false,
        }
    }
}

impl ExtensionConfirmationHandler for Frontend {
    fn command_shell(&mut self) -> Option<&mut InteractiveShell> {
        if !self.fullscreen_before && self.shell.has_remote_fullscreen_mount() {
            self.fullscreen_admitted = true;
        }
        Some(&mut self.shell)
    }

    fn wait_for_command_event<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Event>>> + 'a>> {
        Box::pin(async move {
            if self.events.is_empty() {
                return std::future::pending().await;
            }
            loop {
                let frame = self.shell.dump_rendered_frame().await.unwrap();
                if frame.iter().any(|line| line.contains("REMOTE FRAME")) {
                    self.frames.push(frame);
                    return Ok(self.events.pop_front());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    }

    fn command_event(&mut self, event: Event) -> bool {
        if matches!(event, Event::Key(key)
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat))
        {
            true
        } else {
            panic!("focused component input leaked into host command handling: {event:?}")
        }
    }

    fn command_cancellation_event(&mut self, event: &Event) -> bool {
        matches!(event, Event::Key(key)
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat))
    }

    fn should_yield_to_fullscreen(&self) -> bool {
        self.fullscreen_admitted
            || (!self.fullscreen_before && self.shell.has_remote_fullscreen_mount())
    }

    fn confirm<'a>(
        &'a mut self,
        _extension: &'a str,
        _request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        Box::pin(std::future::ready(Ok(false)))
    }
}

fn key(code: KeyCode, kind: KeyEventKind, modifiers: KeyModifiers) -> Event {
    Event::Key(crossterm::event::KeyEvent::new_with_kind(
        code, modifiers, kind,
    ))
}

async fn command(extensions: &mut ExecutableExtensions, frontend: &mut Frontend, args: &[&str]) {
    tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_with_confirmation(
            "mount",
            args.iter().map(|arg| (*arg).into()).collect(),
            frontend,
        ),
    )
    .await
    .expect("the command must service UI while its result is pending")
    .unwrap()
    .unwrap();
}

fn wire(path: &PathBuf) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn remote_ui_close_then_paste_publishes_focus_within_one_request_drain() {
    use octet_agent::extension_process::ExtensionEvent;
    for (character, expected_requests) in [('p', 1), ('v', 2)] {
        let temp = tempfile::tempdir().unwrap();
        let (mut extensions, log) = fixture(&temp).await;
        let mut frontend = Frontend::new([]);
        command(&mut extensions, &mut frontend, &["fullscreen"]).await;
        let before = frontend.shell.extension_editor_snapshot().text;
        let mut observed = extensions.processes[0].subscribe();
        assert!(extensions.route_remote_ui_event(
            &mut frontend.shell,
            &key(
                KeyCode::Char(character),
                KeyEventKind::Press,
                KeyModifiers::NONE
            ),
        ));
        // Hold the product consumer until both real child requests are queued.
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut requests = 0;
            while requests < expected_requests {
                if matches!(
                    observed.recv().await.unwrap(),
                    ExtensionEvent::RemoteUiRequested { .. }
                        | ExtensionEvent::ComposerRequested { .. }
                ) {
                    requests += 1;
                }
            }
        })
        .await
        .unwrap();
        extensions.drain_events_for_shell(&mut frontend.shell);
        let expected = if character == 'v' {
            format!("{before}-inserted")
        } else {
            before
        };
        assert_eq!(frontend.shell.extension_editor_snapshot().text, expected);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(reply) = wire(&log).into_iter().find(|m| m["id"] == "paste-screen-1") {
                    if character == 'v' {
                        assert!(reply.get("result").is_some(), "{reply}");
                    } else {
                        assert!(
                            reply.get("error").is_some(),
                            "a blocked paste must not ACK: {reply}"
                        );
                        assert!(reply.to_string().contains("native composer"), "{reply}");
                    }
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_ui_session_start_services_reverse_requests_without_blocking_bootstrap() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    extensions.start_session_lifecycle();
    assert_eq!(extensions.pending_session_hook_starts.len(), 1);
    assert!(extensions.session_hook_start_tasks.is_empty());

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            extensions.drain_events_for_shell(&mut frontend.shell);
            extensions.drain_background_updates();
            let frame = frontend.shell.dump_rendered_frame().await.unwrap();
            if frame.iter().any(|line| line.contains("REMOTE FRAME"))
                && extensions
                    .session_hook_start_tasks
                    .iter()
                    .all(|task| task.is_finished())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a session hook must mount UI while the shell services its requests");
    assert!(extensions
        .diagnostics
        .iter()
        .all(|message| !message.contains("session_start hook")));
    assert_eq!(
        wire(&log)
            .iter()
            .filter(|message| message["params"]["hook"] == "session_start")
            .count(),
        1
    );
    extensions.release_binding().await;
    assert_eq!(
        wire(&log)
            .iter()
            .filter(|message| message["params"]["hook"] == "session_end")
            .count(),
        1
    );
}

#[tokio::test]
async fn remote_ui_command_services_live_frames_keys_and_mouse_until_close() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log) = fixture(&temp).await;
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 7,
        row: 3,
        modifiers: KeyModifiers::SHIFT,
    };
    let mut frontend = Frontend::new([
        key(KeyCode::Left, KeyEventKind::Repeat, KeyModifiers::NONE),
        key(KeyCode::Esc, KeyEventKind::Release, KeyModifiers::NONE),
        Event::Mouse(mouse),
        Event::Paste("must not reach the composer".into()),
        key(KeyCode::Char('q'), KeyEventKind::Press, KeyModifiers::NONE),
    ]);
    command(
        &mut extensions,
        &mut frontend,
        &["fullscreen", "hold", "mouse"],
    )
    .await;
    assert!(extensions.remote_ui.is_empty());
    assert!(!frontend.shell.has_overlay());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    assert!(frontend.frames.iter().any(|frame| frame
        .iter()
        .any(|line| line.contains("\x1b[38;2;20;80;200mREMOTE FRAME"))));
    assert!(!frontend.shell.debug_snapshot().contains("REMOTE FRAME"));
    let messages = wire(&log);
    let keys: Vec<_> = messages
        .iter()
        .filter(|m| m["method"] == "ui/key")
        .collect();
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0]["params"]["key"], "ArrowLeft");
    assert_eq!(keys[0]["params"]["kind"], "repeat");
    assert_eq!(keys[1]["params"]["key"], "Escape");
    assert_eq!(keys[1]["params"]["kind"], "release");
    let mouse = messages.iter().find(|m| m["method"] == "ui/mouse").unwrap();
    assert_eq!(mouse["params"]["x"], 7);
    assert_eq!(mouse["params"]["y"], 3);
    assert_eq!(mouse["params"]["kind"], "drag");
    assert!(
        !messages.iter().any(|m| m["method"] == "ui/closed"),
        "extension close uses its normal acknowledgement"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn attended_menu_command_lends_shell_and_yields_to_fullscreen_then_restores_native_shell() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log) = fixture(&temp).await;
    let mut frontend = Frontend::new([key(
        KeyCode::Char('q'),
        KeyEventKind::Press,
        KeyModifiers::NONE,
    )]);
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_menu_action_with_confirmation(
            "remote-ui-fixture",
            "Launch game",
            "Options",
            "mount",
            vec!["fullscreen".into(), "hold".into()],
            false,
            &mut frontend,
        ),
    )
    .await
    .expect("menu command services same-shell remote UI")
    .unwrap();
    assert!(output.trim().is_empty());
    assert!(frontend.should_yield_to_fullscreen());
    assert!(extensions.remote_ui.is_empty());
    assert!(frontend
        .frames
        .iter()
        .any(|frame| frame.iter().any(|line| line.contains("REMOTE FRAME"))));
    assert!(!frontend.shell.has_overlay());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    let native_frame = frontend.shell.dump_rendered_frame().await.unwrap();
    assert!(!native_frame
        .iter()
        .any(|line| line.contains("REMOTE FRAME")));
    assert!(native_frame
        .iter()
        .any(|line| line.contains("untouched draft")));
    let messages = wire(&log);
    assert!(messages
        .iter()
        .any(|message| message["method"] == "ui/key" && message["params"]["key"] == "q"));
    extensions.shutdown().await;
}

#[tokio::test]
async fn cancelling_attended_menu_command_restores_native_shell_and_draft() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([key(
        KeyCode::Char('c'),
        KeyEventKind::Press,
        KeyModifiers::CONTROL,
    )]);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_menu_action_with_confirmation(
            "remote-ui-fixture",
            "Launch game",
            "Options",
            "mount",
            vec!["fullscreen".into(), "hold".into()],
            false,
            &mut frontend,
        ),
    )
    .await
    .expect("cancel reaches the attended command")
    .unwrap_err();
    assert!(result.to_string().contains("cancelled"), "{result:#}");
    extensions.drain_events_for_shell(&mut frontend.shell);
    assert!(extensions.remote_ui.is_empty());
    assert!(!frontend.shell.has_overlay());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    let native_frame = frontend.shell.dump_rendered_frame().await.unwrap();
    assert!(!native_frame
        .iter()
        .any(|line| line.contains("REMOTE FRAME")));
    assert!(native_frame
        .iter()
        .any(|line| line.contains("untouched draft")));
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_ui_footer_survives_command_then_resize_and_session_fencing() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["footer"]).await;
    assert!(!extensions.remote_ui.is_empty());
    assert!(frontend
        .shell
        .dump_rendered_frame()
        .await
        .unwrap()
        .iter()
        .any(|line| line.contains("REMOTE FRAME")));
    let process = extensions.processes[0].clone();
    let mount = &extensions.remote_ui.mounts[0];
    let make_frame = |revision, columns, rows| ExtensionRemoteUiFrame {
        generation: mount.owner.process_generation,
        resource_owner: mount.owner.clone(),
        surface_id: mount.surface_id.clone(),
        revision,
        columns,
        rows,
        lines: vec!["accepted".into()],
    };
    let stale_geometry = make_frame(999, 13, 4);
    let mut foreign_owner = make_frame(999, mount.view.columns, mount.view.rows);
    foreign_owner.resource_owner.extension_instance_id = "foreign-process".into();
    let mut old_generation = make_frame(999, mount.view.columns, mount.view.rows);
    old_generation.generation += 1;
    let mut unsafe_control = make_frame(999, mount.view.columns, mount.view.rows);
    unsafe_control.lines = vec!["\x1b[2J".into()];
    let valid = make_frame(1, mount.view.columns, mount.view.rows);
    for refused in [
        stale_geometry,
        foreign_owner,
        old_generation,
        unsafe_control,
    ] {
        assert!(!extensions.remote_ui.accept_frame(&process, refused));
    }
    assert!(extensions.remote_ui.accept_frame(&process, valid));
    extensions.route_remote_ui_event(&mut frontend.shell, &Event::Resize(80, 24));
    // An old cache is blanked synchronously, before the remote resize reply.
    assert!(extensions
        .remote_ui
        .projection()
        .components
        .slot_lines(Some(&extensions.remote_ui.mounts[0].view.id))
        .unwrap()
        .is_empty());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            extensions.drain_events_for_shell(&mut frontend.shell);
            if frontend
                .shell
                .dump_rendered_frame()
                .await
                .unwrap()
                .iter()
                .any(|line| line.contains("REMOTE FRAME"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(wire(&log)
        .iter()
        .any(|m| m["method"] == "ui/resize" && m["params"]["columns"] == 80));
    extensions.resource_owner = Some("different-session".into());
    extensions.drain_events_for_shell(&mut frontend.shell);
    assert!(extensions.remote_ui.is_empty());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn remote_ui_rescue_and_process_reload_restore_draft() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = fixture(&temp).await;
    let mut frontend = Frontend::new([]);
    command(&mut extensions, &mut frontend, &["editor"]).await;
    command(&mut extensions, &mut frontend, &["fullscreen"]).await;
    assert_eq!(extensions.remote_ui.mounts.len(), 2);
    assert!(!extensions.route_remote_ui_event(
        &mut frontend.shell,
        &key(
            KeyCode::Char('d'),
            KeyEventKind::Press,
            KeyModifiers::CONTROL
        )
    ));
    assert!(extensions.route_remote_ui_event(
        &mut frontend.shell,
        &key(
            KeyCode::Char('g'),
            KeyEventKind::Press,
            KeyModifiers::CONTROL
        )
    ));
    assert_eq!(
        extensions.remote_ui.mounts.len(),
        1,
        "fullscreen rescue preserves the editor underneath"
    );
    assert!(!frontend.shell.has_overlay());
    assert!(extensions.route_remote_ui_event(
        &mut frontend.shell,
        &key(
            KeyCode::Char('g'),
            KeyEventKind::Press,
            KeyModifiers::CONTROL
        )
    ));
    assert!(extensions.remote_ui.is_empty());
    command(&mut extensions, &mut frontend, &["footer"]).await;
    extensions.processes[0].reload().await.unwrap();
    extensions.drain_events_for_shell(&mut frontend.shell);
    assert!(extensions.remote_ui.is_empty());
    assert_eq!(
        frontend.shell.extension_editor_snapshot().text,
        "untouched draft"
    );
    extensions.shutdown().await;
}

#[test]
fn remote_ui_normalizes_key_kinds_and_zero_based_mouse() {
    for (kind, expected) in [
        (KeyEventKind::Press, ExtensionRemoteUiKeyKind::Press),
        (KeyEventKind::Repeat, ExtensionRemoteUiKeyKind::Repeat),
        (KeyEventKind::Release, ExtensionRemoteUiKeyKind::Release),
    ] {
        let event = crossterm::event::KeyEvent::new_with_kind(
            KeyCode::BackTab,
            KeyModifiers::CONTROL,
            kind,
        );
        let normalized = normalized_key("surface", &event).unwrap();
        assert_eq!(normalized.key, "Tab");
        assert_eq!(normalized.kind, expected);
        assert_eq!(
            normalized.modifiers,
            [
                ExtensionRemoteUiKeyModifier::Shift,
                ExtensionRemoteUiKeyModifier::Control
            ]
        );
    }
    let mouse = normalized_mouse(
        "surface",
        &crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
    )
    .unwrap();
    assert_eq!((mouse.x, mouse.y, mouse.wheel_delta), (0, 0, 1));
}

#[path = "editor_tests.rs"]
mod editor_tests;
