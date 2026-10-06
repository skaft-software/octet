//! The default coding tool surface against the real binary.
//!
//! The product prompt advertises a dedicated `search` tool
//! (`crates/octet-coding-agent/src/resources.rs`, `TOOL_PREFERENCE`), but the
//! default allowlist used to omit it, so every discovery step had to go through
//! `bash`. This suite runs the real CLI against a loopback OpenAI-compatible
//! server and asserts that the tool the prompt advertises is the tool the model
//! is offered: the request schema must carry `search`, and a `search` call must
//! execute and return the matching file.

#![cfg(unix)]
#![allow(missing_docs)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL: &str = "custom/onboarding/probe";
const MARKER: &str = "onboarding-tool-ok";

/// One workspace file that only a content search (or a shelled-out `rg`) finds
/// without listing the directory.
async fn fixture() -> (tempfile::TempDir, MockServer) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(workspace.join("nested")).unwrap();
    std::fs::write(workspace.join("README.md"), "unrelated\n").unwrap();
    std::fs::write(workspace.join("nested/needle.txt"), format!("{MARKER}\n")).unwrap();
    let server = MockServer::start().await;
    let sequence = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |request: &Request| {
            let body: Value = request.body_json().unwrap();
            let last = body["messages"].as_array().unwrap().last().unwrap();
            let (delta, finish) = if last["role"] == "tool" {
                assert!(
                    last["content"].as_str().unwrap().contains(MARKER),
                    "read-only search did not return the matching file: {}",
                    last["content"]
                );
                (json!({"content": MARKER}), "stop")
            } else {
                let id = sequence.fetch_add(1, Ordering::Relaxed);
                (
                    json!({"tool_calls": [{"index": 0, "id": format!("search-{id}"),
                        "type": "function", "function": {"name": "search",
                        "arguments": "{\"query\":\"onboarding-tool-ok\"}"}}]}),
                    "tool_calls",
                )
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"fixture", "choices":[{"delta":delta}]}),
                    json!({"id":"fixture", "choices":[{"delta":{},"finish_reason":finish}]})
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

async fn run_cli(root: &Path, args: &[&str]) {
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
                "Find the onboarding marker.",
            ])
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("loopback tool-surface task exceeded its deadline")
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(MARKER));
}

fn tool_names(request: &Request) -> Vec<String> {
    request.body_json::<Value>().unwrap()["tools"]
        .as_array()
        .expect("request must advertise tools")
        .iter()
        .map(|tool| {
            tool["function"]["name"]
                .as_str()
                .or_else(|| tool["custom"]["name"].as_str())
                .expect("every advertised tool is named")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn default_surface_advertises_and_executes_the_search_tool_the_prompt_names() {
    let (root, server) = fixture().await;
    run_cli(root.path(), &[]).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "one search call and one answer");
    let names = tool_names(&requests[0]);
    assert!(
        names.iter().any(|name| name == "search"),
        "the default allowlist omits the tool the prompt advertises: {names:?}"
    );
    let system = requests[0].body_json::<Value>().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        system.contains("Configured core tools: read, edit, write, bash, search."),
        "the prompt must advertise exactly the tools the model is given: {system}"
    );
}
