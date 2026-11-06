use super::*;
use pretty_assertions::assert_eq;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn google_proxy_declaration_preserves_closed_api_03_and_bounded_api_04() {
    let original = serde_json::json!({
        "provider":{"id":"google","label":"Google","auth":{"kind":"none"}},
        "models":[{"id":"selected","api_name":"gemini-local","protocol":"google_generative_ai","context_window":8192,"max_output_tokens":1024,
            "capabilities":{"tools":false,"parallel_tool_calls":false,"structured_output":false,"reasoning":false},
            "pi_metadata":{"base_url":"","input":["text"],"pricing":null,"inherit_provider":"google"}}]
    });
    assert!(model_control::parse_provider_proxy_registration(original.clone()).is_ok());
    let (events, _) = broadcast::channel(8);
    let (state, _) = protocol_read_state_for_test(
        ManifestContributions {
            providers: true,
            ..ManifestContributions::default()
        },
        events,
    );
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = "0.4".into();
        protocol.features = [
            "provider_proxy_v1".to_owned(),
            "provider_inheritance_v1".to_owned(),
        ]
        .into_iter()
        .collect();
    }
    assert!(matches!(
        model_control::register_provider_proxy(&state, original.clone(), false),
        Err(ProviderHostResponseError::Invalid)
    ));
    let mut canonical = original.clone();
    canonical["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("pi_metadata");
    assert!(api_v03::parse_provider_register_params(canonical).is_err());
    for (field, value) in [
        ("protocol", serde_json::json!("google_interactions")),
        ("protocol", serde_json::json!("bedrock_converse")),
        ("api_name", serde_json::json!("x".repeat(65))),
        ("id", serde_json::json!("x".repeat(65))),
        ("display_name", serde_json::Value::Null),
        ("display_name", serde_json::json!("x".repeat(129))),
        (
            "context_window",
            serde_json::json!(9_007_199_254_740_992u64),
        ),
        ("extra", serde_json::json!(true)),
    ] {
        let mut bad = original.clone();
        bad["models"][0][field] = value;
        assert!(
            model_control::parse_provider_proxy_registration(bad).is_err(),
            "{field}"
        );
    }
    let mut bad = original.clone();
    bad["models"][0]["pi_metadata"]["inherit_provider"] = serde_json::json!("other");
    assert!(model_control::parse_provider_proxy_registration(bad).is_err());
    // An extension-streamed Google model (no inheritance) is a label only; its
    // own process performs the request, so registration admits it with pricing.
    let mut own = original.clone();
    own["models"][0]["pi_metadata"]
        .as_object_mut()
        .unwrap()
        .remove("inherit_provider");
    own["models"][0]["pi_metadata"]["pricing"] = serde_json::json!({"input":0,"output":0,"cache_read":0,"cache_write_5m":0,"cache_write_1h":null,"reasoning":null,"tiers":[]});
    assert!(model_control::parse_provider_proxy_registration(own).is_ok());
    for protocol in ["anthropic_messages", "openai_chat", "openai_responses"] {
        let mut old = original.clone();
        old["models"][0]["protocol"] = serde_json::json!(protocol);
        assert!(model_control::parse_provider_proxy_registration(old).is_ok());
    }
}

