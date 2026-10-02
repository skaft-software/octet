//! Extensions that register providers after startup.
//!
//! Covers a late registration becoming live at the next synchronisation boundary,
//! a bulk late-registration notice staying within its bound, and a conflicting late
//! model never replacing a resident catalog route. Split out — and unix-only —
//! because it carries its own Python provider fixture, which nothing else in the
//! suite needs and which cannot run off unix.

use super::*;

const LATE_PROVIDER_FIXTURE: &str = r#"#!/usr/bin/env python3
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


def provider(provider_id, model_id):
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
            "max_output_tokens": 1024,
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


def provider_many(provider_id, count):
    declaration = provider(provider_id, "bulk-model-0")
    declaration["models"] = [
        {
            "id": "bulk-model-%d" % index,
            "api_name": "bulk-model-%d" % index,
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": 1024,
            "capabilities": {
                "tools": False,
                "parallel_tool_calls": False,
                "structured_output": False,
                "reasoning": False,
            },
        }
        for index in range(count)
    ]
    return declaration


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
        elif action == "register-many":
            reverse_request(
                "late-register-many",
                "providers/register",
                provider_many("bulk", 12),
            )
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

async fn start_late_provider_fixture(
    directory: &std::path::Path,
) -> (ExtensionProcess, ExtensionProviderRuntime) {
    std::fs::write(directory.join("late-provider.py"), LATE_PROVIDER_FIXTURE).unwrap();
    let manifest = ExtensionManifest::parse(
        r#"name = "late-provider"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = ["late-provider.py"]
[contributes]
tools = ["late-control"]
providers = true
"#,
    )
    .unwrap();
    let runtime = ExtensionProviderRuntime::default();
    let mut config = ExtensionRuntimeConfig::new(directory);
    config.provider_registry = Some(runtime.registry());
    config.request_timeout = Duration::from_secs(5);
    config.shutdown_timeout = Duration::from_secs(1);
    config.supervise = false;
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: directory.join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        config,
    )
    .await
    .unwrap();
    (process, runtime)
}

