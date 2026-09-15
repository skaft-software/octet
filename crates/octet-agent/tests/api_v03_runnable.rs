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
    let manifest_path = repository.join(EXTENSION_MANIFEST_FILENAME);
    let manifest = ExtensionManifest::load(&manifest_path).expect("minimal API 0.3 manifest");
    assert_eq!(manifest.api_version, "0.3");
    assert_eq!(manifest.requires_octet.as_deref(), Some("=0.7.6"));

    let workspace = TempDir::new().expect("workspace");
    let mut config = ExtensionRuntimeConfig::new(workspace.path());
    // Shutdown must cancel this delayed request before the request deadline;
    // otherwise the two independent timers can report a timeout instead.
    config.request_timeout = Duration::from_secs(3);
    config.shutdown_timeout = Duration::from_secs(1);
    let process = ExtensionProcess::start(trusted_descriptor(manifest_path, manifest), config)
        .await
        .expect("start the released-install API 0.3 example");

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