#[test]
fn google_dialect_negotiation_requires_catalog_proxy_and_inheritance_selection() {
    let manifest = ExtensionManifest::parse("name = \"google-fixture\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"google.py\"\n[contributes]\nproviders = true\n").unwrap();
    for offered in [false, true] {
        for inherit in [false, true] {
            for proxy in [false, true] {
                let mut features = API_0_2_REQUIRED_FEATURES
                    .iter()
                    .map(|feature| (*feature).to_owned())
                    .collect::<Vec<_>>();
                features.push("provider_google_generative_ai_v1".into());
                if inherit {
                    features.push("provider_inheritance_v1".into());
                }
                if proxy {
                    features.push("provider_proxy_v1".into());
                }
                let response = InitializeResponse {
                    api_version: "0.4".into(),
                    tools: Vec::new(),
                    commands: Vec::new(),
                    tool_renderers: Vec::new(),
                    shortcuts: Vec::new(),
                    protocol: Some(ExtensionProtocolResponse {
                        session_snapshot_transport_v1: None,
                        version: "0.4".into(),
                        features,
                        limits: ExtensionProtocolLimits {
                            max_message_bytes: None,
                            resource_refs_v1: None,
                            max_concurrent_requests: 4,
                        },
                        lifecycle_events: Vec::new(),
                    }),
                };
                let result = negotiate_contributions_with_host_services(
                    &manifest,
                    response,
                    4,
                    OfferedHostServices {
                        provider_proxy: true,
                        provider_inheritance: offered,
                        ..OfferedHostServices::default()
                    },
                );
                assert_eq!(
                    result.is_ok(),
                    offered && inherit && proxy,
                    "offered={offered} inherit={inherit} proxy={proxy}: {result:?}"
                );
            }
        }
    }
}

const GOOGLE_PROVIDER: &str = r#"#!/usr/bin/env python3
import json, sys

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)
def recv():
    return json.loads(sys.stdin.readline())
def call(id, method, params):
    send({"jsonrpc":"2.0","id":id,"method":method,"params":params})
    response = recv()
    assert response.get("id") == id and "result" in response, response
    return response["result"]
init = recv()
p = init["params"]["protocol"]
features = p["required_features"] + [f for f in p["optional_features"] if f in ["provider_proxy_v1", "provider_inheritance_v1", "provider_google_generative_ai_v1"]]
assert "provider_google_generative_ai_v1" in features
send({"jsonrpc":"2.0","id":init["id"],"result":{"api_version":"0.4","tools":[],"protocol":{"version":"0.4","features":features,"limits":{"max_concurrent_requests":4},"lifecycle_events":[]}}})
models = call("inherit", "providers/inherit", {"provider_id":"google"})["models"]
assert models[0]["api"] == "google-generative-ai"
call("register", "providers/register", {"provider":{"id":"google","label":"Google","auth":{"kind":"none"}},"models":[{"id":"selected","api_name":"gemini-local","protocol":"google_generative_ai","context_window":8192,"max_output_tokens":1024,"capabilities":{"tools":False,"parallel_tool_calls":False,"structured_output":False,"reasoning":False},"pi_metadata":{"base_url":"","input":["text"],"pricing":None,"inherit_provider":"google"}}]})
send({"jsonrpc":"2.0","method":"providers/complete","params":{}})
sequence = 0
stream = None
def event(kind, payload):
    global sequence
    send({"jsonrpc":"2.0","method":"provider/event","params":{"stream_id":stream,"sequence":sequence,"kind":kind,"payload":payload}})
    sequence += 1
while True:
    msg = recv()
    kind = msg.get("method")
    if kind == "provider/stream":
        stream = msg["params"]["stream_id"]
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"stream_id":stream,"accepted":True}})
        send({"jsonrpc":"2.0","id":"delegate","method":"provider/delegate","params":{"stream_id":stream,"request":msg["params"]["request"],"on_payload":True}})
    elif kind == "provider/delegate_payload":
        payload = msg["params"]["payload"]
        assert "contents" in payload and "generationConfig" in payload and "messages" not in payload
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"payload":payload}})
    elif kind == "provider/delegate_event":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"accepted":True}})
    elif msg.get("id") == "delegate":
        assert "result" in msg, msg
        result = msg["result"]
        assert result["message"]["protocol"] == "google_generative_ai"
        event("started", {})
        event("text_start", {"index":0})
        event("text_delta", {"index":0,"delta":"native Google"})
        event("text_end", {"index":0})
        event("usage", result["usage"])
        event("finished", {"stop_reason":"stop"})
    elif kind == "shutdown":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{}})
        break
    elif kind not in ["$/cancelRequest", "provider/cancel"]:
        raise AssertionError(msg)
"#;

