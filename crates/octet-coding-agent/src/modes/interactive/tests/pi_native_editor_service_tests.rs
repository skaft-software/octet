//! Phase-2 acceptance: actual native frontend and actual adapter exports.
#![cfg(unix)]

use super::pi_contract_support::pi_ui_app;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_editor_service_owns_unicode_cursor_undo_and_submit_callbacks() {
    let factory = r#"
import { CustomEditor } from '@earendil-works/pi-coding-agent';
import { Editor } from '@earendil-works/pi-tui';
import assert from 'node:assert/strict';
import { appendFileSync } from 'node:fs';
export default pi => pi.on('session_start', (_, ctx) => {
  ctx.ui.setEditorComponent((tui, theme, keys) => {
    const editor = new CustomEditor(tui, theme, keys);
    assert.equal(Object.getPrototypeOf(CustomEditor.prototype), Editor.prototype);
    assert.equal(Object.hasOwn(editor, 'state'), false, 'Editor must not own a JavaScript editing model');
    const changes = [], submissions = [];
    editor.onChange = text => changes.push(text);
    editor.onSubmit = text => submissions.push(text);
    editor.setText('α👩‍💻z');
    editor.handleInput('\x1b[D');
    assert.deepEqual(editor.getCursor(), {line: 0, col: 6});
    editor.insertTextAtCursor('β');
    assert.equal(editor.getText(), 'α👩‍💻βz');
    editor.handleInput('\x1b[45;5u');
    assert.equal(editor.getText(), 'α👩‍💻z');
    editor.disableSubmit = true;
    editor.handleInput('\r');
    assert.equal(editor.getText(), 'α👩‍💻z');
    assert.deepEqual(submissions, []);
    editor.disableSubmit = false;
    editor.handleInput('\r');
    assert.deepEqual(submissions, ['α👩‍💻z']);
    assert.equal(editor.getText(), '');
    assert.ok(changes.includes('α👩‍💻βz'));
    appendFileSync(TRACE, JSON.stringify({native: true, changes, submissions}) + '\n');
    const render = editor.render.bind(editor);
    editor.render = width => [...render(width), 'NATIVE-EDITOR-SERVICE'];
    return editor;
  });
});
"#;
    let (directory, mut app, mut shell) = pi_ui_app(factory);
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .expect("native service startup deadline")
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            app.executable_extensions.drain_events_for_shell(&mut shell);
            if directory.path().join("trace.jsonl").is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "native service assertions must execute through the actual adapter: {:?}",
            shell.debug_error()
        )
    });
    let trace: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(directory.path().join("trace.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(trace["native"], true);
    assert_eq!(trace["submissions"], serde_json::json!(["α👩‍💻z"]));
    shell.finish_startup();
    shell.render();
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(frame.contains("NATIVE-EDITOR-SERVICE"), "{frame}");
    app.executable_extensions.shutdown().await;
}
