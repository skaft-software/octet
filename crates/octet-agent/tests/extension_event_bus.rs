#![cfg(unix)]
#![allow(missing_docs)]

use octet_agent::extension_process::ExtensionEventBus;
use octet_agent::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust, EXTENSION_MANIFEST_FILENAME,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

async fn start(name: &str, bus: Option<Arc<ExtensionEventBus>>) -> (TempDir, ExtensionProcess) {
    let directory = TempDir::new().unwrap();
    let manifest_path = directory.path().join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(
        &manifest_path,
        format!(
            r#"name = "{name}"
version = "0.1.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = ["peer.py"]
[contributes]
tools = ["probe"]
"#
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("peer.py"),
        include_str!("support/extension_bus_peer.py")
            .replace("__EXTENSION_NAME__", name)
            .replace("__SDK_PATH__", &format!("{}/../../sdk/python", env!("CARGO_MANIFEST_DIR"))),
    )
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
    let mut config = ExtensionRuntimeConfig::new(directory.path());
    config.event_bus = bus;
    config.request_timeout = Duration::from_secs(3);
    config.shutdown_timeout = Duration::from_secs(1);
    config.supervise = false;
    let process = ExtensionProcess::start(descriptor, config).await.unwrap();
    (directory, process)
}
async fn call(process: &ExtensionProcess, method: &str, params: Value) -> Value {
    process
        .call_tool(
            "probe",
            json!({"method":method,"params":params}),
            process.current_context(),
        )
        .await
        .unwrap()
        .structured_content
        .unwrap()
}
fn declaration() -> Value {
    json!({"topic":"bus.alpha.status", "fields":[{"name":"summary", "kind":"string", "required":true, "max_bytes":128, "values":[]}]})
}
async fn events(process: &ExtensionProcess, count: usize) -> Value {
    process
        .call_tool(
            "probe",
            json!({"method":"events","count":count}),
            process.current_context(),
        )
        .await
        .unwrap()
        .structured_content
        .unwrap()
}

#[tokio::test]
async fn two_process_bus_delivers_only_validated_subscriptions_and_fences_reload() {
    let bus = Arc::new(ExtensionEventBus::default());
    let (_alpha_dir, alpha) = start("alpha", Some(bus.clone())).await;
    let (_beta_dir, beta) = start("beta", Some(bus.clone())).await;
    assert!(alpha.negotiated_features().contains("event_bus"));
    let ack = call(&alpha, "bus/declare", declaration()).await;
    let binding = ack["result"]["binding_id"].as_str().unwrap().to_owned();
    assert!(!binding.is_empty(), "{ack}");
    // An existing topic subscriber is admitted as *active* with the host's
    // publisher instance/generation, not as a bare success.
    let admitted = call(&beta, "bus/subscribe", json!({"topic":"bus.alpha.status"})).await;
    assert_eq!(admitted["result"]["state"], "active", "{admitted}");
    assert_eq!(admitted["result"]["binding_id"], binding.as_str());
    // The publisher instance id is a host-issued opaque identity, never the
    // extension-authored name, and must be stable for one incarnation.
    let publisher_instance = admitted["result"]["publisher_instance_id"]
        .as_str()
        .expect("host-issued publisher instance")
        .to_owned();
    assert!(!publisher_instance.is_empty());
    assert_ne!(publisher_instance, "alpha");
    assert_eq!(admitted["result"]["process_generation"], 1);
    for (process, params, code) in [
        (
            &beta,
            json!({"topic":"bus.alpha.status","payload":{"summary":"foreign"}}),
            -32011,
        ),
        (
            &alpha,
            json!({"topic":"bus.alpha.unknown","payload":{"summary":"unknown"}}),
            -32602,
        ),
        (
            &alpha,
            json!({"topic":"bus.alpha.status","payload":{"summary":"user@example.com"}}),
            -32602,
        ),
        (
            &alpha,
            json!({"topic":"bus.alpha.status","payload":{"summary":"safe","token":"private"}}),
            -32602,
        ),
        (
            &alpha,
            json!({"topic":"bus.alpha.status","publisher":"beta","payload":{"summary":"spoof"}}),
            -32602,
        ),
    ] {
        let response = call(process, "bus/publish", params).await;
        assert_eq!(response["error"]["code"], code);
        octet_agent::extension_api_v03::parse_json_rpc_envelope(response).unwrap();
    }
    assert_eq!(events(&beta, 0).await, json!([]));
    for expected in 1..=2 {
        assert_eq!(
            call(
                &alpha,
                "bus/publish",
                json!({"topic":"bus.alpha.status","payload":{"summary":"ready"}})
            )
            .await["result"]["sequence"],
            expected
        );
    }
    let delivered = events(&beta, 2).await;
    assert_eq!(delivered.as_array().unwrap().len(), 2);
    assert_eq!(delivered[0]["publisher"], "alpha");
    assert_eq!(delivered[0]["process_generation"], 1);
    assert_eq!(delivered[1]["sequence"], 2);
    assert_eq!(delivered[0]["payload"], json!({"summary":"ready"}));
    assert_eq!(events(&alpha, 0).await, json!([]));
    assert_eq!(
        call(
            &beta,
            "bus/unsubscribe",
            json!({"topic":"bus.alpha.status"})
        )
        .await["result"]["binding_id"],
        binding.as_str()
    );
    call(
        &alpha,
        "bus/publish",
        json!({"topic":"bus.alpha.status","payload":{"summary":"unsubscribed"}}),
    )
    .await;
    assert_eq!(events(&beta, 0).await.as_array().unwrap().len(), 2);

    alpha.reload().await.unwrap();
    // The replacement publisher has not declared the topic yet, so the earlier
    // subscription was invalidated. A subscribe now admits a bounded *pending*
    // interest; it is not an active subscription and delivers nothing.
    let pending = call(&beta, "bus/subscribe", json!({"topic":"bus.alpha.status"})).await;
    assert_eq!(pending["result"]["state"], "pending", "{pending}");
    call(&alpha, "bus/declare", declaration()).await;
    let rebound = call(&beta, "bus/subscribe", json!({"topic":"bus.alpha.status"})).await;
    assert_eq!(rebound["result"]["state"], "active", "{rebound}");
    assert_eq!(rebound["result"]["process_generation"], 2);
    // A reload keeps the process identity and advances its generation; the two
    // together are what fences stale deliveries.
    assert_eq!(
        rebound["result"]["publisher_instance_id"].as_str().unwrap(),
        publisher_instance
    );
    call(
        &alpha,
        "bus/publish",
        json!({"topic":"bus.alpha.status","payload":{"summary":"new generation"}}),
    )
    .await;
    let delivered = events(&beta, 3).await;
    assert_eq!(delivered[2]["process_generation"], 2);
    assert_eq!(delivered[2]["sequence"], 1);
    bus.reset();
    let stale = call(
        &alpha,
        "bus/publish",
        json!({"topic":"bus.alpha.status","payload":{"summary":"old binding"}}),
    )
    .await;
    // Both canonical refusals preserve the invariant: a replacement incarnation
    // never accepts the old scope. -32011 means the peer still carried the old
    // binding; -32602 means it already observed the reset, which also cleared
    // the topic. Either way nothing reaches a subscriber.
    let code = stale["error"]["code"].as_i64().expect("refusal");
    assert!(matches!(code, -32011 | -32602), "{stale}");
    // The refusal delivers nothing: the subscriber still holds exactly the
    // three events from the two accepted publications plus the new generation.
    let after_reset = events(&beta, 3).await;
    assert_eq!(after_reset.as_array().unwrap().len(), 3);
    assert_eq!(after_reset[2]["payload"], json!({"summary":"new generation"}));
    assert!(alpha.shutdown().await);
    assert!(beta.shutdown().await);
}

#[tokio::test]
async fn bus_offer_requires_service_and_separate_sessions_never_share_topics() {
    let (_alpha_dir, alpha) = start("alpha", Some(Arc::new(ExtensionEventBus::default()))).await;
    let (_beta_dir, beta) = start("beta", Some(Arc::new(ExtensionEventBus::default()))).await;
    let (_none_dir, none) = start("none", None).await;
    call(&alpha, "bus/declare", declaration()).await;
    // beta is bound to a different bus, so alpha's declaration is invisible
    // there. That yields a bounded pending interest that never becomes active
    // and never delivers another session's event.
    let other = call(&beta, "bus/subscribe", json!({"topic":"bus.alpha.status"})).await;
    assert_eq!(other["result"]["state"], "pending", "{other}");
    assert!(other["result"]["topic_revision"].is_u64(), "{other}");
    assert!(events(&beta, 0).await.as_array().unwrap().is_empty());
    assert!(!none.negotiated_features().contains("event_bus"));
    assert_eq!(
        call(&none, "bus/subscribe", json!({"topic":"bus.alpha.status"})).await["error"]["code"],
        -32601
    );
    assert!(alpha.shutdown().await);
    assert!(beta.shutdown().await);
    assert!(none.shutdown().await);
}
