//! Composer commands execute before a held provider response is released.
#![cfg(unix)]
use super::pi_contract_support::pi_ui_app_with_native_command;
use super::support::{scripted_model, text_turn, HeldApi};
use super::*;

const FACTORY: &str = r#"export default function(pi) {
  pi.registerCommand('live-command', { handler: async (_args, ctx) => {
    ctx.ui.notify('Pi command ran during response', 'info');
  }});
  pi.registerCommand('stalled-command', { handler: async () => {
    for (;;) {}
  }});
  pi.registerCommand('broken-command', { handler: async () => {
    throw new Error('private exception must not reach the transcript');
  }});
}"#;

async fn composer_command_during_response(command: &str, expected: &str) {
    let (_directory, mut app, mut shell) =
        pi_ui_app_with_native_command(FACTORY, "native-probe", "native-command");
    let (server, started, release) = HeldApi::start(text_turn()).await;
    let model = scripted_model(&server.uri);
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    let mut startup = futures_util::stream::pending();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut startup)
        .await
        .unwrap();
    let inspection = ActiveRunInspection::capture(&app);
    let run_id = shell.begin_run("test");
    let mut run = app.agent.prompt("original response").await.unwrap();
    shell.set_awaiting_provider(run_id);
    tokio::pin!(started);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut started => { result.unwrap(); break; }
                event = run.next() => shell.on_run_event(run_id, &event.unwrap()),
            }
        }
    })
    .await
    .unwrap();
    let events = command
        .chars()
        .map(|ch| {
            Ok(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )))
        })
        .chain(std::iter::once(Ok(Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ))));
    let mut input = futures_util::stream::iter(events).chain(futures_util::stream::pending());
    let mut pending = VecDeque::new();
    let control = run.control();
    let mut quit = false;
    let mut made_tool_call = false;
    let mut deadline = None;
    let result = tokio::time::timeout(
        Duration::from_secs(if command.contains("stall") { 34 } else { 2 }),
        drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut app.executable_extensions,
            &mut made_tool_call,
            &inspection,
            &mut deadline,
        ),
    )
    .await;
    assert!(result.is_err(), "the original response must remain held");
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(
        frame.contains(expected),
        "command did not execute while busy: {frame}"
    );
    assert!(
        pending.is_empty(),
        "extension command was deferred: {pending:?}"
    );
    assert!(!frame.contains("private exception"), "{frame}");
    assert!(shell.is_agent_run_active());
    assert!(!quit);
    release.send(true).unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut app.executable_extensions,
            &mut made_tool_call,
            &inspection,
            &mut deadline,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome, HostRunOutcome::Completed);
    drop(run);
    let context = format!("{:?}", app.agent.session().context().unwrap());
    assert!(!context.contains(command));
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_composer_extension_command_runs_during_response() {
    composer_command_during_response("/native-command", "native fixture ran").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_composer_extension_command_runs_during_response() {
    composer_command_during_response("/live-command", "Pi command ran during response").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_composer_extension_command_failure_keeps_response_alive() {
    composer_command_during_response(
        "/broken-command",
        "extension octet-pi-compat command /broken-command failed",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_composer_extension_command_timeout_keeps_response_alive() {
    composer_command_during_response(
        "/stalled-command",
        "extension octet-pi-compat command /stalled-command failed",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_composer_extension_command_failure_keeps_response_alive() {
    composer_command_during_response(
        "/native-command fail",
        "extension native-probe command /native-command failed",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_composer_extension_command_timeout_keeps_response_alive() {
    composer_command_during_response(
        "/native-command stall",
        "extension native-probe command /native-command failed",
    )
    .await;
}
