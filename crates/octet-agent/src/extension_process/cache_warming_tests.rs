use super::*;
use crate::cache_warmer::{CacheWarmingDecision, CacheWarmingPhase};
use octet_ai::ModelId;
use pretty_assertions::assert_eq;

fn manifest() -> ExtensionManifest {
    ExtensionManifest::parse(
        r#"name = "cache-warming"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "extension.py"
[contributes]
hooks = ["cache_warming_decision"]
"#,
    )
    .unwrap()
}

fn context(owner: &str) -> CacheWarmingDecisionContext {
    CacheWarmingDecisionContext {
        decision: CacheWarmingDecision {
            phase: CacheWarmingPhase::Idle,
            warm_cost_microdollars: 100,
            miss_cost_microdollars: 1_000,
            continuation_probability: 0.15,
            expected_savings_microdollars: 50,
            economics_available: true,
            action: CacheWarmingAction::Warm,
        },
        model: ModelId("cache-model".into()),
        resource_owner: owner.into(),
    }
}

#[test]
fn cache_warming_manifest_and_negotiation_are_api_04_declared_only() {
    let declared = manifest();
    for version in ["0.1", "0.2", "0.3"] {
        let mut legacy = declared.clone();
        legacy.api_version = version.into();
        assert!(matches!(
            legacy.validate(),
            Err(ExtensionRuntimeError::InvalidManifest(message))
                if message.contains("cache_warming_decision requires extension API 0.4")
        ));
    }
    let response = |selected: bool, version: &str| -> InitializeResponse {
        let mut features = API_0_2_REQUIRED_FEATURES.to_vec();
        if selected {
            features.push(EXTENSION_FEATURE_CACHE_WARMING_DECISION);
        }
        serde_json::from_value(serde_json::json!({
            "api_version": version,
            "protocol": {
                "version": version, "features": features,
                "limits": {"max_concurrent_requests": 1},
            },
        }))
        .unwrap()
    };
    let negotiate = |manifest: &ExtensionManifest, response| {
        negotiate_contributions_with_host_services(
            manifest,
            response,
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        )
    };
    assert!(negotiate(&declared, response(true, "0.4")).is_ok());
    assert!(matches!(
        negotiate(&declared, response(false, "0.4")),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("requires negotiated")
    ));
    let mut undeclared = declared.clone();
    undeclared.contributes.hooks.clear();
    for version in ["0.2", "0.4"] {
        undeclared.api_version = version.into();
        assert!(matches!(
            negotiate(&undeclared, response(true, version)),
            Err(ExtensionRuntimeError::Protocol(message)) if message.contains("unknown feature")
        ));
    }
}

