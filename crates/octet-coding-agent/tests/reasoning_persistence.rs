//! The user's saved model and thinking level outlive the process.
//!
//! The provider-declared default (change 3 of the do-less set) is only the
//! fallback for a user who has never chosen. Everything the user picked — this
//! run's `--model`/`--reasoning` flag, and the model plus level recorded in the
//! session and in `~/.octet/config.toml` by `/model`, `/thinking` and
//! `/settings default model|reasoning` — keeps winning across a restart. These
//! tests run the real CLI against a loopback provider whose two models declare
//! different levels, and read what goes on the wire.

#![cfg(unix)]
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL_ONE: &str = "custom/onboarding/probe-one";
const MODEL_TWO: &str = "custom/onboarding/probe-two";
const ANSWER: &str = "reasoning persistence ok";

struct Fixture {
    root: tempfile::TempDir,
    server: MockServer,
}

/// Two models that declare different levels and defaults, so a kept choice is
/// never confused with a default: `probe-one` declares none/low/high/max with
/// `high`, `probe-two` declares none/medium/xhigh with `medium`.
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|_request: &Request| {
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
    "auto_discover":false, "models":[
        {"api_name":"probe-one", "context_window":32768, "max_output_tokens":4096,
         "tools":true, "parallel_tool_calls":false, "reasoning":true,
         "reasoning_values":["none","low","high","max"], "reasoning_default":"high"},
        {"api_name":"probe-two", "context_window":32768, "max_output_tokens":4096,
         "tools":true, "parallel_tool_calls":false, "reasoning":true,
         "reasoning_values":["none","medium","xhigh"], "reasoning_default":"medium"},
    ]}}});
    let registry_path = home.join(".octet/credentials/custom.json");
    std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    Fixture { root, server }
}

impl Fixture {
    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn settings_path(&self) -> PathBuf {
        self.home().join(".octet/config.toml")
    }

    /// Write the user-level settings the pickers persist (`/model`,
    /// `/settings default model`, `/settings default reasoning`).
    fn write_settings(&self, body: &str) {
        std::fs::write(self.settings_path(), body).unwrap();
    }

    async fn run(&self, args: &[&str]) -> String {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(env!("CARGO_BIN_EXE_octet"))
                .current_dir(self.root.path().join("workspace"))
                .env_clear()
                .env("HOME", self.home())
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("OCTET_SESSION_DIR", self.root.path().join("sessions"))
                .env("TERM", "dumb")
                .args([
                    "--offline",
                    "--no-context-files",
                    "--print",
                    "Explain the persistence rule.",
                ])
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("loopback persistence task exceeded its deadline")
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains(ANSWER), "{stdout}");
        stdout
    }

    /// The model and effort of the most recent chat request.
    async fn last_request(&self) -> (String, Option<String>) {
        let requests = self.server.received_requests().await.unwrap();
        let body: Value = requests
            .last()
            .expect("the endpoint saw no request")
            .body_json()
            .unwrap();
        (
            body["model"].as_str().unwrap().to_owned(),
            body.get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
    }

    /// The session's last recorded selection, as the harness reads it back.
    fn persisted_reasoning(&self, root: &Path) -> String {
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
}

#[tokio::test]
async fn picked_max_survives_a_restart() {
    let fixture = fixture().await;
    fixture
        .run(&["--model", MODEL_ONE, "--reasoning", "max"])
        .await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-one".to_owned(), Some("max".to_owned()))
    );
    assert_eq!(
        fixture.persisted_reasoning(fixture.root.path()),
        "max",
        "the pick is recorded in the session"
    );

    fixture.run(&["--continue"]).await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-one".to_owned(), Some("max".to_owned())),
        "a restart must keep the picked level instead of the declared default"
    );
}

#[tokio::test]
async fn picked_model_and_level_both_survive_a_restart() {
    let fixture = fixture().await;
    fixture
        .run(&["--model", MODEL_ONE, "--reasoning", "low"])
        .await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-one".to_owned(), Some("low".to_owned()))
    );

    fixture
        .run(&["--continue", "--model", MODEL_TWO, "--reasoning", "xhigh"])
        .await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-two".to_owned(), Some("xhigh".to_owned()))
    );

    fixture.run(&["--continue"]).await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-two".to_owned(), Some("xhigh".to_owned())),
        "both the switched model and its level must persist"
    );
}

#[tokio::test]
async fn a_fresh_session_uses_each_model_declared_default() {
    let fixture = fixture().await;
    fixture.run(&["--model", MODEL_TWO]).await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-two".to_owned(), Some("medium".to_owned())),
        "with nothing picked, the endpoint's declared default applies"
    );
    assert_eq!(fixture.persisted_reasoning(fixture.root.path()), "medium");
}

#[tokio::test]
async fn saved_settings_outlive_the_declared_default() {
    let fixture = fixture().await;
    // What `/settings default model|reasoning` persists for new sessions.
    fixture.write_settings("model = 'custom/onboarding/probe-two'\nreasoning = 'xhigh'\n");
    fixture.run(&[]).await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-two".to_owned(), Some("xhigh".to_owned())),
        "a saved setting must beat the model's declared default"
    );

    let settings = std::fs::read_to_string(fixture.settings_path()).unwrap();
    assert!(settings.contains("reasoning = 'xhigh'"), "{settings}");
}

/// A flag chooses for one run. It never rewrites the settings file, and the
/// session's recorded choice then outranks that file for the resumed session —
/// the documented `Resume keeps the saved choice` rule
/// (`docs/providers.md`, `reasoning_defaults.rs::explicit_off_and_resumed_choices_override_model_and_process_defaults`).
#[tokio::test]
async fn a_flag_overrides_for_that_run_without_rewriting_settings() {
    let fixture = fixture().await;
    fixture.write_settings("reasoning = 'max'\n");
    fixture
        .run(&["--model", MODEL_ONE, "--reasoning", "low"])
        .await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-one".to_owned(), Some("low".to_owned())),
        "the flag wins for this run"
    );
    let settings = std::fs::read_to_string(fixture.settings_path()).unwrap();
    assert!(settings.contains("reasoning = 'max'"), "{settings}");

    fixture.run(&["--continue"]).await;
    assert_eq!(
        fixture.last_request().await,
        ("probe-one".to_owned(), Some("low".to_owned())),
        "the resumed session keeps the choice it recorded"
    );
    let settings = std::fs::read_to_string(fixture.settings_path()).unwrap();
    assert!(settings.contains("reasoning = 'max'"), "{settings}");
}
