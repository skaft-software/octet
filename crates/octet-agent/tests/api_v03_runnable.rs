#![cfg(unix)]
#![allow(missing_docs)]

use std::path::Path;
use std::time::Duration;

use octet_agent::extension_process::{
    ExtensionRuntimeError, EXTENSION_FEATURE_CONTENT_PARTS, EXTENSION_FEATURE_REQUEST_CANCELLATION,
};
use octet_agent::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust, EXTENSION_MANIFEST_FILENAME,
};
use serde_json::json;
use tempfile::TempDir;

fn trusted_descriptor(
    manifest_path: std::path::PathBuf,
    manifest: ExtensionManifest,
) -> DiscoveredExtension {
    DiscoveredExtension {
        manifest,
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    }
}

#[tokio::test]
async fn runnable_api_v03_example_negotiates_calls_cancels_and_shutdowns() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/extensions/api-v03-minimal")
        .canonicalize()
        .expect("canonical API 0.3 example path");
    let published_manifest_path = repository.join(EXTENSION_MANIFEST_FILENAME);
    let published_manifest = std::fs::read_to_string(&published_manifest_path)
        .expect("published API 0.3 example manifest");
    let mut fixture_manifest: toml::Value =
        toml::from_str(&published_manifest).expect("published manifest TOML");
    assert_eq!(fixture_manifest["requires_octet"].as_str(), Some("=0.7.6"));
    assert_eq!(fixture_manifest["api_version"].as_str(), Some("0.3"));
    assert_eq!(fixture_manifest["version"].as_str(), Some("0.1.0"));
    let released_requirement = semver::VersionReq::parse("=0.7.6").unwrap();
    let current_host = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    if released_requirement.matches(&current_host) {
        ExtensionManifest::load(&published_manifest_path).expect("matching released runtime pin");
    } else {
        let error = ExtensionManifest::load(&published_manifest_path)
            .expect_err("an unchanged release example must reject a different host version");
        assert!(error.to_string().contains("extension requires octet"));
    }

    // Qualify the unchanged process source against this checkout's host without
    // republishing the example or loosening its released runtime requirement.
    let staging = TempDir::new().expect("private example staging");
    let example = staging.path().join("api-v03-minimal");
    std::fs::create_dir(&example).unwrap();
    let current_requirement = format!("={}", env!("CARGO_PKG_VERSION"));
    fixture_manifest["requires_octet"] = toml::Value::String(current_requirement.clone());
    let manifest_path = example.join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(&manifest_path, toml::to_string(&fixture_manifest).unwrap()).unwrap();
    std::fs::copy(
        repository.join("extension.py"),
        example.join("extension.py"),
    )
    .expect("copy byte-identical executable example with its permissions");
    assert_eq!(
        std::fs::read(repository.join("extension.py")).unwrap(),
        std::fs::read(example.join("extension.py")).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&published_manifest_path).unwrap(),
        published_manifest
    );
    let manifest = ExtensionManifest::load(&manifest_path).expect("staged API 0.3 manifest");
    assert_eq!(manifest.api_version, "0.3");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(
        manifest.requires_octet.as_deref(),
        Some(current_requirement.as_str())
    );

    let workspace = TempDir::new().expect("workspace");
    let mut config = ExtensionRuntimeConfig::new(workspace.path());
    // Shutdown must cancel this delayed request before the request deadline;
    // otherwise the two independent timers can report a timeout instead.
    config.request_timeout = Duration::from_secs(3);
    config.shutdown_timeout = Duration::from_secs(1);
    let process = ExtensionProcess::start(trusted_descriptor(manifest_path, manifest), config)
        .await
        .expect("start the privately staged API 0.3 example for this host build");

    assert_eq!(process.api_version(), "0.3");
    let negotiated = process.negotiated_features();
    assert!(negotiated.contains(EXTENSION_FEATURE_CONTENT_PARTS));
    assert!(negotiated.contains(EXTENSION_FEATURE_REQUEST_CANCELLATION));
    let tools = process.tool_definitions();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let output = process
        .call_tool(
            "echo",
            json!({"text": "hello from the host"}),
            process.current_context(),
        )
        .await
        .expect("call the example's real tool");
    assert_eq!(output.content, "hello from the host");
    assert!(!output.is_error);
    assert_eq!(output.metadata, json!({"delay_ms": 0}));
    assert_eq!(
        output.structured_content,
        Some(json!({"text": "hello from the host"}))
    );

    let cancellation_process = process.clone();
    let cancellation = tokio::spawn(async move {
        cancellation_process
            .call_tool(
                "echo",
                json!({"text": "cancel me", "delay_ms": 5_000}),
                cancellation_process.current_context(),
            )
            .await
    });
    // A spawned future can be cancelled before request_inner admits its frame.
    // Observe host admission instead of racing shutdown against a fixed delay.
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if process.health_snapshot().pending_requests > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the delayed tool call was not admitted");
    assert_eq!(
        process.health_snapshot().pending_requests,
        1,
        "the delayed tool call must be pending before shutdown"
    );
    assert!(
        process.shutdown().await,
        "the example must acknowledge shutdown"
    );

    match cancellation.await.expect("cancellation task") {
        Err(ExtensionRuntimeError::Cancelled { method, reason }) => {
            assert_eq!(method, "tool/call");
            assert_eq!(reason, "shutdown");
        }
        other => panic!("expected host cancellation, got {other:?}"),
    }
    assert!(!process.is_running());
}

