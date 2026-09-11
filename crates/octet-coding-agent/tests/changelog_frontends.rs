//! Bare release-note commands stay local; explicit template inputs stay model data.

use std::fs;
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[derive(Clone, Copy, Debug)]
enum Frontend {
    Print,
    PlainPositional,
    PlainStdin,
}

async fn invoke(frontend: Frontend, input: &str, template: Option<&str>) -> (Output, Vec<Request>) {
    let server = MockServer::start().await;
    let response = concat!(
        "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"fixture response done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(response),
        )
        .mount(&server)
        .await;

    // Never inherit real credentials, user sessions, workspace resources, or
    // provider discovery. Offline inference goes only to this loopback fixture.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&sessions).unwrap();
    let credential = home.join(".octet/credentials/custom.json");
    fs::write(
        &credential,
        json!({
            "base_url": format!("{}/v1/", server.uri()),
            "api_key": "", "api_name": "probe", "headers": [],
            "models": [], "auto_discover": false,
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
    command
        .args([
            "--offline",
            "--no-context-files",
            "--no-tools",
            "--color",
            "never",
            "--model",
            "custom/probe",
            "--max-turns",
            "1",
            "--system-prompt",
            "Prompt routing fixture",
        ])
        .arg("--workspace")
        .arg(&workspace)
        .arg("--session-dir")
        .arg(&sessions)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("PWD", &workspace)
        .env("TERM", "dumb")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(template) = template {
        fs::create_dir_all(home.join(".octet/prompts")).unwrap();
        fs::write(home.join(".octet/prompts/release-fixture.md"), template).unwrap();
        command.args(["--prompt", "release-fixture"]);
    }
    match frontend {
        Frontend::Print => {
            command.args(["--print", input]);
        }
        Frontend::PlainPositional => {
            command.args(["--plain", input]);
        }
        Frontend::PlainStdin => {
            command.arg("--plain").stdin(Stdio::piped());
        }
    }
    let mut child = command.spawn().expect("spawn isolated octet");
    if let Frontend::PlainStdin = frontend {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).await.unwrap();
        // Dropping stdin closes the pipe so plain mode can finish reading.
    }
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("octet frontend timed out")
        .expect("wait for octet frontend");
    (output, server.received_requests().await.unwrap())
}

async fn assert_template_data(frontend: Frontend) {
    for input in ["/changelog", "/chang"] {
        for template in ["Explain {{prompt}}", "/changelog"] {
            let (output, requests) = invoke(frontend, input, Some(template)).await;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let context = format!("{frontend:?} input={input:?} template={template:?}; stdout={stdout}; stderr={stderr}");
            assert!(output.status.success(), "{context}");
            assert!(stdout.contains("fixture response done"), "{context}");
            assert!(
                !stderr.contains("available in the interactive TUI"),
                "{context}"
            );
            assert_eq!(requests.len(), 1, "{context}");
            let request = &requests[0];
            assert!(!request.headers.contains_key("authorization"));
            let body: Value = request.body_json().unwrap();
            let users = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "user")
                .map(|message| message["content"].as_str().unwrap())
                .collect::<Vec<_>>();
            // Check actual provider-bound text, including a complete expansion
            // that looks exactly like a local command. Never reparse it.
            let expected = template.replace("{{prompt}}", input);
            assert_eq!(users, vec![expected.as_str()], "{context}");
            assert!(body
                .get("tools")
                .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
        }
    }
}

#[tokio::test]
async fn print_preserves_explicit_prompt_template_data() {
    assert_template_data(Frontend::Print).await;
}

#[tokio::test]
async fn positional_plain_preserves_explicit_prompt_template_data() {
    assert_template_data(Frontend::PlainPositional).await;
}

#[tokio::test]
async fn piped_plain_preserves_explicit_prompt_template_data() {
    assert_template_data(Frontend::PlainStdin).await;
}

#[tokio::test]
async fn bare_changelog_commands_are_rejected_without_provider_requests() {
    for frontend in [
        Frontend::Print,
        Frontend::PlainPositional,
        Frontend::PlainStdin,
    ] {
        for input in ["/changelog", "/chang"] {
            let (output, requests) = invoke(frontend, input, None).await;
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{frontend:?} {input}: {stderr}");
            assert!(
                stderr.contains("/changelog is available in the interactive TUI"),
                "{frontend:?} {input}: {stderr}"
            );
            assert!(
                requests.is_empty(),
                "{frontend:?} {input} reached a provider"
            );
        }
    }
}
