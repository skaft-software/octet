//! Real Rust shell + reviewed adapter acceptance for Pi dialogs and editor API.
#![cfg(unix)]
use super::*;
use super::pi_contract_support::{command, pi_app};

async fn pump_until(app: &mut App, shell: &mut InteractiveShell, mut ready: impl FnMut(&InteractiveShell) -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            for message in app.executable_extensions.drain_events_for_shell(shell) {
                shell.notice(message);
            }
            if ready(shell) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("real adapter UI did not reach its acceptance barrier");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_dialog_options_render_countdown_and_restore_on_timeout() {
    let (directory, mut app) = pi_app(r#"
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
"#);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(37, 12);
    command(&mut app, &mut shell, "probe").await.unwrap();
    pump_until(&mut app, &mut shell, |shell| shell.debug_snapshot().contains("NATIVE-PI-INPUT")).await;
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("(1s)"), "{snapshot}");
    assert!(!snapshot.contains("not-a-prefill"), "Pi accepts but does not display placeholder");
    let trace = directory.path().join("trace.jsonl");
    pump_until(&mut app, &mut shell, |_| trace.exists()).await;
    assert_eq!(std::fs::read_to_string(trace).unwrap().trim(), r#"{"cancelled":true}"#);
    pump_until(&mut app, &mut shell, |shell| !shell.debug_snapshot().contains("NATIVE-PI-INPUT")).await;
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_editor_factory_identity_and_composer_checkpoints() {
    let (_directory, mut app) = pi_app(r#"
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
"#);
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "mount").await.unwrap();
    command(&mut app, &mut shell, "edit").await.unwrap();
    assert_eq!(shell.extension_editor_snapshot().text, "native-draft!");
    assert!(shell.debug_snapshot().contains("native-draft!"));
    command(&mut app, &mut shell, "clear").await.unwrap();
    assert_eq!(shell.extension_editor_snapshot().text, "native-draft!");
    assert!(!shell.debug_snapshot().contains("Ctrl+G restore editor"));
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pi_chrome_controls_existing_loader_title_and_tool_disclosure() {
    let (directory, mut app) = pi_app(r#"
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
"#);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    let run = shell.begin_run("offline-test");
    command(&mut app, &mut shell, "chrome").await.unwrap();
    assert_eq!(shell.extension_window_title(), "NATIVE-PI-TITLE");
    let visible = shell.debug_snapshot();
    assert_eq!(visible.matches("NATIVE-PI-WORKING").count(), 1, "loader was duplicated: {visible}");
    assert!(!shell.verbose_tools());
    command(&mut app, &mut shell, "hide").await.unwrap();
    assert!(!shell.debug_snapshot().contains("NATIVE-PI-WORKING"));
    shell.set_verbose_tools(true); // Native Ctrl+O must remain authoritative.
    command(&mut app, &mut shell, "read").await.unwrap();
    assert_eq!(std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap().trim(), r#"{"expanded":true}"#);
    shell.set_verbose_tools(false);
    command(&mut app, &mut shell, "reset").await.unwrap();
    let reset = shell.debug_snapshot();
    assert!(!reset.contains("NATIVE-PI-WORKING") && !reset.contains("NATIVE-PI-HIDDEN"), "{reset}");
    assert!(reset.contains("Working"), "native loader default was not restored: {reset}");
    shell.interrupt_run(run);
    app.executable_extensions.shutdown().await;
}