#[tokio::test]
async fn unimplemented_theme_selection_is_not_offered_and_returns_a_canonical_refusal() {
    let directory = TempDir::new().unwrap();
    let manifest_path = directory.path().join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(
        &manifest_path,
        r#"name = "theme-probe"
version = "0.1.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = ["probe.py"]
[contributes]
tools = ["probe"]
"#,
    )
    .unwrap();
    // This peer selects theme/select iff offered, then exercises the real
    // reverse-request path. Before the fix it negotiated an unimplemented
    // service and received a noncanonical legacy method-not-found response.
    std::fs::write(
        directory.path().join("probe.py"),
        r#"import json, sys

def send(value):
    print(json.dumps(value, sort_keys=True, separators=(",", ":")), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        offer = request["params"]["contract"]
        capabilities = offer["required_capabilities"][:]
        methods = offer["required_methods"][:]
        theme_offered = "theme_selection" in offer["optional_capabilities"]
        method_offered = "theme/select" in offer["optional_methods"]
        if theme_offered:
            capabilities.append("theme_selection")
        if method_offered:
            methods.append("theme/select")
        selection = {"schema": offer["schema"], "encoding": offer["encoding"],
                     "capabilities": sorted(capabilities), "methods": sorted(methods),
                     "limits": offer["limits"]}
        send({"jsonrpc": "2.0", "id": request["id"], "result": {
            "api_version": "0.3", "contract": selection,
            "tools": [{"name": "probe", "description": "Probe theme availability",
                       "parameters": {"type": "object", "properties": {}}}]}})
    elif method == "tool/call":
        call_id = request["id"]
        send({"jsonrpc": "2.0", "id": "theme-probe", "method": "theme/select",
              "params": {"namespace": "theme-probe", "theme_id": "dark",
                         "role": "default", "scope": "extension"}})
    elif request.get("id") == "theme-probe":
        send({"jsonrpc": "2.0", "id": call_id, "result": {
            "content": [{"type": "text", "text": "probe complete"}],
            "is_error": False, "metadata": None,
            "structured_content": {"theme_offered": theme_offered,
                                   "method_offered": method_offered,
                                   "response": request}}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": request["id"], "result": {"terminal": "shutdown"}})
        break
"#,
    )
    .unwrap();
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(directory.path());
    config.request_timeout = Duration::from_secs(3);
    config.shutdown_timeout = Duration::from_secs(1);
    let process = ExtensionProcess::start(trusted_descriptor(manifest_path, manifest), config)
        .await
        .unwrap();
    let output = process
        .call_tool("probe", json!({}), process.current_context())
        .await
        .unwrap();
    let evidence = output.structured_content.unwrap();
    assert_eq!(evidence["theme_offered"], false);
    assert_eq!(evidence["method_offered"], false);
    assert_eq!(evidence["response"]["error"]["code"], -32601);
    assert_eq!(
        evidence["response"]["error"]["message"],
        "unknown or unnegotiated method"
    );
    octet_agent::extension_api_v03::parse_json_rpc_envelope(evidence["response"].clone()).unwrap();
    assert!(process.is_running());
    assert!(process.shutdown().await);
}
