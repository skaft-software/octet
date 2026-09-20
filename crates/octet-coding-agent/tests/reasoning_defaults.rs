#![cfg(unix)]
#![allow(missing_docs)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL: &str = "custom/onboarding/probe";
const MARKER: &str = "onboarding-tool-ok";

async fn fixture(metadata: Value) -> (tempfile::TempDir, MockServer) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("probe.txt"), MARKER).unwrap();
    let server = MockServer::start().await;
    let sequence = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |request: &Request| {
            let body: Value = request.body_json().unwrap();
            let last = body["messages"].as_array().unwrap().last().unwrap();
            let (delta, finish) = if last["role"] == "tool" {
                assert!(last["content"].as_str().unwrap().contains(MARKER));
                (json!({"content": MARKER}), "stop")
            } else {
                let id = sequence.fetch_add(1, Ordering::Relaxed);
                (
                    json!({"tool_calls": [{"index": 0, "id": format!("read-{id}"),
                        "type": "function", "function": {"name": "read",
                        "arguments": "{\"path\":\"probe.txt\"}"}}]}),
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
    let mut model = json!({"api_name":"probe", "context_window":32768,
        "max_output_tokens":4096, "tools":true, "parallel_tool_calls":false});
    model
        .as_object_mut()
        .unwrap()
        .extend(metadata.as_object().unwrap().clone());
    let registry = json!({"version":1, "providers":{"onboarding":{
        "base_url":format!("{}/v1/", server.uri()), "auth":{"kind":"none"},
        "auto_discover":false, "models":[model]}}});
    let registry_path = home.join(".octet/credentials/custom.json");
    std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    (root, server)
}

async fn run_cli(root: &Path, args: &[&str], reasoning_env: Option<&str>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
    command
        .current_dir(root.join("workspace"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("OCTET_SESSION_DIR", root.join("sessions"))
        .env("TERM", "dumb")
        .args([
            "--offline",
            "--safe-mode",
            "--no-context-files",
            "--tools",
            "read",
            "--model",
            MODEL,
            "--print",
            "Read probe.txt and quote its contents.",
        ])
        .args(args)
        .kill_on_drop(true);
    if let Some(reasoning) = reasoning_env {
        command.env("OCTET_REASONING", reasoning);
    }
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .expect("loopback onboarding task exceeded its deadline")
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(MARKER));
}

fn persisted_reasoning(root: &Path) -> String {
    let mut files = Vec::new();
    for workspace in std::fs::read_dir(root.join("sessions")).unwrap() {
        let workspace = workspace.unwrap().path();
        if workspace.is_dir() {
            for file in std::fs::read_dir(workspace).unwrap() {
                let path = file.unwrap().path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                {
                    files.push(path);
                }
            }
        }
    }
    assert_eq!(files.len(), 1, "resume must not create another session");
    std::fs::read_to_string(&files[0])
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .rfind(|record| record["value"]["type"] == "config")
        .unwrap()["value"]["reasoning"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_wire(requests: &[Request], effort: Option<&str>) {
    assert_eq!(
        requests.len(),
        2,
        "one read call and one answer, no retries"
    );
    for request in requests {
        assert!(request.headers.get("authorization").is_none());
        let body: Value = request.body_json().unwrap();
        assert_eq!(body["model"], "probe");
        assert_eq!(body.get("reasoning_effort").and_then(Value::as_str), effort);
        for field in [
            "enable_thinking",
            "thinking",
            "reasoning",
            "chat_template_kwargs",
        ] {
            assert!(body.get(field).is_none(), "unexpected control: {field}");
        }
    }
    let continuation: Value = requests[1].body_json().unwrap();
    let last = continuation["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "tool");
    assert!(last["content"].as_str().unwrap().contains(MARKER));
}

#[tokio::test]
async fn model_defaults_reach_tool_requests_without_guessing_unknown_controls() {
    for (metadata, selection, wire) in [
        (
            json!({"reasoning":true, "reasoning_values":["none","default"],
            "reasoning_default":"default"}),
            "on",
            None,
        ),
        (
            json!({"reasoning":true, "reasoning_values":["none","low","high"],
            "reasoning_default":"high"}),
            "high",
            Some("high"),
        ),
        (
            json!({"reasoning":true, "reasoning_values":["none","low","high"],
            "reasoning_default":"none"}),
            "off",
            Some("none"),
        ),
        (
            json!({"reasoning":true, "reasoning_configurable":false}),
            "on",
            None,
        ),
        (json!({}), "off", None),
        (json!({"reasoning":false}), "off", None),
    ] {
        let (root, server) = fixture(metadata).await;
        run_cli(root.path(), &[], None).await;
        assert_eq!(persisted_reasoning(root.path()), selection);
        assert_wire(&server.received_requests().await.unwrap(), wire);
    }
}

#[tokio::test]
async fn explicit_off_and_resumed_choices_override_model_and_process_defaults() {
    let (root, server) = fixture(json!({"reasoning":true,
        "reasoning_values":["none","default"], "reasoning_default":"default"}))
    .await;
    run_cli(root.path(), &[], None).await;
    assert_eq!(persisted_reasoning(root.path()), "on");
    assert_wire(&server.received_requests().await.unwrap(), None);

    // A new process default must not replace the session's effective choice.
    run_cli(root.path(), &["--continue"], Some("off")).await;
    assert_eq!(persisted_reasoning(root.path()), "on");
    assert_wire(&server.received_requests().await.unwrap()[2..], None);

    // A deliberate CLI Off wins even over environment and resumed On.
    run_cli(
        root.path(),
        &["--continue", "--reasoning", "off"],
        Some("on"),
    )
    .await;
    assert_eq!(persisted_reasoning(root.path()), "off");
    assert_wire(
        &server.received_requests().await.unwrap()[4..],
        Some("none"),
    );

    // Restart without a CLI override: saved Off is not treated as unset.
    run_cli(root.path(), &["--continue"], Some("on")).await;
    assert_eq!(persisted_reasoning(root.path()), "off");
    assert_wire(
        &server.received_requests().await.unwrap()[6..],
        Some("none"),
    );
}

#[tokio::test]
async fn explicit_process_preferences_are_not_replaced_by_model_defaults() {
    for source in ["global", "project", "environment", "cli"] {
        let (root, server) = fixture(json!({"reasoning":true,
            "reasoning_values":["none","default"], "reasoning_default":"default"}))
        .await;
        let mut args = Vec::new();
        match source {
            "global" => std::fs::write(
                root.path().join("home/.octet/config.toml"),
                "reasoning = 'off'\n",
            )
            .unwrap(),
            "project" => {
                let config_dir = root.path().join("workspace/.octet");
                std::fs::create_dir_all(&config_dir).unwrap();
                std::fs::write(config_dir.join("config.toml"), "reasoning = 'off'\n").unwrap();
                args.push("--workspace-trusted");
            }
            "cli" => args.extend(["--reasoning", "off"]),
            _ => {}
        }
        run_cli(
            root.path(),
            &args,
            (source == "environment").then_some("off"),
        )
        .await;
        assert_eq!(persisted_reasoning(root.path()), "off", "{source}");
        assert_wire(&server.received_requests().await.unwrap(), Some("none"));
    }
}
