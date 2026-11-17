//! Interactive activation returns the fleet before factory initialization.
// Unix-only like `pi_contract_support`: the probe drives terminal input
// through a Unix socket pair.
#![cfg(unix)]
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_toggle_returns_control_before_hooks_ready() {
    if std::env::var_os("OCTET_PI_TOGGLE_PROBE").is_none() {
        let home = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "modes::interactive::tests::pi_toggle_tests::pi_toggle_returns_control_before_hooks_ready", "--nocapture"])
            .env("OCTET_PI_TOGGLE_PROBE", "1")
            .env("OCTET_STARTUP_TRACE", "1")
            .env("HOME", home.path()).env("TMPDIR", tmp.path())
            .env_remove("XDG_CONFIG_HOME")
            .output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{stderr}");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let ack = stderr
            .find("extensions.toggle.acknowledged")
            .expect("activation ACK trace");
        let control = stderr
            .find("extensions.toggle.control-returned")
            .expect("editable control trace");
        assert!(ack < control);
        return;
    }
    let reference = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(
            "../../scripts/fixtures/pi-reference/node_modules/@earendil-works/pi-ai/dist/index.js",
        )
        .canonicalize()
        .expect("install the pi-reference fixture as in CI");
    let factory = r#"
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { contentText } from REFERENCE;
export default async pi => {
  const gate = join(process.cwd(), 'hold-pi-init');
  while (existsSync(gate)) await new Promise(resolve => setTimeout(resolve, 10));
  if (contentText([{type:'text', text:'reference'}]) !== 'reference') throw new Error('missing reference');
  pi.on('input', event => ({action: 'transform', text: event.text + '-hooked'}));
  pi.registerCommand('toggle-ready', {handler: () => {}});
};
"#.replace("REFERENCE", &serde_json::to_string(&url::Url::from_file_path(reference).unwrap().as_str()).unwrap());
    let (directory, mut app, mut shell) = pi_contract_support::pi_ui_app(&factory);
    // Build the disabled state first, then use the same rebuild as /extensions.
    app.config.enabled_extensions.clear();
    use std::os::fd::OwnedFd;
    let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    let mut input = EventStream::from_terminal_file(std::fs::File::from(OwnedFd::from(reader)));
    (app, _) = reload_resources(app, &mut shell, &mut input).await.unwrap();
    shell.set_size(100, 30);
    shell.finish_startup();
    shell.extension_set_editor("user draft".into());
    let gate = directory.path().join("hold-pi-init");
    std::fs::write(&gate, "hold").unwrap();
    let started = std::time::Instant::now();
    let rebuilt = tokio::time::timeout(
        Duration::from_secs(2),
        set_extension_enabled(app, &mut shell, &mut input, "octet-pi-compat", false),
    )
    .await;
    // A failed old synchronous path must release its owned worker too.
    if rebuilt.is_err() {
        std::fs::remove_file(&gate).unwrap();
    }
    let (mut app, changed) = rebuilt
        .expect("control must return while the Pi factory is still blocked")
        .unwrap();
    eprintln!("pi-toggle control={}us", started.elapsed().as_micros());
    assert!(changed);
    assert!(app.executable_extensions.startup_pending());
    assert_eq!(
        app.executable_extensions.pending_prompt_hook_names(),
        ["octet-pi-compat"]
    );
    assert_eq!(shell.pending(), "user draft");
    handle_cancellable_wait_input(&mut shell, Event::Paste("-editable".into()));
    assert_eq!(shell.pending(), "user draft-editable");
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(
        sexy_tui_rs::strip_terminal_sequences(&frame).contains("Loading your Pi setup"),
        "{frame}"
    );
    // Cancelling an early submission must not retire its still-starting hooks.
    assert!(tokio::time::timeout(
        Duration::from_millis(100),
        app.await_extension_prompt_hooks(),
    )
    .await
    .is_err());
    assert!(app.executable_extensions.startup_pending());
    assert_eq!(
        app.executable_extensions.pending_prompt_hook_names(),
        ["octet-pi-compat"]
    );
    // The draft was editable and the pending status painted with the gate shut.
    std::fs::remove_file(&gate).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.executable_extensions.startup_pending() {
            app.pump_extension_startup().await.unwrap();
            tokio::task::yield_now().await;
        }
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
            .await
            .unwrap();
    })
    .await
    .unwrap();
    eprintln!("pi-toggle hooks-ready={}us", started.elapsed().as_micros());
    assert!(!app.executable_extensions.has_pending_session_starts());
    assert!(app
        .executable_extensions
        .command_owner("toggle-ready")
        .is_some());
    assert_eq!(shell.pending(), "user draft-editable");
    let mut events = futures_util::stream::pending::<std::io::Result<Event>>();
    let mut frontend = ShellPromptFrontend::new(&mut shell, &mut events);
    let processed = app
        .executable_extensions
        .process_input_with_frontend(
            "first".into(),
            None,
            "interactive",
            None,
            Some(&mut frontend),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(processed.text, "first-hooked");
    // A second activation stays gated through close: shutdown retires the
    // pending process instead of waiting for the factory or replaying input.
    std::fs::write(&gate, "hold").unwrap();
    (app, _) = reload_resources_after_toggle(app, &mut shell, &mut input)
        .await
        .unwrap();
    assert!(app.executable_extensions.startup_pending());
    assert!(handle_cancellable_wait_input(
        &mut shell,
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))
    ));
    assert!(shell.close_requested());
    assert_eq!(shell.pending(), "user draft-editable");
    tokio::time::timeout(Duration::from_secs(2), app.executable_extensions.shutdown())
        .await
        .unwrap();
    std::fs::remove_file(gate).unwrap();
}
