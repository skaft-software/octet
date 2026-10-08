//! Real CLI/Node acceptance: a Pi tool image result keeps its `turn_end` observation.
//! Requires the checked-in Pi adapter's Node dependencies; uses only loopback AI.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

/// The exact inline frame bytes the durable entry fixture pins
/// (`extensions/octet-pi-compat/test/fixtures/model-turn-image-entry.json`).
const FRAME_BASE64: &str = "iVBORw0KGgpwaS1kb29tLWZyYW1l";

/// One scripted chat-completions run: a `frame` tool call, then a final answer.
struct Turns {
    index: AtomicUsize,
}

impl Respond for Turns {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let body = match self.index.fetch_add(1, Ordering::SeqCst) {
            0 => concat!(
                "data: {\"id\":\"chat-tools\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"frame-call\",\"type\":\"function\",\"function\":{\"name\":\"frame\",\"arguments\":\"{}\"}}]}}]}\n\n",
                "data: {\"id\":\"chat-tools\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
            _ => concat!(
                "data: {\"id\":\"chat\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"frame observed\"}}]}\n\n",
                "data: {\"id\":\"chat\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2,\"total_tokens\":9}}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_completions_image_tool_result_keeps_the_pi_turn_end_observation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Turns {
            index: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let home = root_path.join("home");
    let workspace = root_path.join("workspace");
    let extensions = root_path.join("extensions");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    // An image-capable chat-completions model, exactly like the reported session.
    let credential = home.join(".octet/credentials/custom.json");
    std::fs::write(
        &credential,
        json!({
            "base_url": format!("{}/v1/", server.uri()),
            "api_key": "", "api_name": "probe", "headers": [],
            "auto_discover": false,
            "models": [{"api_name": "probe", "vision": true, "tools": true,
                "context_window": 32768, "max_output_tokens": 2048}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let marker = workspace.join("turn-end.json");
    let entry = root_path.join("frame.mjs");
    std::fs::write(
        &entry,
        format!(
            r#"import {{ writeFileSync }} from 'node:fs';
const observed = [];
export default pi => {{
  pi.registerTool({{
    name: 'frame', description: 'Render one fullscreen frame.',
    parameters: {{ type: 'object', properties: {{}}, additionalProperties: false }},
    async execute() {{
      return {{ content: [{{ type: 'text', text: 'frame rendered' }},
        {{ type: 'image', data: '{FRAME_BASE64}', mimeType: 'image/png' }}] }};
    }},
  }});
  pi.on('turn_end', event => {{
    observed.push({{ index: event.turnIndex, tools: event.toolResults }});
    writeFileSync({}, JSON.stringify(observed));
  }});
}};
"#,
            json!(marker),
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
        .arg(&entry)
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("PI_OFFLINE", "1")
        .kill_on_drop(true)
        .output()
        .await
        .expect("Node and adapter dependencies are required");
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );

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
        .args(["--model", "custom/probe"])
        .arg("--workspace")
        .arg(&workspace)
        .arg("--extension-dir")
        .arg(&extensions)
        .args(["--enable-extension", "octet-pi-compat"])
        .args(["--trust-extension", "octet-pi-compat"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.stdin(Stdio::null());
    command.arg("--print").arg("render a frame");
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .expect("bounded headless invocation")
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !stderr.contains("[Extension issues]") && !stderr.contains("could not convert the turn"),
        "the image tool result must not skip turn_end observations: {stderr}"
    );
    let observed: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&marker)
            .unwrap_or_else(|error| panic!("turn_end was not observed: {error}; stderr={stderr}")),
    )
    .unwrap();
    let turns = observed.as_array().expect("observed turns");
    let with_tools = turns
        .iter()
        .find(|turn| {
            turn["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        })
        .unwrap_or_else(|| panic!("no turn carried the tool result: {observed}"));
    let mut observed_result = with_tools["tools"][0].clone();
    let timestamp = observed_result["timestamp"].as_u64();
    if let Some(object) = observed_result.as_object_mut() {
        object.remove("timestamp");
    }
    assert!(
        timestamp.is_some_and(|value| value > 0),
        "Pi result carries the durable entry timestamp: {observed}"
    );
    assert_eq!(
        observed_result,
        json!({
            "role": "toolResult", "toolCallId": "frame-call", "toolName": "frame",
            "content": [
                {"type": "text", "text": "frame rendered"},
                {"type": "image", "data": FRAME_BASE64, "mimeType": "image/png"}
            ],
            "isError": false
        }),
        "Pi must see one tool result whose content holds the image: {observed}"
    );

    // The native wire lowering is unchanged: the image follows the tool message
    // as a user image part, exactly like Pi's openai-completions provider.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "tool call turn plus final answer");
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let messages = second["messages"].as_array().unwrap();
    let positions = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        positions.last().copied(),
        Some("user"),
        "chat-completions carries tool-result images in a follow-up user message: {second}"
    );
    assert!(
        messages.last().unwrap().to_string().contains(FRAME_BASE64),
        "the replayed image bytes must reach the provider"
    );
}
