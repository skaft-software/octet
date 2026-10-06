//! Real adapter and native input loop: no synthetic completion peer.
#![cfg(unix)]
use super::pi_contract_support::{pi_app, pi_ui_app};
use super::*;
use crossterm::event::KeyEvent;

const FACTORY: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => pi.registerCommand('cmd', {
  description: 'Completion acceptance',
  getArgumentCompletions: async prefix => {
    appendFileSync(TRACE, JSON.stringify({prefix}) + '\n');
    if (prefix === 'slow') await new Promise(resolve => setTimeout(resolve, 1500));
    if (prefix === 'none') return null;
    return [{value: 'alpha', label: 'CHOICE-ALPHA'}, {value: '文', label: 'CHOICE-UNICODE'}];
  },
  handler: () => {},
});
"#;

async fn input_for(
    app: &mut App,
    shell: &mut InteractiveShell,
    text: Option<&str>,
    duration: Duration,
) {
    let events = text
        .into_iter()
        .map(|text| Ok(Event::Paste(text.into())))
        .collect::<Vec<_>>();
    let mut input = futures_util::stream::iter(events).chain(futures_util::stream::pending());
    let mut scroll = tokio::time::interval(Duration::from_millis(16));
    let mut extensions = tokio::time::interval(Duration::from_millis(10));
    let (watcher, mut reload, mut tick) = support::test_reload();
    assert!(
        tokio::time::timeout(
            duration,
            wait_for_prompt(
                shell,
                &mut input,
                &mut scroll,
                &mut extensions,
                &mut app.executable_extensions,
                None,
                &mut tick,
                &watcher,
                &mut reload,
                None,
            )
        )
        .await
        .is_err(),
        "typing/completing must not execute the command"
    );
}

async fn frame(shell: &mut InteractiveShell) -> String {
    shell.render();
    sexy_tui_rs::strip_terminal_sequences(&shell.dump_rendered_frame().await.unwrap().join("\n"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_argument_completions_appear_on_typing_and_select_without_execution() {
    let (directory, mut app, mut shell) = pi_ui_app(FACTORY);
    shell.finish_startup();
    input_for(&mut app, &mut shell, Some("/cmd "), Duration::from_secs(2)).await;
    let rendered = frame(&mut shell).await;
    assert!(rendered.contains("CHOICE-ALPHA"), "{rendered}");
    assert!(rendered.contains("CHOICE-UNICODE"), "{rendered}");
    assert!(
        !rendered.contains("host refused autocomplete"),
        "{rendered}"
    );
    assert!(
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .contains(r#""prefix":"""#)
    );
    let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(
        shell.translate_input(Some(down), false),
        InputAction::Ignore
    ));
    assert_eq!(
        shell.pending(),
        "/cmd ",
        "menu navigation must not edit the draft"
    );
    let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(
        shell.translate_input(Some(tab), false),
        InputAction::CompletePath
    ));
    assert!(app
        .executable_extensions
        .accept_editor_autocomplete(&mut shell));
    assert_eq!(shell.pending(), "/cmd 文");
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_slow_completion_does_not_block_typing_or_replace_new_draft() {
    let (directory, mut app, mut shell) = pi_ui_app(FACTORY);
    shell.finish_startup();
    input_for(
        &mut app,
        &mut shell,
        Some("/cmd slow"),
        Duration::from_millis(300),
    )
    .await;
    assert!(
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .contains("slow")
    );
    // The callback is still sleeping; the sole native input owner must accept typing now.
    input_for(&mut app, &mut shell, Some("er"), Duration::from_millis(100)).await;
    assert_eq!(shell.pending(), "/cmd slower");
    input_for(&mut app, &mut shell, None, Duration::from_secs(2)).await;
    assert_eq!(shell.pending(), "/cmd slower");
    assert!(frame(&mut shell).await.contains("CHOICE-ALPHA"));
    shell.extension_set_editor("/cmd none".into());
    input_for(&mut app, &mut shell, None, Duration::from_millis(600)).await;
    assert_eq!(
        shell.pending(),
        "/cmd none",
        "unclaimed automatic queries must not insert paths"
    );
    assert!(!frame(&mut shell).await.contains("CHOICE-ALPHA"));
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_command_registration_is_not_a_foreground_ui_request() {
    let (_directory, mut app) = pi_app(FACTORY);
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        notices.extend(app.executable_extensions.drain_events());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !notices
            .join("\n")
            .contains("host refused autocomplete registration"),
        "{notices:?}"
    );
    app.executable_extensions.shutdown().await;
}
