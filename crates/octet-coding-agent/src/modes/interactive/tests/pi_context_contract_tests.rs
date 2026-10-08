//! Real App/fleet/Node acceptance for Pi's early raw-input phase.
#![cfg(unix)]
use super::pi_contract_support::pi_app;

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
