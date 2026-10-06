//! The default coding prompt's verification contract, against the real binary.
//!
//! The measured cost of octet's old default was ~1.17 self-check calls per task
//! (git status/diff plus ad-hoc `/tmp` harnesses) against 0.14 for an agent whose
//! prompt says to stop after the change. This suite runs the real CLI against a
//! loopback provider and reads the system message the model actually receives:
//! one requested change, one relevant check, then stop.

#![cfg(unix)]
#![allow(missing_docs)]

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL: &str = "custom/onboarding/probe";
const ANSWER: &str = "changed one file";

async fn fixture() -> (tempfile::TempDir, MockServer) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("probe.txt"), "before\n").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |_request: &Request| {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"fixture", "choices":[{"delta":{"content": ANSWER}}]}),
                    json!({"id":"fixture", "choices":[{"delta":{},"finish_reason":"stop"}]})
                ))
        })
        .mount(&server)
        .await;
    let registry = json!({"version":1, "providers":{"onboarding":{
        "base_url":format!("{}/v1/", server.uri()), "auth":{"kind":"none"},
        "auto_discover":false, "models":[{"api_name":"probe", "context_window":32768,
        "max_output_tokens":4096, "tools":true, "parallel_tool_calls":false}]}}});
    let registry_path = home.join(".octet/credentials/custom.json");
    std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    (root, server)
}

async fn system_message(root: &Path, server: &MockServer) -> String {
    let output = tokio::time::timeout(
        Duration::from_secs(20),
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
                "Rename the marker in probe.txt.",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("loopback prompt task exceeded its deadline")
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(ANSWER));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "one answer, no extra turns");
    requests[0].body_json::<Value>().unwrap()["messages"][0]["content"]
        .as_str()
        .expect("the default prompt is the first system message")
        .to_owned()
}

#[tokio::test]
async fn default_prompt_asks_for_one_check_then_stops() {
    let (root, server) = fixture().await;
    let system = system_message(root.path(), &server).await;
    assert!(
        system.contains("After the change and its check, stop"),
        "the default prompt must stop after one check: {system}"
    );
    assert!(
        !system.contains("Inspect the resulting diff and run the relevant tests"),
        "the default prompt must not ask for the extra diff inspection: {system}"
    );
    assert!(
        system.contains("no extra harnesses, no `git diff`"),
        "the stop rule must name the extra work it forbids: {system}"
    );
    assert!(
        system.contains("Never claim an unrun check passed"),
        "dropping the extra verification must not drop honest reporting: {system}"
    );
}
