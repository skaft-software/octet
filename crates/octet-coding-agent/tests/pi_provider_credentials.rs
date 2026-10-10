//! Real CLI/Node acceptance for reviewed Pi model credentials.
//!
//! Runs the shipped binary with the checked-in Pi adapter, a real credential
//! file, and a reviewed factory that asks for its own model's credential the way
//! `pi-hermes-memory`'s direct auto-review path does. The reviewing
//! `configure.mjs` run decides the outcome: with `--provider-credentials` the
//! factory receives the value the host resolved, and without it the same factory
//! is refused before any credential can cross the boundary. Neither run may print
//! the value. Uses only loopback AI.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE_SECRET: &str = "end-to-end-fixture-secret";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_pi_factory_resolves_one_model_credential_through_the_real_host() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"id\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"credential probe\"},\"finish_reason\":null}]}\n\n",
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
    std::fs::create_dir_all(home.join(".octet/credentials")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let credential = home.join(".octet/credentials/custom.json");
    std::fs::write(
        &credential,
        json!({
            "base_url": format!("{}/v1/", server.uri()),
            "api_key": FIXTURE_SECRET, "api_name": "probe", "headers": [],
            "auto_discover": false, "models": [{"api_name": "probe"}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();

    // The reviewed factory asks the way `pi-hermes-memory` asks, and records only
    // the result its own factory callback observed. The marker path is generated
    // into the entry: an extension process never inherits arbitrary host
    // environment.
    let probe = |marker: &std::path::Path| {
        format!(
            r#"import {{ writeFileSync }} from 'node:fs';
export default pi => {{
  pi.on('before_agent_start', async (_event, ctx) => {{
    let observed;
    try {{
      observed = await ctx.modelRegistry.getApiKeyAndHeaders(ctx.model);
    }} catch (error) {{
      observed = {{ error: String(error?.message ?? error) }};
    }}
    writeFileSync({}, JSON.stringify(observed));
  }});
}};
"#,
            json!(&marker)
        )
    };

    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();

    // Two reviewed configurations of the same factory: the credential grant, and
    // the default that withholds it.
    let mut outputs = Vec::new();
    for (label, granted) in [("granted", true), ("default", false)] {
        let extensions = root_path.join(format!("{label}-extensions"));
        let marker = root_path.join(format!("{label}-credentials.json"));
        let entry = root_path.join(format!("{label}-credential-probe.mjs"));
        std::fs::write(&entry, probe(&marker)).unwrap();
        let mut configure = Command::new("node");
        configure
            .arg(adapter.join("configure.mjs"))
            .args(["--reviewed", "--output"])
            .arg(extensions.join("octet-pi-compat"))
            .arg(&entry)
            .current_dir(&workspace)
            .env("PI_OFFLINE", "1")
            .kill_on_drop(true);
        if granted {
            configure.arg("--provider-credentials");
        }
        let configured = configure
            .output()
            .await
            .expect("Node and adapter dependencies are required");
        assert!(
            configured.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&configured.stderr)
        );
        let manifest =
            std::fs::read_to_string(extensions.join("octet-pi-compat/extension.toml")).unwrap();
        assert_eq!(
            manifest.contains("provider_credentials = true"),
            granted,
            "{label}: the reviewed capability must follow the explicit grant"
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
            .args(["--model", "custom/probe", "--tools", "read"])
            .arg("--workspace")
            .arg(&workspace)
            .arg("--extension-dir")
            .arg(&extensions)
            .args(["--enable-extension", "octet-pi-compat"])
            .args(["--trust-extension", "octet-pi-compat"])
            .arg("--print")
            .arg("probe")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(20), command.output())
            .await
            .expect("bounded headless invocation")
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "{label}: stdout={stdout} stderr={stderr}"
        );
        // The value reaches the reviewed factory and nothing else the run prints.
        assert!(!stdout.contains(FIXTURE_SECRET), "{label} stdout leaked");
        assert!(!stderr.contains(FIXTURE_SECRET), "{label} stderr leaked");
        let observed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap_or_else(|error| {
                panic!("{label}: the factory did not observe a result: {error}; stderr={stderr}")
            }))
            .unwrap();
        outputs.push((label, observed));
    }

    let (_, granted) = &outputs[0];
    assert_eq!(granted["ok"], json!(true));
    assert_eq!(granted["apiKey"], json!(FIXTURE_SECRET));
    assert!(granted.get("error").is_none(), "{granted}");

    let (_, withheld) = &outputs[1];
    assert!(
        withheld["error"]
            .as_str()
            .is_some_and(|error| error.contains("unsupported_feature provider_credentials")),
        "{withheld}"
    );
    assert!(!withheld.to_string().contains(FIXTURE_SECRET));
}
