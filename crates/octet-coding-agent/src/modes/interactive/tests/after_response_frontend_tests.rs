//! The post-turn hook must retain the same native reverse-request consumer.
#![cfg(unix)]
use super::pi_contract_support::pi_ui_app;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_custom_keybindings_survive_host_state_refresh() {
    let (directory, mut app, mut shell) = pi_ui_app(
        r#"
import { writeFileSync } from 'node:fs';
export default pi => pi.registerCommand('native-keys', { handler: async (_, ctx) => {
  await ctx.ui.custom((tui, theme, keys, done) => {
    writeFileSync(TRACE, JSON.stringify(keys.getKeys('app.tools.expand')));
    return { render: () => ['Native keybinding dialog'], invalidate() {}, handleInput(data) {
      keys.matches(data, 'app.tools.expand');
      writeFileSync(TRACE, JSON.stringify(keys.getKeys('app.tools.expand')));
      if (data === 'y') done('yes');
    } };
  });
}});
"#,
    );
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    super::pi_contract_support::command(&mut app, &mut shell, "native-keys")
        .await
        .unwrap();
    let keys: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
    )
    .unwrap();
    assert_eq!(keys, shell.extension_keybindings()["app.tools.expand"]);
    std::fs::write(
        directory.path().join("keybindings.json"),
        r#"{"app.tools.expand":"ctrl+x"}"#,
    )
    .unwrap();
    shell.test_set_keybindings(crate::tui::keymap::keybindings::KeybindingsManager::create(
        directory.path(),
        "linux",
        false,
    ));
    app.executable_extensions
        .refresh_frontend_keybindings(&shell);
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let key = Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::NONE,
    ));
    assert!(app
        .executable_extensions
        .route_remote_ui_event(&mut shell, &key, false));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            apply_extension_background(&mut shell, &mut app.executable_extensions);
            let latest: Vec<String> = serde_json::from_str(
                &std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
            )
            .unwrap();
            if latest == ["ctrl+x"] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retained component must see native keybinding reload");
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_response_services_native_pi_chrome_before_the_hook_deadline() {
    let (directory, mut app, mut shell) = pi_ui_app(
        r#"
import { writeFileSync } from 'node:fs';
export default pi => pi.on('after_response', (_, ctx) => {
  ctx.ui.setWorkingMessage('Post-turn native receipt');
  writeFileSync(TRACE, 'host-acknowledged');
});
"#,
    );
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .expect("native startup timed out")
    .unwrap();
    shell.finish_startup();
    shell.extension_set_editor("kept draft".into());
    let notices = after_response_with_frontend(
        &mut app.executable_extensions,
        &mut shell,
        &mut input,
        "assistant finished",
    )
    .await
    .expect("post-turn hook cancelled");
    assert!(notices.is_empty(), "post-turn hook failed: {notices:?}");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
        "host-acknowledged"
    );
    assert_eq!(shell.extension_editor_snapshot().text, "kept draft");
    app.executable_extensions.shutdown().await;
}
