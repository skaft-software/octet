//! The default coding tool surface against the real binary.
//!
//! Runs the real CLI against a loopback OpenAI-compatible server. The default
//! surface is exactly read/edit/write/bash; repository content search executes
//! through bash, without a separate search schema or prompt instruction.

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
                    "bash content search did not return the matching file: {}",
                    last["content"]
                );
                (json!({"content": MARKER}), "stop")
            } else {
                let id = sequence.fetch_add(1, Ordering::Relaxed);
                (
                    json!({"tool_calls": [{"index": 0, "id": format!("bash-{id}"),
                        "type": "function", "function": {"name": "bash",
                        "arguments": json!({"command": "rg --fixed-strings --line-number onboarding-tool-ok nested"}).to_string()}}]}),
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
async fn default_surface_has_only_four_core_tools_and_searches_through_bash() {
    let (root, server) = fixture().await;
    run_cli(root.path(), &[]).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "one bash call and one answer");
    let names = tool_names(&requests[0]);
    assert_eq!(names, ["read", "edit", "write", "bash"]);
    let system = requests[0].body_json::<Value>().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        system.contains("Configured core tools: read, edit, write, bash."),
        "the prompt must advertise exactly the tools the model is given: {system}"
    );
    assert!(system.contains("Use `bash` for shell commands and repository content search"));
    assert_eq!(system.matches("<tools>").count(), 1);
    assert_eq!(system.matches("</tools>").count(), 1);
    assert_eq!(system.matches("<rules>").count(), 1);
    assert_eq!(system.matches("</rules>").count(), 1);
    assert!(!system.contains("Tool preference:"));
    assert!(!system.contains("dedicated `search` tool"));
}

#[tokio::test]
async fn explicitly_requesting_removed_search_tool_fails_before_inference() {
    let (root, server) = fixture().await;
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(env!("CARGO_BIN_EXE_octet"))
            .current_dir(root.path().join("workspace"))
            .env_clear()
            .env("HOME", root.path().join("home"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("OCTET_SESSION_DIR", root.path().join("sessions"))
            .env("TERM", "dumb")
            .args([
                "--offline",
                "--no-context-files",
                "--model",
                MODEL,
                "--tools",
                "search",
                "--print",
                "Find the onboarding marker.",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("removed-tool rejection exceeded its deadline")
    .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("search"), "{error}");
    assert!(error.contains("unavailable"), "{error}");
    assert!(server.received_requests().await.unwrap().is_empty());
}
