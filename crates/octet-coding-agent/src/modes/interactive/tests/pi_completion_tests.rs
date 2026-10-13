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
    input_events_for(app, shell, events, duration).await;
}

async fn input_events_for(
    app: &mut App,
    shell: &mut InteractiveShell,
    events: Vec<std::io::Result<Event>>,
    duration: Duration,
) {
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
    // A settled query is not restarted by idle frames: the callback runs once
    // per draft, so latency stays bounded and typing is never queued behind it.
    let trace = directory.path().join("trace.jsonl");
    let settled = std::fs::read_to_string(&trace).unwrap().lines().count();
    input_for(&mut app, &mut shell, None, Duration::from_millis(700)).await;
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap().lines().count(),
        settled,
        "idle frames must not restart a settled completion query"
    );
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

fn native_composer_factory(seed: &str) -> String {
    let factory = format!(
        "{FACTORY}\n{}",
        r#"
import { CustomEditor } from '@earendil-works/pi-coding-agent';
"#
    );
    // Keep command and editor registration in the same real factory.
    let factory = factory.replace(
        "export default pi => pi.registerCommand",
        "const register = pi => pi.registerCommand",
    ) + r#"
export default pi => {
  register(pi);
  pi.on('session_start', (_, ctx) => ctx.ui.setEditorComponent((tui, theme, keys) => {
    const editor = new CustomEditor(tui, theme, keys);
    const setText = editor.setText.bind(editor);
    let seeded = false;
    editor.setText = text => {
      setText(text);
      if (!seeded) {
        seeded = true;
        setTimeout(() => { setText('/cmd α尾'); editor.handleInput('\x1b[D'); }, 30);
      }
    };
    const render = editor.render.bind(editor);
    editor.render = width => [...render(width), 'NATIVE-REGISTRY-COMPOSER'];
    editor.onChange = () => appendFileSync(TRACE, JSON.stringify({draft:editor.getText(),cursor:editor.getCursor()}) + '\n');
    return editor;
  }));
};
"#;
    factory.replace("'/cmd α尾'", &serde_json::to_string(seed).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_registry_completion_in_custom_editor_uses_actual_caret_and_one_host_menu() {
    let factory = native_composer_factory("/cmd α尾");
    let (directory, mut app, mut shell) = pi_ui_app(&factory);
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .unwrap()
    .unwrap();
    shell.finish_startup();
    input_for(&mut app, &mut shell, None, Duration::from_millis(800)).await;
    let snapshot = shell.extension_editor_snapshot();
    assert_eq!(snapshot.text, "/cmd α尾");
    assert_eq!(
        snapshot.cursor,
        "/cmd α".len(),
        "registry query must use the native component caret, not the end of its draft"
    );
    assert!(
        shell.extension_autocomplete_displayed(),
        "the one native registry menu, not a component-local command popup, must own selection"
    );
    let rendered = frame(&mut shell).await;
    assert_eq!(rendered.matches("CHOICE-ALPHA").count(), 1, "{rendered}");
    assert_eq!(
        rendered.matches("NATIVE-REGISTRY-COMPOSER").count(),
        1,
        "{rendered}"
    );
    input_events_for(
        &mut app,
        &mut shell,
        vec![
            Ok(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))),
            Ok(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))),
        ],
        Duration::from_millis(300),
    )
    .await;
    assert_eq!(shell.pending(), "/cmd 文尾");
    let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    assert!(trace.contains(r#""prefix":"α""#), "{trace}");
    assert!(
        trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .any(|row| row["draft"] == "/cmd 文尾"
                && row["cursor"] == serde_json::json!({"line":0,"col":6})),
        "host acceptance must commit the native editor text and caret: {trace}"
    );
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_registry_completion_cannot_survive_native_clear_or_mount_retirement() {
    for retire in [false, true] {
        let factory = native_composer_factory("/cmd slow尾").replace("if (prefix === 'none')", "appendFileSync(TRACE, JSON.stringify({settled: prefix}) + '\\n'); if (prefix === 'none')");
        let (directory, mut app, mut shell) = pi_ui_app(&factory);
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        tokio::time::timeout(
            Duration::from_secs(8),
            resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
        )
        .await
        .unwrap()
        .unwrap();
        shell.finish_startup();
        input_for(&mut app, &mut shell, None, Duration::from_millis(350)).await;
        let trace_path = directory.path().join("trace.jsonl");
        assert!(
            std::fs::read_to_string(&trace_path)
                .unwrap()
                .contains(r#""prefix":"slow""#),
            "the old query must actually be in flight"
        );
        if retire {
            app.executable_extensions
                .revoke_terminal_grant_for_shell(&mut shell, "retire native editor query");
        } else {
            input_events_for(
                &mut app,
                &mut shell,
                vec![Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )))],
                Duration::from_millis(100),
            )
            .await;
            assert_eq!(shell.pending(), "");
        }
        let saved = shell.extension_editor_snapshot();
        input_for(&mut app, &mut shell, None, Duration::from_secs(2)).await;
        assert!(
            std::fs::read_to_string(trace_path)
                .unwrap()
                .contains(r#""settled":"slow""#),
            "retirement must be checked even after the uncancellable Pi callback settles"
        );
        assert_eq!(shell.extension_editor_snapshot(), saved);
        assert!(!shell.extension_autocomplete_displayed());
        assert!(!app
            .executable_extensions
            .accept_editor_autocomplete(&mut shell));
        assert!(!frame(&mut shell).await.contains("CHOICE-ALPHA"));
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_registry_completion_refusal_preserves_native_draft_and_recovery_caret() {
    let factory = native_composer_factory("/cmd α尾")
        .replace(
            "return [{value: 'alpha', label: 'CHOICE-ALPHA'}, {value: '文', label: 'CHOICE-UNICODE'}];",
            "return [{value: '[paste #1 140000 chars]'.repeat(2), label: 'CHOICE-EXPANSION'}];",
        )
        .replace(
            "setTimeout(() => { setText(",
            "setTimeout(() => { editor.handleInput('\\x1b[200~' + 'x'.repeat(140000) + '\\x1b[201~'); setText(",
        );
    let (_directory, mut app, mut shell) = pi_ui_app(&factory);
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .unwrap()
    .unwrap();
    shell.finish_startup();
    input_for(&mut app, &mut shell, None, Duration::from_millis(800)).await;
    assert!(frame(&mut shell).await.contains("CHOICE-EXPANSION"));
    let before = shell.extension_editor_snapshot();
    assert_eq!(before.text, "/cmd α尾");
    input_events_for(
        &mut app,
        &mut shell,
        vec![Ok(Event::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )))],
        Duration::from_millis(200),
    )
    .await;
    let after = shell.extension_editor_snapshot();
    assert_eq!(
        after.text, before.text,
        "refused native expansion must not become the admission/recovery draft"
    );
    assert_eq!(after.cursor, before.cursor);
    input_for(&mut app, &mut shell, Some("β"), Duration::from_millis(200)).await;
    assert_eq!(
        shell.pending(),
        "/cmd αβ尾",
        "the next checkpoint must retain the native caret and its live recovery revision"
    );
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
