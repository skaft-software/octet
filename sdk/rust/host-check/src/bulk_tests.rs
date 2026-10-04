//! Actual SDK executables through production bulk admission and file transport.
use octet_agent::extension_process::{ExtensionEvent, ResourceRef, ToolCallOutput};
use octet_agent::{
    BlobRef, BulkLimits, BulkStorage, DiscoveredExtension, ExtensionActivation, ExtensionManifest,
    ExtensionProcess, ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

async fn start(workspace: &Path, storage: &BulkStorage) -> ExtensionProcess {
    let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/examples/bulk-probe")
        .canonicalize()
        .expect("build SDK bulk-probe; missing executable is a failure");
    let source = format!("name='bulk-rust'\nversion='0.1.0'\napi_version='0.4'\n[entrypoint]\ncommand={}\nargs=[{}]\n[entrypoint.env]\nHOME={}\n[contributes]\ntools=['write','read','joint']\n", json!(binary), json!(workspace), json!(workspace));
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
    config.bulk_store = Some(storage.clone());
    config.request_timeout = Duration::from_secs(10);
    config.shutdown_timeout = Duration::from_secs(2);
    config.cancellation_grace = Duration::from_secs(3);
    config.supervise = false;
    ExtensionProcess::start(descriptor, config).await.unwrap()
}
async fn call(
    process: &ExtensionProcess,
    session: &str,
    name: &str,
    args: Value,
) -> Result<ToolCallOutput, octet_agent::extension_process::ExtensionRuntimeError> {
    process
        .call_tool(
            name,
            args,
            process.current_context_for_resource_owner(session),
        )
        .await
}
async fn write(process: &ExtensionProcess, bytes: u64) -> BlobRef {
    let output = call(process, "A", "write", json!({"bytes":bytes}))
        .await
        .unwrap();
    assert!(!output.content.contains("octet-transfer-"));
    assert_eq!(output.metadata, Value::Null);
    serde_json::from_value(output.structured_content.unwrap()["data"].clone()).unwrap()
}
fn events(workspace: &Path) -> Vec<Value> {
    std::fs::read_to_string(workspace.join("bulk.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
async fn wait_events(workspace: &Path, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while events(workspace).len() < count {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("SDK process barrier");
}

#[tokio::test]
async fn bulk_sdk_large_file_cross_process_read_exact_metadata_and_retention_release() {
    let storage = BulkStorage::new().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let producer = start(a.path(), &storage).await;
    let consumer = start(b.path(), &storage).await;
    assert!(producer.supports_feature("bulk_objects_v1"));
    let bytes = 3 * 1024 * 1024 + 17;
    let reference = write(&producer, bytes).await;
    assert_eq!(reference.bytes, bytes);
    assert_eq!(reference.digest.algorithm, "sha256");
    assert_eq!(reference.digest.value.len(), 64);
    let output = call(&consumer, "A", "read", json!({"data":reference}))
        .await
        .unwrap();
    assert_eq!(output.structured_content, Some(json!({"bytes":bytes})));
    let before = events(b.path()).len();
    assert!(call(&consumer, "B", "read", json!({"data":reference}))
        .await
        .is_err());
    let mut changed = reference.clone();
    changed.bytes -= 1;
    assert!(call(&consumer, "A", "read", json!({"data":changed}))
        .await
        .is_err());
    changed = reference.clone();
    changed.digest.value = "0".repeat(64);
    assert!(call(&consumer, "A", "read", json!({"data":changed}))
        .await
        .is_err());
    assert_eq!(
        events(b.path()).len(),
        before,
        "host denies before SDK domain entry"
    );
    assert_ne!(events(a.path())[0]["pid"], events(b.path())[0]["pid"]);
    assert!(producer.shutdown().await);
    assert!(
        call(&consumer, "A", "read", json!({"data":reference}))
            .await
            .is_ok(),
        "producer generation retirement preserves admitted blob retention"
    );
    storage.release_blob("A", &reference).unwrap();
    let before = events(b.path()).len();
    assert!(call(&consumer, "A", "read", json!({"data":reference}))
        .await
        .is_err());
    assert_eq!(events(b.path()).len(), before);
    assert!(consumer.shutdown().await);
}

#[tokio::test]
async fn bulk_sdk_failed_and_cancelled_results_never_admit_provisional_blobs() {
    let storage = BulkStorage::new().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path(), &storage).await;
    for mode in ["error", "invalid-output"] {
        let result = call(&process, "A", "write", json!({"bytes":64,"mode":mode})).await;
        if mode == "error" {
            assert!(result.unwrap().is_error);
        } else {
            assert!(result.is_err());
        }
        let log = events(workspace.path());
        let blob = log.last().unwrap()["blob"].clone();
        assert!(call(&process, "A", "read", json!({"data":blob}))
            .await
            .is_err());
        assert_eq!(events(workspace.path()).len(), log.len());
    }
    let before = events(workspace.path()).len();
    let mut telemetry = process.subscribe();
    let child = process.clone();
    let pending = tokio::spawn(async move {
        call(&child, "A", "write", json!({"bytes":64,"mode":"cancel"})).await
    });
    wait_events(workspace.path(), before + 2).await;
    let blob = events(workspace.path()).last().unwrap()["blob"].clone();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ExtensionEvent::Diagnostic { message } = telemetry.recv().await.unwrap() {
                if message.contains("ignored late response for cancelled request") {
                    break;
                }
            }
        }
    })
    .await
    .expect("actual host observed SDK cancellation settlement");
    assert!(call(&process, "A", "read", json!({"data":blob}))
        .await
        .is_err());
    assert_eq!(events(workspace.path()).len(), before + 2);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn bulk_sdk_joint_resource_and_blob_have_one_success_disposition() {
    let storage = BulkStorage::new().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let process = start(workspace.path(), &storage).await;
    let output = call(&process, "A", "joint", json!({"bytes":128}))
        .await
        .unwrap()
        .structured_content
        .unwrap();
    let native: ResourceRef = serde_json::from_value(output["native"].clone()).unwrap();
    let blob: BlobRef = serde_json::from_value(output["data"].clone()).unwrap();
    assert!(process.lookup_resource("A", &native).is_ok());
    assert!(call(&process, "A", "read", json!({"data":blob}))
        .await
        .is_ok());
    let before = events(workspace.path()).len();
    assert!(
        call(&process, "A", "joint", json!({"bytes":128,"mode":"error"}))
            .await
            .unwrap()
            .is_error
    );
    wait_events(workspace.path(), before + 3).await; // call, joint refs, native disposal
    let log = events(workspace.path());
    let failed = log.iter().rev().find(|e| e["kind"] == "joint").unwrap();
    let failed_native: ResourceRef = serde_json::from_value(failed["native"].clone()).unwrap();
    assert!(process.lookup_resource("A", &failed_native).is_err());
    assert!(call(&process, "A", "read", json!({"data":failed["blob"]}))
        .await
        .is_err());
    assert_eq!(events(workspace.path()).len(), log.len());
    assert!(process.lookup_resource("A", &native).is_ok());
    process.release_resource("A", &native).unwrap();
    storage.release_blob("A", &blob).unwrap();
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn bulk_sdk_durable_reopen_needs_explicit_host_reauthorization_and_fresh_lease() {
    let root = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let storage =
        BulkStorage::with_root_and_limits(root.path().join("store"), BulkLimits::default())
            .unwrap();
    let producer = start(a.path(), &storage).await;
    let reference = write(&producer, 32).await;
    storage.retain_durable("A", &reference).await.unwrap();
    assert!(producer.shutdown().await);
    drop(producer);
    drop(storage);
    let storage =
        BulkStorage::with_root_and_limits(root.path().join("store"), BulkLimits::default())
            .unwrap();
    let consumer = start(b.path(), &storage).await;
    assert!(call(&consumer, "A", "read", json!({"data":reference}))
        .await
        .is_err());
    assert!(events(b.path()).is_empty());
    assert!(storage.recover_durable("B", &reference).await.is_err());
    storage.recover_durable("A", &reference).await.unwrap();
    assert_eq!(
        call(&consumer, "A", "read", json!({"data":reference}))
            .await
            .unwrap()
            .structured_content,
        Some(json!({"bytes":32}))
    );
    storage.release_durable("A", &reference).unwrap();
    storage.release_blob("A", &reference).unwrap();
    assert!(consumer.shutdown().await);
}