async fn wait_for_extension_provider(
    runtime: &ExtensionProviderRuntime,
    provider_id: &str,
    model_id: &str,
) -> bool {
    for _ in 0..500 {
        if runtime.registry().resolve(provider_id, model_id).is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

fn resident_endpoint(id: &str) -> Endpoint {
    Endpoint {
        id: EndpointId(id.to_owned()),
        base_url: url::Url::parse("http://127.0.0.1:9/").unwrap(),
        auth: Auth::None,
        default_headers: http::HeaderMap::new(),
        transport: EndpointTransport::Http,
        runtime: RequestRuntime::default(),
        timeout: Duration::from_secs(30),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn late_provider_registration_becomes_live_at_the_next_synchronization_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let (process, runtime) = start_late_provider_fixture(temp.path()).await;
    let processes = vec![process.clone()];
    assert!(
        wait_for_extension_provider(&runtime, "alpha", "alpha-model").await,
        "the initial declaration was never published"
    );

    let mut catalog = ModelCatalog::builtin().unwrap();
    let client = AiClient::new();
    assert!(
        runtime
            .synchronize(&mut catalog, &client, &processes)
            .is_empty(),
        "the first projection must not report a live registration"
    );
    assert!(catalog
        .resolve(&ModelId("alpha/alpha-model".into()))
        .is_ok());

    // The registration is issued from a tool handler, i.e. strictly after
    // the initial load phase.
    process
        .call_tool(
            "late-control",
            serde_json::json!({"action": "register-beta"}),
            process.current_context(),
        )
        .await
        .unwrap();
    assert!(
        wait_for_extension_provider(&runtime, "beta", "beta-model").await,
        "the late declaration never reached the registry"
    );
    let beta = ModelId("beta/beta-model".into());
    assert!(
        catalog.resolve(&beta).is_err(),
        "a running session must not mutate before its synchronization boundary"
    );

    let notices = runtime.synchronize(&mut catalog, &client, &processes);
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("beta/beta-model")),
        "a live registration must be visible to the user: {notices:?}"
    );
    let registered = catalog
        .resolve(&beta)
        .expect("the late model is selectable");
    assert_eq!(registered.spec.api_name, "beta-model");
    assert_eq!(registered.spec.limits.context_window, 8192);
    assert!(!registered.spec.capabilities.tools);
    assert!(
        registered.spec.pricing.is_none(),
        "an unpriced declaration must stay explicitly unknown, never free"
    );
    assert!(
        runtime
            .synchronize(&mut catalog, &client, &processes)
            .is_empty(),
        "an unchanged registry must not re-project or re-notice"
    );

    // The live summary surface reports the same declaration as live.
    let mut extensions = ExecutableExtensions::default();
    extensions.provider_runtime = runtime.clone();
    extensions.processes.push(process.clone());
    extensions.summaries = vec![ExtensionSummary {
        name: "late-provider".into(),
        version: "0.3.0".into(),
        manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
        manifest_digest: String::new(),
        bundle_digest: None,
        source: ExtensionSource::Explicit,
        enabled: true,
        trusted: true,
        running: false,
        api_version: "0.3".into(),
        negotiated_features: Vec::new(),
        telemetry_schema: None,
        compatibility: "compatible".into(),
        health: None,
        runtime: None,
        tools: Vec::new(),
        commands: Vec::new(),
        hooks: Vec::new(),
        ui: Vec::new(),
        providers: Vec::new(),
    }];
    let summary = extensions.summaries().remove(0);
    let provider = summary
        .providers
        .iter()
        .find(|provider| provider.id == "beta")
        .expect("the live declaration is listed");
    assert!(provider.live, "the late declaration must be reported live");
    assert_eq!(provider.authorization, "ready");
    assert_eq!(provider.models, vec!["beta/beta-model".to_owned()]);
    assert!(extensions.inspect_text().contains("beta [ready] live"));

    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn a_bulk_late_registration_notice_stays_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let (process, runtime) = start_late_provider_fixture(temp.path()).await;
    let processes = vec![process.clone()];
    assert!(wait_for_extension_provider(&runtime, "alpha", "alpha-model").await);

    let mut catalog = ModelCatalog::builtin().unwrap();
    let client = AiClient::new();
    runtime.synchronize(&mut catalog, &client, &processes);

    process
        .call_tool(
            "late-control",
            serde_json::json!({"action": "register-many"}),
            process.current_context(),
        )
        .await
        .unwrap();
    assert!(wait_for_extension_provider(&runtime, "bulk", "bulk-model-0").await);

    let notices = runtime.synchronize(&mut catalog, &client, &processes);
    assert_eq!(
        notices
            .iter()
            .filter(|notice| notice.contains("is now live"))
            .count(),
        MAX_LIVE_REGISTRATION_NOTICES,
        "live-registration notices must stay bounded: {notices:?}"
    );
    assert!(
        notices.iter().any(
            |notice| notice.contains("more model(s) registered while this session was running")
        ),
        "the overflow must be summarized: {notices:?}"
    );
    assert!(catalog
        .resolve(&ModelId("bulk/bulk-model-11".into()))
        .is_ok());
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn a_conflicting_late_model_never_replaces_a_resident_catalog_route() {
    let temp = tempfile::tempdir().unwrap();
    let (process, runtime) = start_late_provider_fixture(temp.path()).await;
    let processes = vec![process.clone()];
    assert!(wait_for_extension_provider(&runtime, "alpha", "alpha-model").await);

    let mut catalog = ModelCatalog::builtin().unwrap();
    let client = AiClient::new();
    runtime.synchronize(&mut catalog, &client, &processes);

    // A resident route (for example a built-in) already owns the exact
    // catalog id the late declaration would use.
    let shadow = ModelId("beta/beta-model".into());
    catalog
        .register_endpoint(resident_endpoint("resident-route"))
        .unwrap();
    catalog
        .register_model(ModelSpec {
            id: shadow.clone(),
            endpoint: EndpointId("resident-route".into()),
            api_name: "resident-model".into(),
            display_name: None,
            protocol: Protocol::OpenAiChat,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: Default::default(),
                output_modalities: Default::default(),
                tools: false,
                parallel_tool_calls: false,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 8192,
                max_output_tokens: 1024,
            },
            pricing: None,
            preset: Default::default(),
            cache: CacheCompatibility {
                prompt_cache: Default::default(),
                supports_long_retention: false,
                supports_explicit_prompt_cache_mode: false,
                send_session_id_header: false,
                send_session_affinity_headers: false,
                session_affinity_format: None,
                cache_control_format: None,
                supports_cache_control_on_tools: false,
            },
        })
        .unwrap();

    process
        .call_tool(
            "late-control",
            serde_json::json!({"action": "register-beta"}),
            process.current_context(),
        )
        .await
        .unwrap();
    assert!(wait_for_extension_provider(&runtime, "beta", "beta-model").await);

    let notices = runtime.synchronize(&mut catalog, &client, &processes);
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("conflicts with an existing catalog model")),
        "a conflicting late model must be refused with a diagnostic: {notices:?}"
    );
    assert_eq!(
        catalog.resolve(&shadow).unwrap().endpoint.id,
        EndpointId("resident-route".into()),
        "the resident route must never be silently replaced"
    );
    assert!(
        runtime
            .synchronize(&mut catalog, &client, &processes)
            .is_empty(),
        "a refused declaration must not force re-projection on every boundary"
    );

    // Unregistering the late declaration leaves the resident route intact.
    process
        .call_tool(
            "late-control",
            serde_json::json!({"action": "unregister-beta"}),
            process.current_context(),
        )
        .await
        .unwrap();
    for _ in 0..500 {
        if runtime.registry().resolve("beta", "beta-model").is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(runtime.registry().resolve("beta", "beta-model").is_none());
    runtime.synchronize(&mut catalog, &client, &processes);
    assert_eq!(
        catalog.resolve(&shadow).unwrap().endpoint.id,
        EndpointId("resident-route".into())
    );
    assert!(catalog
        .resolve(&ModelId("alpha/alpha-model".into()))
        .is_ok());

    assert!(process.shutdown().await);
}
