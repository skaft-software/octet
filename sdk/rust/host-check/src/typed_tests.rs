//! Production ExtensionProcess admission/decoder with a real Rust SDK executable.
mod progress_model;

use octet_agent::extension_process::{ExtensionEvent, ExtensionRuntimeError};
use octet_agent::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../conformance/typed-values-v1.json")).unwrap()
}
async fn start(workspace: &Path) -> ExtensionProcess {
    let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/examples/typed-probe")
        .canonicalize()
        .expect("build SDK examples; a missing executable is not a skipped test");
    let source = format!("name = \"typed-rust\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = {}\nargs = [{}]\n[contributes]\ntools = [\"typed\"]\n", json!(binary), json!(workspace));
    let manifest_path = workspace.join("extension.toml");
    std::fs::write(&manifest_path, &source).unwrap();
    let descriptor = DiscoveredExtension {
        manifest: ExtensionManifest::parse(&source).unwrap(),
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut config = ExtensionRuntimeConfig::new(workspace);
    config.request_timeout = Duration::from_secs(3);
    config.shutdown_timeout = Duration::from_secs(1);
    config.supervise = false;
    ExtensionProcess::start(descriptor, config).await.unwrap()
}
async fn shutdown(process: &ExtensionProcess, workspace: &Path) {
    let log = std::fs::read_to_string(workspace.join("calls.jsonl")).unwrap();
    let entries: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(entries
        .iter()
        .all(|entry| entry["pid"] == entries[0]["pid"]));
    eprintln!("typed Rust child log: {}", json!(entries));
    assert!(process.shutdown().await);
}
fn record(name: &str) -> Value {
    json!({"name":name,"enabled":true,"samples":[0.5]})
}
fn normalize(mut schema: Value) -> Value {
    if let Some(object) = schema.as_object_mut() {
        for key in ["title", "description", "$schema"] {
            object.remove(key);
        }
        for (key, child) in object {
            *child = normalize(child.take());
            if matches!(key.as_str(), "required" | "anyOf") {
                if let Some(array) = child.as_array_mut() {
                    array.sort_by_key(Value::to_string);
                }
            }
        }
    } else if let Some(array) = schema.as_array_mut() {
        for child in array {
            *child = normalize(child.take());
        }
    }
    schema
}

#[tokio::test]
async fn a01_typed_roundtrip_a04_optional_values_actual_host() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let definition = &process.tool_definitions()[0];
    let fixture = fixture();
    let case = &fixture["cases"][0];
    assert_eq!(
        normalize(definition.parameters.clone()),
        normalize(case["schema"].clone())
    );
    assert_eq!(
        normalize(definition.output_schema.clone().unwrap()),
        normalize(case["schema"].clone())
    );
    for input in case["valid"].as_array().unwrap() {
        let output = process
            .call_tool("typed", input.clone(), process.current_context())
            .await
            .unwrap();
        let mut expected = input.clone();
        for sample in expected["samples"].as_array_mut().unwrap() {
            *sample = json!(sample.as_f64().unwrap());
        }
        expected
            .as_object_mut()
            .unwrap()
            .entry("note")
            .or_insert(Value::Null);
        assert_eq!(output.structured_content, Some(expected));
        assert_eq!(output.content, "typed record");
        assert!(!output.is_error);
    }
    let log = std::fs::read_to_string(workspace.path().join("calls.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 3);
    assert!(log
        .lines()
        .all(|line| serde_json::from_str::<Value>(line).unwrap()["pid"]
            .as_u64()
            .unwrap()
            > 0));
    shutdown(&process, workspace.path()).await;
}

#[tokio::test]
async fn a02_invalid_input_zero_domain_entries_actual_host() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let mut invalid = fixture()["cases"][0]["invalid"].as_array().unwrap().clone();
    invalid.push(json!({"name":"large","enabled":true,"samples":[9_007_199_254_740_992_u64]}));
    for input in invalid {
        let error = process
            .call_tool("typed", input, process.current_context())
            .await
            .unwrap_err();
        assert!(
            matches!(error, ExtensionRuntimeError::Remote { code: -32602, .. }),
            "{error:?}"
        );
        assert!(!workspace.path().join("calls.jsonl").exists());
    }
    assert!(process
        .call_tool("typed", record("healthy"), process.current_context())
        .await
        .is_ok());
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    shutdown(&process, workspace.path()).await;
}

#[tokio::test]
async fn a03_invalid_output_a05_diagnostics_actual_host() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    for name in [
        "invalid-output",
        "missing-output",
        "nonfinite-output",
        "nonportable-output",
        "invalid-diagnostic",
    ] {
        let error = process
            .call_tool("typed", record(name), process.current_context())
            .await
            .unwrap_err();
        assert!(
            matches!(error, ExtensionRuntimeError::Remote { code: -32603, .. }),
            "{error:?}"
        );
    }
    let output = process
        .call_tool("typed", record("diagnostic"), process.current_context())
        .await
        .unwrap();
    assert!(output.is_error);
    assert!(output.structured_content.is_none());
    assert_eq!(
        output.content,
        "Domain failure\nerror[solver.nonconvergent]: Operating point did not converge."
    );
    let diagnostic = &output.metadata["octet_diagnostics_v1"][0];
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["primary"]["source"]["path"], "fixture.cir");
    assert_eq!(diagnostic["fixes"][0]["edits"][0]["replacement"], "0");
    assert_eq!(
        std::fs::read(workspace.path().join("fixture.cir")).unwrap(),
        b"* fixture\nR1 a b 1k\n",
        "fixes are not applied"
    );
    assert!(process
        .call_tool("typed", record("healthy"), process.current_context())
        .await
        .is_ok());
    shutdown(&process, workspace.path()).await;
}

#[tokio::test]
async fn a06_cancellation_a07_progress_actual_host() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    assert!(process.supports_feature("request_progress"));
    let output = process
        .call_tool("typed", record("progress"), process.current_context())
        .await
        .unwrap();
    // Direct calls have no progress sink: progress cannot leak into retained output.
    assert_eq!(output.content, "typed record");
    assert_eq!(output.metadata, Value::Null);
    let generation = process.health_snapshot().generation;
    let mut events = process.subscribe();
    let child = process.clone();
    let call = tokio::spawn(async move {
        child
            .call_tool("typed", record("cancel"), child.current_context())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::read_to_string(workspace.path().join("calls.jsonl"))
                .unwrap_or_default()
                .lines()
                .any(|line| {
                    serde_json::from_str::<Value>(line).is_ok_and(|v| v["name"] == "cancel")
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child entered barrier");
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ExtensionEvent::Diagnostic { message } = events.recv().await.unwrap() {
                if message.contains("ignored late response for cancelled request") {
                    break;
                }
            }
        }
    })
    .await
    .expect("actual host observed the cooperative terminal, no scheduling sleep");
    assert!(process
        .call_tool("typed", record("healthy"), process.current_context())
        .await
        .is_ok());
    assert_eq!(process.health_snapshot().generation, generation);
    assert!(process.is_running());
    shutdown(&process, workspace.path()).await;
}