#[tokio::test]
async fn google_native_process_delegate_keeps_google_wire_auth_route_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-local:streamGenerateContent"))
        .and(header("x-goog-api-key", "local-fixture-only"))
        .and(body_string_contains("contents"))
        .and(body_string_contains("generationConfig"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(
            "data: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"native Google\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":3,\"totalTokenCount\":5}}\n\n"))
        .expect(1)
        .mount(&server).await;
    let endpoint = octet_ai::Endpoint {
        id: octet_ai::EndpointId("native-google".into()),
        base_url: url::Url::parse(&format!("{}/v1beta/", server.uri())).unwrap(),
        auth: octet_ai::Auth::header(
            http::HeaderName::from_static("x-goog-api-key"),
            "local-fixture-only",
        ),
        default_headers: Default::default(),
        transport: octet_ai::EndpointTransport::Http,
        runtime: Default::default(),
        timeout: Duration::from_secs(2),
    };
    let native = octet_ai::Model {
        spec: Arc::new(octet_ai::ModelSpec {
            preset: Default::default(),
            id: octet_ai::ModelId("native-google-model".into()),
            api_name: "gemini-local".into(),
            display_name: Some("Local Google".into()),
            endpoint: endpoint.id.clone(),
            protocol: Protocol::GoogleGenerativeAi,
            capabilities: octet_ai::Capabilities {
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
            limits: octet_ai::ModelLimits {
                context_window: 8192,
                max_output_tokens: 1024,
            },
            pricing: None,
            cache: Default::default(),
        }),
        endpoint: Arc::new(endpoint),
    };
    let view: ExtensionModelView = serde_json::from_value(serde_json::json!({
        "id":"gemini-local", "name":"Local Google", "api":"google-generative-ai", "provider":"google",
        "reasoning":false, "input":["text"], "context_window":8192, "max_tokens":1024
    })).unwrap();
    let registry = Arc::new(ExtensionProviderRegistry::new());
    registry.set_native_provider_catalog(NativeProviderCatalog {
        models: vec![("google".into(), native.clone(), view)],
    });
    let directory = TempDir::new().unwrap();
    write_executable_script(&directory.path().join("google.py"), GOOGLE_PROVIDER);
    let manifest = ExtensionManifest::parse("name = \"google-fixture\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"google.py\"\n[contributes]\nproviders = true\n").unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(directory.path());
    runtime.provider_registry = Some(registry.clone());
    let process = ExtensionProcess::start(trusted_descriptor(directory.path(), manifest), runtime)
        .await
        .unwrap();
    let route = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(route) = registry.resolve("google", "selected") {
                break route;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(route.model.protocol, "google_generative_ai");
    assert_eq!(
        route
            .model_metadata
            .as_ref()
            .unwrap()
            .native_model_id
            .as_deref(),
        Some("native-google-model")
    );
    let mut selected = native.clone();
    let mut endpoint = (*native.endpoint).clone();
    endpoint.id = octet_ai::EndpointId("overlay-google".into());
    let mut spec = (*native.spec).clone();
    spec.endpoint = endpoint.id.clone();
    selected.spec = Arc::new(spec);
    selected.endpoint = Arc::new(endpoint);
    let client = octet_ai::AiClient::new();
    client.register_host_stream_transport(
        selected.endpoint.id.clone(),
        process
            .prepare_provider_stream_transport(&route, octet_ai::HostStreamModel::from(&selected))
            .unwrap(),
    );
    let request: octet_ai::Request = serde_json::from_value(serde_json::json!({"system":null,"messages":[{"User":{"content":[{"Text":"local input"}]}}],"tools":[],"tool_choice":"auto","max_output_tokens":32,"temperature":null,"stop":[],"reasoning":{"type":"off"},"output_modalities":{"type":"text"}})).unwrap();
    let response = client.complete(&selected, request).await.unwrap();
    assert_eq!(response.usage.input_tokens, 2);
    assert_eq!(response.usage.output_tokens, 3);
    assert_eq!(response.usage.total_tokens, 5);
    assert_eq!(response.message.protocol, Protocol::GoogleGenerativeAi);
    assert_eq!(response.message.model, native.spec.id);
    assert!(process.shutdown().await);
    assert!(registry.resolve("google", "selected").is_none());
}
