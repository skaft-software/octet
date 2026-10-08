//! Provider-request budgets for canonical, model-ideal coding paths.
//!
//! The real CLI and built-in tools run against a scripted loopback provider. The
//! script issues exactly the intended tool calls; any request Octet adds (startup
//! inventory, auxiliary work, re-reads, or forced verification turns) breaks the
//! per-task budget.

#![cfg(unix)]
#![allow(missing_docs)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL: &str = "custom/budget/probe";

#[derive(Clone, Copy)]
enum Scenario {
    KnownFile,
    SingleSite,
    MultiSite,
}

async fn fixture(scenario: Scenario) -> (tempfile::TempDir, MockServer) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("known.txt"), "before\nsecond before\n").unwrap();

    let server = MockServer::start().await;
    let turn = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |request: &Request| {
            let body: Value = request.body_json().unwrap();
            let index = turn.fetch_add(1, Ordering::Relaxed);
            let messages = body["messages"].as_array().unwrap();
            let last = messages.last().unwrap();
            let (delta, finish) = match (scenario, index) {
                (Scenario::KnownFile, 0) => (tool("read", json!({"path":"known.txt"})), "tool_calls"),
                (Scenario::SingleSite, 0) | (Scenario::MultiSite, 0) => {
                    (tool("read", json!({"path":"known.txt"})), "tool_calls")
                }
                (Scenario::SingleSite, 1) => (
                    tool("edit", json!({"path":"known.txt", "edits":[{"old":"before\nsecond before\n", "new":"after\nsecond before\n"}]})),
                    "tool_calls",
                ),
                (Scenario::MultiSite, 1) => (
                    tool("edit", json!({"path":"known.txt", "edits":[{"old":"before\nsecond before\n", "new":"after\nsecond after\n"}]})),
                    "tool_calls",
                ),
                (Scenario::SingleSite, 2) => (
                    tool("bash", json!({"command":"test \"$(sed -n '1p' known.txt)\" = after"})),
                    "tool_calls",
                ),
                (Scenario::MultiSite, 2) => (
                    tool("bash", json!({"command":"test \"$(sed -n '1p' known.txt)\" = after && test \"$(sed -n '2p' known.txt)\" = 'second after'"})),
                    "tool_calls",
                ),
                (_, _) if last["role"] == "tool" => (json!({"content":"Completed as requested."}), "stop"),
                (_, _) => panic!("unexpected scripted provider turn {index}: {last}"),
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"budget", "choices":[{"delta":delta}]}),
                    json!({"id":"budget", "choices":[{"delta":{},"finish_reason":finish}]})
                ))
        })
        .mount(&server)
        .await;

    let registry = json!({"version":1, "providers":{"budget":{
        "base_url":format!("{}/v1/", server.uri()), "auth":{"kind":"none"},
        "auto_discover":false, "models":[{"api_name":"probe", "context_window":32768,
        "max_output_tokens":4096, "tools":true, "parallel_tool_calls":false}]}}});
    let registry_path = home.join(".octet/credentials/custom.json");
    std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    (root, server)
}

fn tool(name: &str, arguments: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":format!("call-{name}"),"type":"function",
        "function":{"name":name,"arguments":arguments.to_string()}}]})
}

async fn run(root: &Path, task: &str) {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_octet"))
            .current_dir(root.join("workspace"))
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("OCTET_SESSION_DIR", root.join("sessions"))
            .env("TERM", "dumb")
            .args([
                "--offline",
                "--no-context-files",
                "--model",
                MODEL,
                "--print",
                task,
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("scripted request-budget task exceeded deadline")
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_budget(scenario: Scenario, task: &str, budget: usize) {
    let (root, server) = fixture(scenario).await;
    run(root.path(), task).await;
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|request| {
            request.method == "POST" && request.url.path() == "/v1/chat/completions"
        }),
        "unexpected startup or auxiliary provider request: {requests:?}"
    );
    assert_eq!(
        requests.len(),
        budget,
        "Octet exceeded the happy-path POST budget"
    );
    match scenario {
        Scenario::KnownFile => {}
        Scenario::SingleSite => assert_eq!(
            std::fs::read_to_string(root.path().join("workspace/known.txt")).unwrap(),
            "after\nsecond before\n"
        ),
        Scenario::MultiSite => assert_eq!(
            std::fs::read_to_string(root.path().join("workspace/known.txt")).unwrap(),
            "after\nsecond after\n"
        ),
    }
}

#[tokio::test]
async fn canonical_scripted_tasks_stay_within_provider_request_budgets() {
    assert_budget(
        Scenario::KnownFile,
        "Answer the question from known.txt.",
        2,
    )
    .await;
    assert_budget(
        Scenario::SingleSite,
        "Change before to after in known.txt and verify it.",
        4,
    )
    .await;
    assert_budget(
        Scenario::MultiSite,
        "Change both phrases in known.txt in one edit and verify.",
        4,
    )
    .await;
}
