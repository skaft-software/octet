//! Real CLI/Pi regression: tool guards must be able to read the host tool catalog.
//! Uses only loopback inference and the locked local adapter dependencies.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_tool_guards_answer_snapshots_and_preserve_denials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let has_result = body["messages"].as_array().unwrap().iter()
                .any(|message| message["role"] == "tool");
            let (delta, finish) = if has_result {
                (json!({"content": "guard completed"}), "stop")
            } else {
                (json!({"tool_calls": [{"index": 0, "id": "read-call", "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"fixture.txt\"}"}}]}), "tool_calls")
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"probe", "choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                    json!({"id":"probe", "choices":[{"index":0,"delta":{},"finish_reason":finish}],
                        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})))
        })
        .mount(&server).await;

    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let home = root.join("home");
    let workspace = root.join("workspace");
    let extensions = root.join("extensions");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(workspace.join(".pi")).unwrap();
    std::fs::write(workspace.join(".pi/settings.json"), "{}").unwrap();
    std::fs::write(workspace.join("fixture.txt"), "verified tool content").unwrap();
    let credential = home.join(".octet/credentials/custom.json");
    std::fs::write(
        &credential,
        json!({
            "base_url": format!("{}/v1/", server.uri()), "api_key":"", "api_name":"probe",
            "headers":[], "auto_discover":false,
            "models":[{"api_name":"probe","context_window":32768}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let marker = workspace.join("guard-tools.json");
    let after_marker = workspace.join("after-response-tools.json");
    let factory = root.join("guard.mjs");
    std::fs::write(
        &factory,
        format!(
            r#"import {{ writeFileSync, existsSync }} from 'node:fs';
export default pi => {{
  pi.on('tool_call', () => {{
    const tools = pi.getActiveTools();
    writeFileSync({}, JSON.stringify(tools));
    if (existsSync({})) return {{block: true, reason: 'deliberate guard denial'}};
  }});
  pi.on('after_response', () => {{
    const tools = pi.getActiveTools();
    writeFileSync({}, JSON.stringify(tools));
  }});
}};
"#,
            json!(marker),
            json!(workspace.join("deny")),
            json!(after_marker)
        ),
    )
    .unwrap();
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let configured = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .args(["--reviewed", "--output"])
        .arg(extensions.join("octet-pi-compat"))
        .arg(&factory)
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("PI_OFFLINE", "1")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );

    for mode in ["print", "json", "plain", "rpc"] {
        for denied in [false, true] {
            let _ = std::fs::remove_file(&marker);
            let _ = std::fs::remove_file(&after_marker);
            if denied {
                std::fs::write(workspace.join("deny"), "").unwrap();
            } else {
                let _ = std::fs::remove_file(workspace.join("deny"));
            }
            let offset = server.received_requests().await.unwrap().len();
            let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
            command
                .current_dir(&workspace)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("PWD", &workspace)
                .env("TERM", "dumb")
                .env("LANG", "C.UTF-8")
                .env("PI_OFFLINE", "1")
                .args(["--offline", "--no-context-files", "--color", "never"])
                .args(["--model", "custom/probe", "--tools", "read"])
                .arg("--workspace")
                .arg(&workspace)
                .arg("--extension-dir")
                .arg(&extensions)
                .args([
                    "--enable-extension",
                    "octet-pi-compat",
                    "--trust-extension",
                    "octet-pi-compat",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            match mode {
                "print" => {
                    command.args(["--print", "probe"]);
                }
                "json" => {
                    command.args(["--mode", "json", "probe"]);
                }
                "plain" => {
                    command.args(["--plain", "probe"]);
                }
                "rpc" => {
                    command.args(["--mode", "rpc"]);
                }
                _ => unreachable!(),
            }
            command.stdin(if mode == "rpc" {
                Stdio::piped()
            } else {
                Stdio::null()
            });
            let started = std::time::Instant::now();
            let output = tokio::time::timeout(Duration::from_secs(20), async {
                let mut child = command.spawn().unwrap();
                if mode != "rpc" {
                    return child.wait_with_output().await.unwrap();
                }
                let mut input = child.stdin.take().unwrap();
                input
                    .write_all(b"{\"type\":\"prompt\",\"message\":\"probe\"}\n")
                    .await
                    .unwrap();
                let mut stdout = BufReader::new(child.stdout.take().unwrap());
                let mut captured = String::new();
                loop {
                    let mut line = String::new();
                    assert_ne!(
                        stdout.read_line(&mut line).await.unwrap(),
                        0,
                        "RPC lost before settlement"
                    );
                    let settled =
                        serde_json::from_str::<Value>(&line).unwrap()["type"] == "agent_settled";
                    captured.push_str(&line);
                    if settled {
                        break;
                    }
                }
                drop(input);
                stdout.read_to_string(&mut captured).await.unwrap();
                let mut output = child.wait_with_output().await.unwrap();
                output.stdout = captured.into_bytes();
                output
            })
            .await
            .unwrap_or_else(|_| {
                panic!("{mode}, denied={denied}: tool guard waited for the host timeout")
            });
            // A permanent stall trips the outer timeout; this bound also catches
            // a partial regression that answers only after an unrelated idle
            // host event rather than from the headless pump itself.
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{mode}, denied={denied}: guard answered only after {:?}",
                started.elapsed()
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{mode}: {stderr}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("guard completed"),
                "{mode}: {stderr}"
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(&marker).unwrap_or_else(
                    |e| panic!("{mode}: guard never received tools: {e}; {stderr}")
                ))
                .unwrap(),
                json!(["read"])
            );
            let requests = server.received_requests().await.unwrap();
            assert_eq!(
                requests.len() - offset,
                2,
                "{mode}: one tool turn, one final answer"
            );
            let follow_up: Value = serde_json::from_slice(&requests[offset + 1].body).unwrap();
            let result = follow_up["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "tool")
                .unwrap()["content"]
                .to_string();
            if denied {
                assert!(
                    result.contains("deliberate guard denial"),
                    "{mode}: {result}"
                );
                assert!(
                    !result.contains("verified tool content"),
                    "denied tool ran: {result}"
                );
            } else {
                assert!(
                    result.contains("verified tool content"),
                    "{mode}: {result}; {stderr}"
                );
            }
            // Settlement dispatches the after_response hook (print/json/plain
            // before exit, RPC before agent_settled); its reverse tools/snapshot
            // must be answered from the headless pump, not from the hook's own
            // 2s deadline, so the marker only exists if the pump ran.
            let after = std::fs::read(&after_marker).unwrap_or_else(|e| {
                panic!(
                    "{mode}, denied={denied}: after_response never received tools: {e}; {stderr}"
                )
            });
            assert_eq!(
                serde_json::from_slice::<Value>(&after).unwrap(),
                json!(["read"]),
                "{mode}, denied={denied}: after_response active tools"
            );
        }
    }
}
