//! Provider-free qualification against the source Rust host, not a wire recorder.
use std::path::PathBuf;
use std::time::Duration;

use octet_agent::{
    discover_extension_manifests, ExtensionCatalog, ExtensionPolicy, ExtensionProcess,
    ExtensionRoot, ExtensionRuntimeConfig, ExtensionRuntimeError, ExtensionSource,
};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository");
    let root = repository.join("examples/extensions");
    let (inputs, _) = discover_extension_manifests(&[ExtensionRoot {
        directory: root,
        source: ExtensionSource::Explicit,
    }]);
    let input = inputs
        .into_iter()
        .find(|item| {
            item.path
                .parent()
                .and_then(|parent| parent.file_name())
                .is_some_and(|name| name == "typescript-hello")
        })
        .expect("tooling-generated example manifest discovered");
    let mut config = ExtensionRuntimeConfig::new(&repository);
    config.request_timeout = Duration::from_secs(3);
    config.cancellation_grace = Duration::from_millis(300);
    config.supervise = false; // A restarted process cannot disguise failed cancellation.
    let off =
        ExtensionCatalog::load_resolved([input.clone()], &ExtensionPolicy::default(), 256 * 1024);
    assert!(off.diagnostics.is_empty(), "{:?}", off.diagnostics);
    let descriptor = off
        .extensions
        .into_iter()
        .next()
        .expect("discovered descriptor");
    assert!(!descriptor.activation.enabled);
    assert!(matches!(
        ExtensionProcess::start(descriptor, config.clone()).await,
        Err(ExtensionRuntimeError::Disabled(_))
    ));
    println!("PASS discovery does not start or implicitly enable an extension");

    let mut policy = ExtensionPolicy::default();
    policy.enable("typescript-hello");
    let enabled = ExtensionCatalog::load_resolved([input], &policy, 256 * 1024);
    assert!(enabled.diagnostics.is_empty());
    let descriptor = enabled
        .extensions
        .into_iter()
        .next()
        .expect("enabled descriptor");
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("real API 0.4 TypeScript process accepted by host");
    assert_eq!(process.api_version(), "0.4");
    assert_eq!(process.contributions().tools.len(), 1);
    assert!(process.negotiated_features().contains("request_progress"));
    assert!(!process.negotiated_features().contains("dynamic_tools"));
    println!("PASS explicit enablement and exact feature negotiation");
    let output = process
        .call_tool(
            "text_stats",
            json!({"text": "hello world\n🦀"}),
            process.current_context(),
        )
        .await
        .expect("real useful tool call");
    assert_eq!(
        output.content,
        "characters=13 words=3 lines=2 utf8_bytes=16"
    );
    assert!(!output.is_error);
    println!("PASS useful tool: {}", output.content);
    let invalid = process
        .call_tool("text_stats", json!({"text": 7}), process.current_context())
        .await;
    assert!(matches!(
        invalid,
        Err(ExtensionRuntimeError::Remote { code: -32602, .. })
    ));
    println!("PASS malformed tool arguments rejected at process boundary");

    let generation = process.health_snapshot().generation;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(100),
        process.call_tool(
            "text_stats",
            json!({"text": "cancel me", "delayMs": 2000}),
            process.current_context(),
        ),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "host waiter must be dropped while real work is in flight"
    );
    // Drop triggers the host's cancellation notification. The SDK must settle, not stall
    // until the 2-second work delay completes. This smoke tightens the host's
    // cancellation grace to 300 ms, with supervision disabled.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(process.health_snapshot().generation, generation);
    let output = process
        .call_tool(
            "text_stats",
            json!({"text": "still ready"}),
            process.current_context(),
        )
        .await
        .expect("generation remains usable after cooperative cancellation");
    assert_eq!(
        output.content,
        "characters=11 words=2 lines=1 utf8_bytes=11"
    );
    println!("PASS cooperative cancellation and same-generation subsequent call");
    assert!(
        process.shutdown().await,
        "real host shutdown must be acknowledged and exit cleanly"
    );
    println!("PASS host-qualified shutdown (no providers/credentials/network)");
}
