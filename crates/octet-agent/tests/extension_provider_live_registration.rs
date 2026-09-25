#![cfg(unix)]
#![allow(missing_docs)]

//! Late API 0.3 provider registrations must take effect in the running process
//! generation.
//!
//! The fixture issues `providers/register`, `providers/update`, and
//! `providers/unregister` from a tool handler, i.e. strictly after the initial
//! catalog completed. Those reverse requests arrive on the always-running
//! protocol reader, so the registry must reflect them immediately: no `/reload`,
//! no second `providers/complete`, and no process generation change.

use std::sync::Arc;
use std::time::Duration;

use octet_agent::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionProviderOwner, ExtensionProviderRegistry, ExtensionRuntimeConfig, ExtensionSource,
    ExtensionTrust, EXTENSION_MANIFEST_FILENAME,
};
use serde_json::json;
use tempfile::TempDir;

const FIXTURE: &str = r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


def provider(provider_id, model_id, max_output_tokens=1024):
    return {
        "provider": {
            "id": provider_id,
            "label": provider_id + " provider",
            "auth": {"kind": "none"},
        },
        "models": [{
            "id": model_id,
            "api_name": model_id,
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": max_output_tokens,
            "capabilities": {
                "tools": False,
                "parallel_tool_calls": False,
                "structured_output": False,
                "reasoning": False,
            },
        }],
    }


def reverse_request(identifier, method, params):
    send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
    response = receive()
    assert response.get("id") == identifier and "result" in response, response
    return response["result"]


initialize = receive()
assert initialize["method"] == "initialize", initialize
contract = initialize["params"]["contract"]
provider_capabilities = {"provider_catalog", "provider_stream", "provider_auth"}
provider_methods = {
    "providers/complete",
    "providers/register",
    "providers/update",
    "providers/unregister",
    "provider/stream",
    "provider/event",
    "provider/cancel",
    "provider/auth/request",
    "provider/auth/revoke",
}
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": [
        capability
        for capability in contract["required_capabilities"] + contract["optional_capabilities"]
        if capability in contract["required_capabilities"] or capability in provider_capabilities
    ],
    "methods": [
        method
        for method in contract["required_methods"] + contract["optional_methods"]
        if method in contract["required_methods"] or method in provider_methods
    ],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.3",
        "tools": [{
            "name": "late-control",
            "description": "Publish or retire a provider declaration after the initial catalog",
            "parameters": {"type": "object"},
        }],
        "contract": selection,
    },
})
reverse_request("initial-register", "providers/register", provider("alpha", "alpha-model"))
send({"jsonrpc": "2.0", "method": "providers/complete", "params": {}})

while True:
    message = receive()
    method = message.get("method")
    if method == "tool/call":
        action = message["params"]["arguments"].get("action")
        if action == "register-beta":
            reverse_request("late-register", "providers/register", provider("beta", "beta-model"))
        elif action == "update-beta":
            reverse_request("late-update", "providers/update", provider("beta", "beta-model", 4096))
        elif action == "unregister-beta":
            reverse_request("late-unregister", "providers/unregister", {"provider_id": "beta"})
        else:
            raise AssertionError(action)
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "content": [{"type": "text", "text": action}],
                "is_error": False,
                "metadata": {},
            },
        })
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
    else:
        raise AssertionError(message)
"#;

/// A fixture that records one declaration and never publishes it: the initial
/// batch deliberately stays incomplete.
const PARKED_FIXTURE: &str = r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    return json.loads(line)


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
assert initialize["method"] == "initialize", initialize
contract = initialize["params"]["contract"]
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"] + ["provider_catalog"],
    "methods": contract["required_methods"] + ["providers/register"],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})
send({
    "jsonrpc": "2.0",
    "id": "parked",
    "method": "providers/register",
    "params": {
        "provider": {"id": "parked", "label": "Parked provider", "auth": {"kind": "none"}},
        "models": [{
            "id": "parked-model",
            "api_name": "parked-model",
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": 1024,
            "capabilities": {
                "tools": False,
                "parallel_tool_calls": False,
                "structured_output": False,
                "reasoning": False,
            },
        }],
    },
})

while True:
    message = receive()
    if message.get("method") == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
"#;

