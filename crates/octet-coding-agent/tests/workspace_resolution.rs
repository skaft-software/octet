//! Real process cwd, tool defaults and resume identity with a dotfiles Git
//! repository at HOME. All sessions and provider responses are disposable.

#![cfg(unix)]
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
    server: MockServer,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = root.join("home");
        let alpha = home.join("alpha");
        let beta = home.join("beta");
        for dir in [
            home.join(".git"),
            home.join(".octet/credentials"),
            alpha.clone(),
            beta.clone(),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(|request: &Request| {
                let body: Value = request.body_json().unwrap();
                let last = body["messages"].as_array().unwrap().last().unwrap();
                let (delta, finish) = if last["role"] == "user" && last["content"] == "probe cwd" {
                    (
                        json!({"tool_calls": [{"index": 0, "id": "cwd-probe",
                        "type": "function", "function": {"name": "bash",
                        "arguments": json!({"command": "pwd"}).to_string()}}]}),
                        "tool_calls",
                    )
                } else {
                    (json!({"content": "workspace fixture ok"}), "stop")
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
        let registry = json!({"version":1, "providers":{"workspace":{
            "base_url":format!("{}/v1/", server.uri()), "auth":{"kind":"none"},
            "auto_discover":false, "models":[{"api_name":"probe", "context_window":32768,
            "max_output_tokens":4096, "tools":true, "parallel_tool_calls":false}]}}});
        let registry_path = home.join(".octet/credentials/custom.json");
        std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _directory: directory,
            root,
            home,
            alpha,
            beta,
            server,
        }
    }

    async fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(env!("CARGO_BIN_EXE_octet"))
                .current_dir(cwd)
                .env_clear()
                .env("HOME", &self.home)
                .env("PWD", &self.home) // Intentionally stale; current_dir is authoritative.
                .env("TMPDIR", &self.root)
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("OCTET_SESSION_DIR", self.root.join("sessions"))
                .env("TERM", "dumb")
                .args([
                    "--offline",
                    "--no-context-files",
                    "--no-extensions",
                    "--color",
                    "never",
                ])
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("bounded workspace fixture invocation")
        .unwrap()
    }

    async fn prompt(&self, cwd: &Path, prompt: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            "--model",
            "custom/workspace/probe",
            "--tools",
            "bash",
            "--print",
            prompt,
        ];
        args.extend_from_slice(extra);
        self.run(cwd, &args).await
    }

    fn success(output: &Output) -> String {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn transcripts(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.root.join("sessions"))
            .unwrap()
            .flat_map(|entry| std::fs::read_dir(entry.unwrap().path()).unwrap())
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().is_some_and(|ext| ext == "jsonl")).then_some(path)
            })
            .collect()
    }
}

#[tokio::test]
async fn home_git_does_not_change_effective_tool_cwd() {
    let fixture = Fixture::new().await;
    assert_tool_workspace(&fixture, &fixture.alpha, &[]).await;
}

#[tokio::test]
async fn ordinary_home_subdirectory_keeps_effective_tool_cwd() {
    let fixture = Fixture::new().await;
    // Remove only this fixture's empty marker: the ordinary no-Git control.
    std::fs::remove_dir(fixture.home.join(".git")).unwrap();
    assert_tool_workspace(&fixture, &fixture.alpha, &[]).await;
}

#[tokio::test]
async fn explicit_home_workspace_overrides_implicit_child_boundary() {
    let fixture = Fixture::new().await;
    assert_tool_workspace(
        &fixture,
        &fixture.home,
        &["--workspace", fixture.home.to_str().unwrap()],
    )
    .await;
}

async fn assert_tool_workspace(fixture: &Fixture, expected: &Path, extra: &[&str]) {
    Fixture::success(&fixture.prompt(&fixture.alpha, "probe cwd", extra).await);
    let requests = fixture.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "one shell probe, then one answer");
    let body: Value = requests[1].body_json().unwrap();
    let result = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(
        result.contains(&format!("{}\n", expected.display())),
        "actual default bash cwd: {result}"
    );
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains(&format!("Workspace root: {}\n", expected.display())),
        "{system}"
    );
    assert!(
        system.contains(&format!(
            "Invocation directory: {}\n",
            fixture.alpha.display()
        )),
        "{system}"
    );
}

#[tokio::test]
async fn home_git_siblings_keep_default_resume_lists_and_continue_separate() {
    let fixture = Fixture::new().await;
    Fixture::success(
        &fixture
            .prompt(&fixture.alpha, "alpha history marker", &[])
            .await,
    );
    Fixture::success(
        &fixture
            .prompt(&fixture.beta, "beta history marker", &[])
            .await,
    );
    let alpha = Fixture::success(&fixture.run(&fixture.alpha, &["sessions", "list"]).await);
    let beta = Fixture::success(&fixture.run(&fixture.beta, &["sessions", "list"]).await);
    assert!(
        alpha.contains("alpha history marker") && !alpha.contains("beta history marker"),
        "alpha default resume list: {alpha}"
    );
    assert!(
        beta.contains("beta history marker") && !beta.contains("alpha history marker"),
        "beta default resume list: {beta}"
    );
    let paths = fixture.transcripts();
    assert_eq!(paths.len(), 2);
    assert_ne!(
        paths[0].parent(),
        paths[1].parent(),
        "distinct workspace-key directories"
    );
    for cwd in [&fixture.alpha, &fixture.beta] {
        Fixture::success(
            &fixture
                .prompt(cwd, "continue own history", &["--continue"])
                .await,
        );
        let requests = fixture.server.received_requests().await.unwrap();
        let body: Value = requests.last().unwrap().body_json().unwrap();
        let history = body["messages"].to_string();
        let (own, other) = if cwd == &fixture.alpha {
            ("alpha history marker", "beta history marker")
        } else {
            ("beta history marker", "alpha history marker")
        };
        assert!(
            history.contains(own) && !history.contains(other),
            "{history}"
        );
    }
    let alpha_path = paths
        .iter()
        .find(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains("alpha history marker")
        })
        .unwrap();
    let id = alpha_path.file_stem().unwrap().to_str().unwrap();
    Fixture::success(
        &fixture
            .prompt(&fixture.alpha, "by-id resume", &["--resume", id])
            .await,
    );
    let refused = fixture
        .prompt(&fixture.beta, "wrong-workspace by-id", &["--resume", id])
        .await;
    assert!(
        !refused.status.success(),
        "by-id must not acquire another workspace's session"
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("was not found"));
    assert_eq!(
        fixture.transcripts().len(),
        2,
        "resume never creates or migrates sessions"
    );
}
