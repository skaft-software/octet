//! Real App/fleet/Node acceptance for Pi's context facts and early raw-input phase.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app};
use super::*;

const SYSTEM_PROMPT_FACTORY: &str = r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.registerCommand('native-prompt', { handler: (_, ctx) => {
    let row;
    try {
      const text = ctx.getSystemPrompt();
      if (typeof text !== 'string') throw new Error('getter must be synchronous text');
      row = { kind: 'prompt', text, pid: process.pid };
    } catch (error) {
      row = { kind: 'refused', code: error.code, message: error.message, pid: process.pid };
    }
    appendFileSync(TRACE, JSON.stringify(row) + '\n');
  } });
};
"#;

fn system_prompt_rows(directory: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(directory.join("trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_system_prompt_reads_actual_agent_after_changed_and_empty_idle_setters() {
    let (directory, mut app) = pi_app(SYSTEM_PROMPT_FACTORY);
    let mut shell = InteractiveShell::test_shell();
    // command() drives the real resource startup barrier and then
    // run_interactive_extension_command, whose owner loop lends the live Agent.
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    let original = app.agent.system_prompt().to_owned();
    assert!(
        !original.is_empty(),
        "the base App prompt must be real text"
    );
    let rows = system_prompt_rows(directory.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "prompt");
    assert_eq!(rows[0]["text"].as_str(), Some(original.as_str()));
    let pid = rows[0]["pid"].clone();

    let changed = "Changed actual Agent prompt\nwith Unicode 文 and exact whitespace\t";
    app.agent.set_system_prompt(changed);
    assert_ne!(app.agent.system_prompt(), original);
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    let rows = system_prompt_rows(directory.path());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["kind"], "prompt");
    assert_eq!(rows[1]["text"].as_str(), Some(app.agent.system_prompt()));
    assert_eq!(rows[1]["pid"], pid, "same adapter, no restart/cache reset");
    assert_ne!(app.agent.system_prompt(), app.system);

    app.agent.set_system_prompt("");
    assert!(app.agent.system_prompt().is_empty());
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    let rows = system_prompt_rows(directory.path());
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2]["kind"], "prompt");
    assert_eq!(rows[2]["text"].as_str(), Some(app.agent.system_prompt()));
    assert_eq!(rows[2]["text"], "");
    assert_eq!(rows[2]["pid"], pid);
    assert!(app.agent.session().usage_records().is_empty());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_system_prompt_native_bounds_refuse_and_next_command_remains_healthy() {
    let (directory, mut app) = pi_app(SYSTEM_PROMPT_FACTORY);
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    for text in ["文".repeat(87382), "private\0prompt".to_owned()] {
        app.agent.set_system_prompt(text);
        command(&mut app, &mut shell, "native-prompt")
            .await
            .unwrap();
        let rows = system_prompt_rows(directory.path());
        let row = rows.last().unwrap();
        assert_eq!(row["kind"], "refused");
        assert_eq!(row["code"], -32602);
        let diagnostic = row["message"].as_str().unwrap();
        assert!(!diagnostic.contains("private") && !diagnostic.contains('文'));
        assert!(row.get("text").is_none());
    }
    app.agent.set_system_prompt("healthy actual Agent prompt");
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    let rows = system_prompt_rows(directory.path());
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3]["kind"], "prompt");
    assert_eq!(rows[3]["text"].as_str(), Some(app.agent.system_prompt()));
    assert!(app.agent.session().usage_records().is_empty());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_system_prompt_capability_withdrawal_rebuild_refuses_without_cached_disclosure() {
    let (directory, mut app) = pi_app(SYSTEM_PROMPT_FACTORY);
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    let admitted = app
        .executable_extensions
        .summaries()
        .into_iter()
        .find(|summary| summary.name == "octet-pi-compat")
        .unwrap();
    assert!(admitted
        .negotiated_features
        .iter()
        .any(|feature| feature == "system_prompt_read"));
    let manifest = admitted.manifest_path;
    let granted = std::fs::read_to_string(&manifest).unwrap();
    assert!(granted.contains("system_prompt = true"));
    // Only this generated fixture manifest changes. Rebuild uses the actual
    // native discovery/capability offer; no JS or host-state grant is injected.
    std::fs::write(
        &manifest,
        granted.replace("system_prompt = true", "system_prompt = false"),
    )
    .unwrap();
    app = rebuild_app(app, None, None, None, None).unwrap();
    let refused = app
        .executable_extensions
        .summaries()
        .into_iter()
        .find(|summary| summary.name == "octet-pi-compat")
        .unwrap();
    assert!(refused.running);
    assert!(!refused
        .negotiated_features
        .iter()
        .any(|feature| feature == "system_prompt_read"));
    command(&mut app, &mut shell, "native-prompt")
        .await
        .unwrap();
    let rows = system_prompt_rows(directory.path());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["kind"], "prompt");
    assert_eq!(rows[1]["kind"], "refused");
    assert_eq!(rows[1]["code"], -32601);
    assert!(rows[1]["message"]
        .as_str()
        .unwrap()
        .contains("system_prompt_read"));
    assert!(rows[1].get("text").is_none());
    assert!(app.agent.session().usage_records().is_empty());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_input_transform_is_ordered_and_before_agent_start_is_separate() {
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.on('input', event => {
    trace({ type: event.type, text: event.text, source: event.source });
    return { action: 'transform', text: 'first', images: [] };
  });
  pi.on('input', event => {
    trace({ type: event.type, text: event.text, images: event.images });
    return { action: 'transform', text: 'second' };
  });
  pi.on('before_agent_start', event => { trace({ type: event.type, prompt: event.prompt }); });
};
"#,
    );
    let before = app.agent.session().entries().len();
    let input = app
        .executable_extensions
        .process_input("original".into(), None, "interactive", None)
        .await
        .unwrap()
        .expect("transformed input is not handled");
    assert_eq!(input.text, "second");
    assert!(input.images.unwrap().is_empty());
    assert_eq!(
        app.agent.session().entries().len(),
        before,
        "input hooks must precede prompt persistence"
    );
    let early = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    let early: Vec<serde_json::Value> = early
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(early.len(), 2);
    assert_eq!(early[0]["source"], "interactive");
    assert_eq!(early[1]["text"], "first");
    assert_eq!(early[1]["images"], serde_json::json!([]));
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, input.text)
        .await
        .unwrap();
    assert!(composition.prompt.contains("second"));
    let final_trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
    let final_trace: Vec<serde_json::Value> = final_trace
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        final_trace.len(),
        3,
        "composition must not invoke raw input a second time"
    );
    assert_eq!(final_trace[2]["type"], "before_agent_start");
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_handled_input_stops_later_handlers_without_persistence() {
    let (directory, mut app) = pi_app(
        r#"
import { appendFileSync } from 'node:fs';
export default pi => {
  pi.on('input', event => { appendFileSync(TRACE, 'handled\n'); return { action: 'handled' }; });
  pi.on('input', () => { appendFileSync(TRACE, 'unexpected\n'); });
};
"#,
    );
    let before = app.agent.session().entries().len();
    assert!(app
        .executable_extensions
        .process_input("consume locally".into(), None, "interactive", None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(app.agent.session().entries().len(), before);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
        "handled\n"
    );
    app.executable_extensions.shutdown().await;
}
