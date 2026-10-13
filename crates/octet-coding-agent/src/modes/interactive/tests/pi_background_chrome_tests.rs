//! U27: a Pi extension's background chrome must reach the live foreground
//! session instead of being refused as if the host were headless.
//!
//! The take's reviewed five-extension bridge refreshes its footer from an async
//! continuation of `session_start`. While the interactive frontend composes the
//! next prompt, the hook pump is the narrow `drain_events()` path, and that path
//! refused the request with `invalid_request: no foreground session is available
//! in this host mode` even though the frontend owns the session and its shell.
//! Pi 1.0.2 resolves `ctx.ui.*` against the live session context at any time
//! (`dist/core/extensions/runner.js` `createContext`, gated only by
//! `assertActive()`), so the octet host must queue the request for the shell it
//! already has. This drives the real adapter and the real host, not a helper.
#![cfg(unix)]

use super::pi_contract_support::pi_ui_app;
use super::*;

/// A Pi factory whose settled `session_start` hook refreshes a footer from the
/// background, exactly like pi-powerline-footer's status refresh.
const BACKGROUND_CHROME_FACTORY: &str = r#"
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
export default pi => {
  pi.on('session_start', (_event, ctx) => {
    trace({hook: 'session_start'});
    setTimeout(() => {
      trace({background: 'issued'});
      ctx.ui.setFooter(() => ({render: () => ['U27-BACKGROUND-FOOTER'], invalidate() {}}))
        .then(() => trace({background: 'applied'}))
        .catch(error => trace({background: 'error', message: String((error && error.message) || error)}));
    }, 25);
  });
};
"#;

fn trace_rows(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_background_chrome_from_a_settled_hook_reaches_the_live_shell() {
    let (directory, mut app, mut shell) = pi_ui_app(BACKGROUND_CHROME_FACTORY);
    let trace_path = directory.path().join("trace.jsonl");
    let mut notices = Vec::new();
    // The interactive frontend composes a prompt here, so the only pump is the
    // narrow hook-wait drain. A request that needs the foreground session must
    // queue for the shell instead of receiving a headless refusal.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            notices.extend(app.executable_extensions.drain_events());
            if !trace_rows(&trace_path).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real adapter never ran the Pi session_start hook");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            notices.extend(app.executable_extensions.drain_events());
            if trace_rows(&trace_path)
                .iter()
                .any(|row| row["background"] == "issued")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the background chrome request was never issued");
    // Bounded reproduction window: the settled hook's continuation issues
    // `ui/chrome` while only the headless-policy drain runs.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            notices.extend(app.executable_extensions.drain_events());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok();
    assert!(
        !notices
            .iter()
            .any(|notice| notice.contains("no foreground session")),
        "the live interactive session must not be refused as headless: {notices:?}"
    );
    let rows = trace_rows(&trace_path);
    assert!(
        !rows.iter().any(|row| row["background"] == "error"),
        "background chrome must not fail against the live frontend: {rows:?}"
    );
    // The queued request is served by the next real shell pump.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for message in app.executable_extensions.drain_events_for_shell(&mut shell) {
                notices.push(message);
            }
            if trace_rows(&trace_path)
                .iter()
                .any(|row| row["background"] == "applied")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued background chrome never reached the live shell");
    let rows = trace_rows(&trace_path);
    assert!(
        rows.iter().any(|row| row["background"] == "applied"),
        "{rows:?}"
    );
}
