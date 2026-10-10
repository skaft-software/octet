//! Real CLI/Node regression for synchronous reverse requests inside BeforePrompt.
//! Requires the checked-in Pi adapter's Node dependencies; uses only loopback AI.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_before_prompt_answers_pi_tool_snapshot_without_hook_timeout() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hook completed\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let home = root_path.join("home");
    let workspace = root_path.join("workspace");
    let extensions = root_path.join("extensions");
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let credential = home.join(".octet/credentials/custom.json");
    std::fs::write(
        &credential,
        json!({
            "base_url": format!("{}/v1/", server.uri()),
            "api_key": "", "api_name": "probe", "headers": [],
            "auto_discover": false, "models": [{"api_name": "probe"}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let marker = workspace.join("before-prompt-tools.json");
    let entry = root_path.join("probe.mjs");
    std::fs::write(
        &entry,
        format!(
            r#"import {{ writeFileSync }} from 'node:fs';
export default pi => {{
  pi.on('turn_start', () => {{ throw new Error('private callback details'); }});
  pi.on('turn_start', () => {{ writeFileSync({1}, 'continued'); }});
  pi.on('before_agent_start', async (_event, ctx) => {{
    // This is synchronous Pi API backed by an owner-fenced native reverse RPC.
    const active = pi.getActiveTools();
    pi.setActiveTools([]);
    const narrowed = pi.getActiveTools();
    // Unknown and policy-excluded names cannot expand the native tool surface.
    pi.setActiveTools(['read', 'bash', 'unknown']);
    if (_event.prompt === 'probe ui') await ctx.ui.setEditorText('no headless composer');
    writeFileSync({}, JSON.stringify({{ active, narrowed, after: pi.getActiveTools(), sessionDir: ctx.sessionManager.getSessionDir(), sessionFile: ctx.sessionManager.getSessionFile(), projectTrusted: ctx.isProjectTrusted(), hasUI: ctx.hasUI, mode: ctx.mode, sessionId: ctx.sessionId, managerSessionId: ctx.sessionManager.getSessionId() }}));
  }});
}};
"#,
            json!(marker),
            json!(workspace.join("turn-start-survived.txt")),
        ),
    )
    .unwrap();
    let optional = root_path.join("optional.mjs");
    std::fs::write(
        &optional,
        "export default pi => { pi.registerCommand('discard', {handler(){}}); pi.registerShortcut('alt+shift+z', {handler(){}}); };",
    ).unwrap();
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let configured = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .args(["--reviewed", "--output"])
        .arg(extensions.join("octet-pi-compat"))
        .arg(&entry)
        .arg(&optional)
        .current_dir(&workspace)
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

    // Losing one reviewed factory must not prevent the other from starting.
    std::fs::remove_file(&optional).unwrap();
    // Both print presentations prepare their first prompt headlessly.
    for mode in ["text", "json", "headless-ui"] {
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(workspace.join("turn-start-survived.txt"));
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
            .args(["--enable-extension", "octet-pi-compat"])
            .args(["--trust-extension", "octet-pi-compat"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.stdin(Stdio::null());
        if mode == "json" {
            command.args(["--mode", "json", "--workspace-trusted"]);
        } else {
            command.arg("--print");
        }
        command.arg(if mode == "headless-ui" {
            "probe ui"
        } else {
            "probe"
        });
        let output = tokio::time::timeout(Duration::from_secs(20), command.output())
            .await
            .expect("bounded headless invocation")
            .unwrap();
        if mode == "headless-ui" {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("no foreground composer"));
            continue;
        }
        assert!(
            output.status.success(),
            "{mode}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("[Extension issues]") && stderr.contains("optional.mjs"),
            "the skipped factory must be reported with its path: {stderr}"
        );
        assert!(
            stderr.contains("turn_start callback failed and was skipped"),
            "the failing callback must be reported with its event: {stderr}"
        );
        assert!(
            !stderr.contains("private callback details"),
            "callback exceptions must stay redacted: {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("turn-start-survived.txt")).unwrap(),
            "continued"
        );
        let observed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap_or_else(|error| {
                panic!(
                    "{mode}: before_prompt snapshot did not complete: {error}; stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                )
            }))
            .unwrap();
        assert_eq!(observed["projectTrusted"], json!(mode == "json"));
        assert_eq!(observed["hasUI"], json!(false));
        assert_eq!(observed["mode"], json!("print"));
        assert_eq!(observed["sessionId"], observed["managerSessionId"]);
        assert!(!observed["sessionId"].as_str().unwrap().is_empty());
        assert_eq!(
            observed["sessionDir"].as_str().unwrap(),
            std::path::Path::new(observed["sessionFile"].as_str().unwrap())
                .parent()
                .unwrap()
                .to_str()
                .unwrap(),
            "session directory must come from the native session file"
        );
        assert_eq!(
            observed["active"],
            json!(["read"]),
            "host-policed tool surface"
        );
        assert_eq!(
            observed["after"], observed["active"],
            "selection must not widen the host-policed tool surface"
        );
        assert_eq!(
            observed["narrowed"],
            json!([]),
            "selection narrows tools headlessly"
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "each accepted prompt reached the provider"
    );
}
