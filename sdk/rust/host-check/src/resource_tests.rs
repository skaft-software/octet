//! Production host + SDK resource authoring. No hand-written JSON-RPC child.
mod owner_refusals;

use octet_agent::extension_process::{ResourceRef, ToolCallOutput};
use octet_agent::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

async fn start(workspace: &Path) -> ExtensionProcess {
    let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/examples/resource-probe")
        .canonicalize()
        .expect("build SDK resource-probe; missing executable is a failure");
    let source = format!("name='resource-rust'\nversion='0.1.0'\napi_version='0.4'\n[entrypoint]\ncommand={}\nargs=[{}]\n[entrypoint.env]\nHOME={}\n[contributes]\ntools=['create','add','combine','release_saved']\n", json!(binary), json!(workspace), json!(workspace));
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
    config.request_timeout = Duration::from_secs(10);
    config.shutdown_timeout = Duration::from_secs(2);
    config.cancellation_grace = Duration::from_secs(3);
    config.supervise = false;
    ExtensionProcess::start(descriptor, config).await.unwrap()
}
async fn call(
    process: &ExtensionProcess,
    owner: &str,
    name: &str,
    args: Value,
) -> Result<ToolCallOutput, octet_agent::extension_process::ExtensionRuntimeError> {
    process
        .call_tool(
            name,
            args,
            process.current_context_for_resource_owner(owner),
        )
        .await
}
async fn create(process: &ExtensionProcess, name: &str) -> ResourceRef {
    let result = call(process, "A", "create", json!({"name":name}))
        .await
        .unwrap();
    serde_json::from_value(result.structured_content.unwrap()["counter"].clone()).unwrap()
}
fn events(workspace: &Path) -> Vec<Value> {
    std::fs::read_to_string(workspace.join("resources.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
async fn event(workspace: &Path, kind: &str, name: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if events(workspace)
                .iter()
                .any(|e| e["kind"] == kind && e["name"] == name)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing {kind}/{name}: {:?}", events(workspace)));
}

#[tokio::test]
async fn resources_and_bulk_checked_in_author_examples_run_in_the_real_host() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples/extensions/native-hello");
    for (directory, create, use_tool) in [
        ("resources", "counter_create", "counter_add"),
        ("blobs", "blob_save", "blob_size"),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let manifest_path = root
            .join(directory)
            .join("extension.toml")
            .canonicalize()
            .unwrap();
        let descriptor = DiscoveredExtension {
            manifest: ExtensionManifest::load(&manifest_path).unwrap(),
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let mut config = ExtensionRuntimeConfig::new(workspace.path());
        config.bulk_store = Some(octet_agent::BulkStorage::new().unwrap());
        config.supervise = false;
        let process = ExtensionProcess::start(descriptor, config).await.unwrap();
        let args = if directory == "resources" {
            json!({"initial":2})
        } else {
            json!({})
        };
        let mut output = call(&process, "A", create, args)
            .await
            .unwrap()
            .structured_content
            .unwrap();
        let expected = if directory == "resources" {
            json!(7)
        } else {
            output["data"]["bytes"].clone()
        };
        if directory == "resources" {
            output["amount"] = 5.into();
        }
        let result = call(&process, "A", use_tool, output)
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(
            result[if directory == "resources" {
                "value"
            } else {
                "bytes"
            }],
            expected
        );
        assert!(process.shutdown().await);
    }
}

#[tokio::test]
async fn resources_native_state_nominal_metadata_and_zero_entry_refusals() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    assert!(process.supports_feature("resource_refs_v1"));
    assert!(process.supports_feature("operation_descriptors_v1"));
    let definitions = process.tool_definitions();
    let add = definitions.iter().find(|t| t.name == "add").unwrap();
    assert_eq!(
        add.operation.as_ref().unwrap().receiver.as_deref(),
        Some("/counter")
    );
    assert_eq!(
        add.parameters["properties"]["counter"]["properties"]["type"]["enum"],
        json!(["fixture.Counter"])
    );
    let reference = create(&process, "ordinary").await;
    assert!(process.lookup_resource("A", &reference).is_ok());
    assert!(process.lookup_resource("B", &reference).is_err());
    let output = call(&process, "A", "add", json!({"counter":reference,"delta":7}))
        .await
        .unwrap();
    assert_eq!(output.structured_content, Some(json!({"value":7})));
    let output = call(
        &process,
        "A",
        "combine",
        json!({"first":reference,"second":{"counter":reference}}),
    )
    .await
    .unwrap();
    assert_eq!(output.structured_content, Some(json!({"value":14})));
    let before = events(workspace.path()).len();
    let mut forged = reference.clone();
    forged.resource = "forged".into();
    assert!(call(
        &process,
        "A",
        "combine",
        json!({"first":reference,"second":{"counter":forged}})
    )
    .await
    .is_err());
    assert!(
        call(&process, "B", "add", json!({"counter":reference,"delta":1}))
            .await
            .is_err()
    );
    let mut wrong = reference.clone();
    wrong.resource_type = "other.Counter".into();
    assert!(
        call(&process, "A", "add", json!({"counter":wrong,"delta":1}))
            .await
            .is_err()
    );
    assert_eq!(events(workspace.path()).len(), before);
    assert!(
        call(
            &process,
            "A",
            "add",
            json!({"counter":reference,"delta":1,"mode":"release"})
        )
        .await
        .is_err(),
        "a pinned input cannot be released"
    );
    assert!(process.lookup_resource("A", &reference).is_ok());
    let output = call(&process, "A", "release_saved", json!({}))
        .await
        .unwrap();
    assert_eq!(output.structured_content, Some(json!({"value":1})));
    assert!(process.lookup_resource("A", &reference).is_err());
    event(workspace.path(), "dispose", "ordinary").await;
    let events = events(workspace.path());
    assert!(events.iter().all(|e| e["pid"] == events[0]["pid"]));
    assert!(events
        .iter()
        .filter(|e| e["kind"] == "dispose")
        .all(|e| e["thread"] == "octet-native-dispose"));
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn resources_failed_outputs_and_cancelled_creation_clean_provisional_values() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    assert!(
        call(&process, "A", "create", json!({"name":"invalid-output"}))
            .await
            .is_err()
    );
    event(workspace.path(), "dispose", "invalid-output").await;
    assert!(
        call(&process, "A", "create", json!({"name":"error"}))
            .await
            .unwrap()
            .is_error
    );
    event(workspace.path(), "dispose", "error").await;
    let child = process.clone();
    let pending =
        tokio::spawn(async move { call(&child, "A", "create", json!({"name":"cancel"})).await });
    event(workspace.path(), "exported", "cancel").await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    event(workspace.path(), "dispose", "cancel").await;
    let reference = create(&process, "healthy").await;
    process.release_resource("A", &reference).unwrap();
    event(workspace.path(), "dispose", "healthy").await;
    assert!(process.is_running());
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn resources_retirement_cancellation_preserve_native_pin_until_settlement() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    let reference = create(&process, "pinned").await;
    let child = process.clone();
    let input = reference.clone();
    let pending = tokio::spawn(async move {
        call(
            &child,
            "A",
            "add",
            json!({"counter":input,"delta":1,"mode":"cancel"}),
        )
        .await
    });
    event(workspace.path(), "holding", "add").await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    process.retire_resource_owner("A");
    assert!(process.lookup_resource("A", &reference).is_err());
    assert!(!events(workspace.path())
        .iter()
        .any(|e| e["kind"] == "dispose"));
    std::fs::write(workspace.path().join("settle"), b"").unwrap();
    event(workspace.path(), "dispose", "pinned").await;
    let log = events(workspace.path());
    assert!(
        log.iter().position(|e| e["kind"] == "settled").unwrap()
            < log.iter().position(|e| e["kind"] == "dispose").unwrap()
    );
    assert!(process.is_running());
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn resources_cleanup_failure_never_resurrects_native_or_host_identity() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path()).await;
    for mode in ["fail-dispose", "panic-dispose"] {
        let reference = create(&process, mode).await;
        assert!(process.release_resource("A", &reference).unwrap().retired);
        assert!(process.lookup_resource("A", &reference).is_err());
        event(workspace.path(), "dispose", mode).await;
    }
    assert!(process.is_running());
    assert!(process.shutdown().await);
    assert_eq!(
        events(workspace.path())
            .iter()
            .filter(|e| e["kind"] == "dispose")
            .count(),
        2
    );
}
