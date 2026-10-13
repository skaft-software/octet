//! Runnable, staged one-source author recipes through the production host.
use octet_agent::{
    BulkStorage, DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};
use serde_json::{json, Value};
use std::path::Path;

async fn start(recipe: &str, workspace: &Path) -> ExtensionProcess {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../examples/extensions/native-hello")
        .join(recipe)
        .join("extension.toml")
        .canonicalize()
        .unwrap();
    let mut manifest = ExtensionManifest::load(&manifest_path).unwrap();
    manifest
        .entrypoint
        .env
        .insert("HOME".into(), workspace.to_string_lossy().into_owned());
    let descriptor = DiscoveredExtension {
        manifest,
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut config = ExtensionRuntimeConfig::new(workspace);
    if recipe == "blobs" {
        config.bulk_store = Some(BulkStorage::new().unwrap());
    }
    config.supervise = false;
    ExtensionProcess::start(descriptor, config)
        .await
        .expect("build/stage checked-in author recipe; no missing-binary skip")
}
async fn call(
    process: &ExtensionProcess,
    name: &str,
    args: Value,
) -> octet_agent::extension_process::ToolCallOutput {
    process
        .call_tool(
            name,
            args,
            process.current_context_for_resource_owner("author-example"),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn typed_recipe_generates_output_and_structured_domain_diagnostic() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start("typed", workspace.path()).await;
    assert!(!process.supports_feature("resource_refs_v1"));
    assert!(!process.supports_feature("bulk_objects_v1"));
    let output = call(
        &process,
        "summarize",
        json!({"label":"voltage","samples":[1.0,3.0]}),
    )
    .await;
    assert_eq!(output.content, "Sample summary ready");
    assert_eq!(
        output.structured_content,
        Some(json!({"label":"voltage","count":2,"mean":2.0,"unit":null}))
    );
    let output = call(&process, "summarize", json!({"label":"empty","samples":[]})).await;
    assert!(output.is_error);
    assert!(output.structured_content.is_none());
    assert_eq!(
        output.metadata["octet_diagnostics_v1"][0]["code"],
        "samples.empty"
    );
    assert!(process.shutdown().await);
}
#[tokio::test]
async fn resource_recipe_mutates_releases_and_refuses_old_identity() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start("resources", workspace.path()).await;
    let created = call(&process, "counter_create", json!({"initial":7}))
        .await
        .structured_content
        .unwrap();
    let reference = created["counter"].clone();
    assert_eq!(
        call(
            &process,
            "counter_add",
            json!({"counter":reference,"amount":5})
        )
        .await
        .structured_content,
        Some(json!({"value":12}))
    );
    let released = call(&process, "counter_release_last", json!({}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(released["retired"], true);
    assert!(matches!(
        released["cleanup"].as_str(),
        Some("pending" | "completed")
    ));
    let error = process
        .call_tool(
            "counter_add",
            json!({"counter":reference,"amount":1}),
            process.current_context_for_resource_owner("author-example"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, octet_agent::extension_process::ExtensionRuntimeError::Remote { code:-32000, ref message, .. } if message == "resource_unavailable")
    );
    let fresh = call(&process, "counter_create", json!({"initial":1}))
        .await
        .structured_content
        .unwrap();
    assert_ne!(fresh["counter"], reference);
    assert_eq!(
        call(
            &process,
            "counter_add",
            json!({"counter":fresh["counter"],"amount":2})
        )
        .await
        .structured_content,
        Some(json!({"value":3}))
    );
    assert!(process.shutdown().await);
}
#[tokio::test]
async fn blob_recipe_reads_exact_bytes_with_fresh_scoped_leases() {
    let workspace = tempfile::tempdir().unwrap();
    let process = start("blobs", workspace.path()).await;
    let saved = call(&process, "blob_save", json!({}))
        .await
        .structured_content
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            call(&process, "blob_size", saved.clone())
                .await
                .structured_content,
            Some(json!({"bytes":saved["data"]["bytes"]}))
        );
    }
    assert!(process.shutdown().await);
}