#[test]
fn cache_warming_wire_actions_are_typed_and_no_opinion_is_optional() {
    for (wire, expected) in [
        (
            serde_json::json!({"cache_warming_decision": "warm"}),
            Some(CacheWarmingAction::Warm),
        ),
        (
            serde_json::json!({"cache_warming_decision": "stop"}),
            Some(CacheWarmingAction::Stop),
        ),
        (serde_json::json!({"cache_warming_decision": null}), None),
        (serde_json::json!({}), None),
    ] {
        let result: ExtensionHookOutput = serde_json::from_value(wire).unwrap();
        assert_eq!(result.cache_warming_decision, expected);
    }
    for action in [
        serde_json::json!("retry"),
        serde_json::json!(true),
        serde_json::json!({"action": "warm"}),
        serde_json::json!(["warm"]),
    ] {
        assert!(serde_json::from_value::<ExtensionHookOutput>(
            serde_json::json!({"cache_warming_decision": action})
        )
        .is_err());
    }
    assert!(serde_json::from_value::<ExtensionHookOutput>(
        serde_json::json!({"cache_warming_decision": "warm", "deadline_ms": 60_000})
    )
    .is_err());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn cache_warming_process_preserves_owner_and_falls_back_on_bad_advice() {
    let temp = TempDir::new().unwrap();
    write_executable_script(
        &temp.path().join("extension.py"),
        r#"#!/usr/bin/env python3
import json
import sys


def receive():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


init = receive()
protocol = init["params"]["protocol"]
assert init["params"]["api_version"] == "0.4"
assert init["params"]["contributes"]["hooks"] == ["cache_warming_decision"]
assert "cache_warming_decision" in protocol["optional_features"]
send({"jsonrpc": "2.0", "id": init["id"], "result": {
    "api_version": "0.4", "tools": [], "commands": [],
    "protocol": {"version": "0.4", "features":
        protocol["required_features"] + ["cache_warming_decision"],
        "limits": {"max_concurrent_requests": 1}},
}})
fence = None
for index, response in enumerate([
    {"cache_warming_decision": "warm"}, {"cache_warming_decision": "stop"},
    {}, {"cache_warming_decision": None}, {"cache_warming_decision": "secret-invalid-action"},
    None,
]):
    request = receive()
    assert request["method"] == "hook/run"
    params = request["params"]
    assert params["hook"] == "cache_warming_decision"
    assert params["payload"] == {"model": "cache-model", "decision": {
        "phase": "idle", "warm_cost_microdollars": 100, "miss_cost_microdollars": 1000,
        "continuation_probability": 0.15, "expected_savings_microdollars": 50,
        "economics_available": True, "action": "warm",
    }}
    owner = params["context"]["resource_owner"]
    assert owner["session_id"] == ("owner-one" if index % 2 == 0 else "owner-two")
    assert owner["process_generation"] == 1
    assert owner["extension_instance_id"]
    if fence is not None:
        assert owner["extension_instance_id"] == fence
    fence = owner["extension_instance_id"]
    if response is None:
        send({"jsonrpc": "2.0", "id": request["id"], "error": {
            "code": -32603, "message": "secret-provider-error", "data": "secret-data",
        }})
    else:
        send({"jsonrpc": "2.0", "id": request["id"], "result": response})
shutdown = receive()
assert shutdown["method"] == "shutdown"
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {}})
"#,
    );
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest()),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut events = process.subscribe();
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    assert_eq!(host.cache_warming_decision_hooks.len(), 1);
    assert!(process.supports_feature(EXTENSION_FEATURE_CACHE_WARMING_DECISION));
    for (index, expected) in [
        Some(CacheWarmingAction::Warm),
        Some(CacheWarmingAction::Stop),
        None,
        None,
        None,
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let owner = if index % 2 == 0 {
            "owner-one"
        } else {
            "owner-two"
        };
        assert_eq!(
            host.cache_warming_decision_hooks[0]
                .cache_warming_decision(&context(owner))
                .await,
            expected
        );
    }
    while let Ok(event) = events.try_recv() {
        if let ExtensionEvent::Diagnostic { message } = event {
            assert!(!message.contains("secret-"));
        }
    }
    assert!(process.shutdown().await);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn cache_warming_process_timeout_cancels_and_returns_no_opinion() {
    let temp = TempDir::new().unwrap();
    write_executable_script(
        &temp.path().join("extension.py"),
        r#"#!/usr/bin/env python3
import json
import sys


def receive():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


init = receive()
protocol = init["params"]["protocol"]
send({"jsonrpc": "2.0", "id": init["id"], "result": {
    "api_version": "0.4", "tools": [], "commands": [],
    "protocol": {"version": "0.4", "features":
        protocol["required_features"] + ["cache_warming_decision"],
        "limits": {"max_concurrent_requests": 1}},
}})
request = receive()
assert request["params"]["hook"] == "cache_warming_decision"
cancel = receive()
assert cancel["method"] == "$/cancelRequest"
assert cancel["params"]["id"] == request["id"]
send({"jsonrpc": "2.0", "id": request["id"], "error": {
    "code": -32800, "message": "Request cancelled",
}})
shutdown = receive()
assert shutdown["method"] == "shutdown"
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {}})
"#,
    );
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest()),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            process.cache_warming_decision(&context("owner-one")),
        )
        .await
        .expect("the advisory cap is shorter than the ordinary 30s request timeout"),
        None
    );
    assert_eq!(process.health_snapshot().pending_requests, 0);
    assert!(process.shutdown().await);
}