async fn start_fixture(
    manifest: &str,
    source: &str,
) -> (TempDir, ExtensionProcess, Arc<ExtensionProviderRegistry>) {
    let directory = TempDir::new().expect("fixture workspace");
    let manifest_path = directory.path().join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(&manifest_path, manifest).expect("fixture manifest");
    let script = directory.path().join("fixture.py");
    std::fs::write(&script, source).expect("fixture source");
    let registry = Arc::new(ExtensionProviderRegistry::new());
    let mut config = ExtensionRuntimeConfig::new(directory.path());
    config.provider_registry = Some(Arc::clone(&registry));
    config.request_timeout = Duration::from_secs(5);
    config.shutdown_timeout = Duration::from_secs(1);
    config.supervise = false;
    let descriptor = DiscoveredExtension {
        manifest: ExtensionManifest::load(&manifest_path).expect("fixture manifest"),
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("start late-provider fixture");
    (directory, process, registry)
}

async fn wait_until(mut predicate: impl FnMut() -> bool) -> bool {
    for _ in 0..500 {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

const MANIFEST: &str = r#"name = "late-provider"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = ["fixture.py"]
[contributes]
tools = ["late-control"]
providers = true
"#;

#[tokio::test]
async fn late_provider_registration_update_and_unregister_are_live_in_the_same_generation() {
    let (_directory, process, registry) = start_fixture(MANIFEST, FIXTURE).await;
    assert!(
        wait_until(|| registry.resolve("alpha", "alpha-model").is_some()).await,
        "the initial declaration was never published"
    );
    let generation = process.health_snapshot().generation;

    // The late reverse request is issued from a tool handler, strictly after
    // the owner completed its initial catalog.
    process
        .call_tool(
            "late-control",
            json!({"action": "register-beta"}),
            process.current_context(),
        )
        .await
        .expect("late registration handler");

    let route = registry
        .resolve("beta", "beta-model")
        .expect("a late registration is callable immediately");
    assert_eq!(
        route.owner.extension_instance_id,
        process.extension_instance_id()
    );
    assert!(registry.route_is_active(&route));
    let entries = registry.snapshot().1;
    assert_eq!(entries.len(), 2, "both declarations are live: {entries:?}");

    // `providers/update` replaces the live declaration immediately.
    process
        .call_tool(
            "late-control",
            json!({"action": "update-beta"}),
            process.current_context(),
        )
        .await
        .expect("late update handler");
    assert!(
        wait_until(|| registry
            .resolve("beta", "beta-model")
            .is_some_and(|current| current.model.max_output_tokens == 4096))
        .await,
        "the late update never replaced the declaration"
    );
    assert!(
        !registry.route_is_active(&route),
        "the replaced route must stop being active"
    );

    // `providers/unregister` withdraws it immediately.
    process
        .call_tool(
            "late-control",
            json!({"action": "unregister-beta"}),
            process.current_context(),
        )
        .await
        .expect("late unregister handler");
    assert!(
        wait_until(|| registry.resolve("beta", "beta-model").is_none()).await,
        "the late unregister never removed the declaration"
    );
    assert!(registry.resolve("alpha", "alpha-model").is_some());
    assert_eq!(registry.snapshot().1.len(), 1);

    assert_eq!(
        process.health_snapshot().generation,
        generation,
        "late registration must never require a process generation change"
    );
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn late_registration_without_initial_completion_stays_withheld() {
    let (_directory, process, registry) = start_fixture(
        r#"name = "parked-provider"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = ["fixture.py"]
[contributes]
providers = true
"#,
        PARKED_FIXTURE,
    )
    .await;

    assert!(
        wait_until(|| !registry.recorded_providers().is_empty()).await,
        "the recorded declaration never arrived"
    );
    let (entry, complete) = registry.recorded_providers().remove(0);
    assert_eq!(entry.provider.id, "parked");
    assert!(
        !complete,
        "an owner that never completed its initial batch must not be live"
    );
    assert!(registry.resolve("parked", "parked-model").is_none());
    assert!(registry.snapshot().1.is_empty());
    assert!(process.shutdown().await);
}

#[test]
fn foreign_and_duplicate_owners_cannot_replace_a_late_registration() {
    let registry = ExtensionProviderRegistry::new();
    let owner = ExtensionProviderOwner {
        extension_instance_id: "extension-1".into(),
        generation: 1,
    };
    let params = json!({
        "provider": {"id": "fixture", "label": "Fixture", "auth": {"kind": "none"}},
        "models": [{
            "id": "model",
            "api_name": "model",
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": 1024,
            "capabilities": {
                "tools": false,
                "parallel_tool_calls": false,
                "structured_output": false,
                "reasoning": false,
            },
        }],
    });
    let register = octet_agent::extension_api_v03::parse_provider_register_params(params)
        .expect("valid declaration");
    registry
        .register(owner.clone(), register)
        .expect("initial registration");
    registry.complete_initial_catalog(&owner);

    let duplicate = octet_agent::extension_api_v03::parse_provider_register_params(json!({
        "provider": {"id": "fixture", "label": "Fixture", "auth": {"kind": "none"}},
        "models": [],
    }))
    .expect("valid declaration");
    assert_eq!(
        registry.register(owner, duplicate).unwrap_err(),
        octet_agent::ExtensionProviderRegistryError::ProviderConflict
    );
    let foreign = ExtensionProviderOwner {
        extension_instance_id: "extension-2".into(),
        generation: 2,
    };
    let foreign_params = octet_agent::extension_api_v03::parse_provider_register_params(json!({
        "provider": {"id": "fixture", "label": "Fixture", "auth": {"kind": "none"}},
        "models": [],
    }))
    .expect("valid declaration");
    assert_eq!(
        registry
            .register(foreign.clone(), foreign_params)
            .unwrap_err(),
        octet_agent::ExtensionProviderRegistryError::ProviderConflict
    );
    assert_eq!(
        registry.unregister(&foreign, "fixture").unwrap_err(),
        octet_agent::ExtensionProviderRegistryError::StaleOwner
    );
    assert!(registry.resolve("fixture", "model").is_some());
}
