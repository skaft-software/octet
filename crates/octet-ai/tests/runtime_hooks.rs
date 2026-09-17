#![allow(missing_docs)]

//! Host-owned request runtime hook regressions (ledger 1b.1 consumers).
//!
//! These tests drive the real dispatch path against loopback: payload/header
//! hooks, the per-request credential override, the response observer, and the
//! refusal of host-owned retry/metadata/transport combinations.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use octet_ai::{
    AiClient, AiError, Auth, Capabilities, CompatibilityMode::Strict, Diagnostic, Endpoint,
    EndpointId, HeaderTransform, HookModelContext, HostRequestOptions, HostStreamModel,
    HostStreamTransport, Message, ModalitySet, Model, ModelId, ModelLimits, ModelSpec,
    OutputFormat, OutputModalities, PayloadHook, ReasoningConfig, ReasoningMode, Request,
    RequestOverrides, ResponseHook, ResponseStream, Secret, ToolChoice, UserMessage, UserPart,
};

const SSE_BODY: &str = "data: {\"id\": \"chatcmpl-hooks\", \"choices\": [{\"delta\": {\"content\": \"hooked\"}}]}\n\n\
                        data: {\"id\": \"chatcmpl-hooks\", \"choices\": [{\"delta\": {}, \"finish_reason\": \"stop\"}]}\n\n\
                        data: [DONE]\n\n";

fn test_model(base_url: &str, auth: Auth) -> Model {
    let spec = ModelSpec {
        preset: Default::default(),
        id: ModelId("hook-model".to_string()),
        endpoint: EndpointId("hook-ep".to_string()),
        api_name: "gpt-4-test".to_string(),
        display_name: None,
        protocol: octet_ai::Protocol::OpenAiChat,
        capabilities: Capabilities {
            input_modalities: ModalitySet::none(),
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: false,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: false,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window: 10000,
            max_output_tokens: 2000,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    };
    let endpoint = Endpoint {
        id: EndpointId("hook-ep".to_string()),
        base_url: url::Url::parse(base_url).unwrap(),
        auth,
        default_headers: http::HeaderMap::new(),
        transport: octet_ai::EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime::default(),
        timeout: Duration::from_secs(5),
    };
    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(endpoint),
    }
}

fn request() -> Request {
    Request {
        system: None,
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("go".to_string())],
        })],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: vec![],
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    }
}

async fn sse_mock(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SSE_BODY)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-marker", "observed"),
        )
        .mount(server)
        .await;
}

struct AddHeader(&'static str, &'static str);

impl HeaderTransform for AddHeader {
    fn transform_headers(
        &self,
        headers: &mut http::HeaderMap,
        model: &HookModelContext,
    ) -> Result<(), AiError> {
        assert_eq!(model.id, "hook-model");
        assert_eq!(model.provider, "hook-ep");
        assert_eq!(model.api, "openai-chat");
        headers.insert(
            http::HeaderName::from_static(self.0),
            http::HeaderValue::from_static(self.1),
        );
        Ok(())
    }
}

struct InjectTopK;

impl PayloadHook for InjectTopK {
    fn on_payload(
        &self,
        mut payload: serde_json::Value,
        _model: &HookModelContext,
    ) -> Result<Option<serde_json::Value>, AiError> {
        payload["top_k"] = serde_json::json!(3);
        Ok(Some(payload))
    }
}

struct ObserveResponse(Arc<Mutex<Option<(u16, Option<String>)>>>);

impl ResponseHook for ObserveResponse {
    fn on_response(
        &self,
        status: http::StatusCode,
        headers: &http::HeaderMap,
        _model: &HookModelContext,
    ) {
        let marker = headers
            .get("x-marker")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        *self.0.lock().unwrap() = Some((status.as_u16(), marker));
    }
}

#[derive(Default)]
struct CountingTransport {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl HostStreamTransport for CountingTransport {
    async fn stream(
        &self,
        _model: HostStreamModel,
        _request: Request,
        _diagnostics: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(AiError::Provider(octet_ai::ProviderError {
            code: Some("host_transport".to_owned()),
            kind: None,
            message: "host transport used".to_owned(),
            request_id: None,
        }))
    }
}

#[tokio::test]
async fn header_transform_runs_before_auth_and_observes_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-order", "caller"))
        .and(header("authorization", "Bearer env-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SSE_BODY)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;
    let model = test_model(
        &format!("{}/", server.uri()),
        Auth::BearerEnv {
            var: "OCTET_RUNTIME_HOOK_TOKEN".to_owned(),
        },
    );
    let client = AiClient::new();
    let overrides = RequestOverrides {
        env: BTreeMap::from([(
            "OCTET_RUNTIME_HOOK_TOKEN".to_owned(),
            "env-token".to_owned(),
        )]),
        ..Default::default()
    };
    let runtime = HostRequestOptions {
        transform_headers: Some(Arc::new(AddHeader("x-order", "caller"))),
        ..Default::default()
    };
    let mut stream = client
        .stream_with_host_options(&model, request(), overrides, runtime)
        .await
        .unwrap();
    let mut terminal = None;
    while let Some(event) = stream.next().await {
        if let octet_ai::StreamEvent::Finished(response) = event.unwrap() {
            terminal = Some(response);
        }
    }
    assert!(terminal.is_some());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn header_transform_cannot_forge_or_suppress_auth() {
    let server = MockServer::start().await;
    sse_mock(&server).await;
    let model = test_model(
        &format!("{}/", server.uri()),
        Auth::BearerEnv {
            var: "OCTET_RUNTIME_HOOK_TOKEN".to_owned(),
        },
    );
    let client = AiClient::new();
    let runtime = HostRequestOptions {
        transform_headers: Some(Arc::new(AddHeader("authorization", "forged"))),
        ..Default::default()
    };
    let error = client
        .stream_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .err()
        .expect("reserved header must fail closed");
    assert!(matches!(
        error,
        AiError::Config(octet_ai::ConfigError::ReservedHeader(_))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn payload_hook_replaces_the_encoded_body() {
    let server = MockServer::start().await;
    sse_mock(&server).await;
    let model = test_model(&format!("{}/", server.uri()), Auth::None);
    let client = AiClient::new();
    let runtime = HostRequestOptions {
        on_payload: Some(Arc::new(InjectTopK)),
        ..Default::default()
    };
    let response = client
        .complete_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .unwrap();
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl-hooks"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["top_k"], 3);
    assert_eq!(body["model"], "gpt-4-test");
    assert_eq!(body["messages"][0]["content"], "go");
}

#[tokio::test]
async fn response_hook_observes_status_and_headers_before_body_read() {
    let server = MockServer::start().await;
    sse_mock(&server).await;
    let observed = Arc::new(Mutex::new(None));
    let model = test_model(&format!("{}/", server.uri()), Auth::None);
    let client = AiClient::new();
    let runtime = HostRequestOptions {
        on_response: Some(Arc::new(ObserveResponse(observed.clone()))),
        ..Default::default()
    };
    client
        .complete_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .unwrap();
    let observed = observed.lock().unwrap().clone();
    assert_eq!(observed, Some((200, Some("observed".to_owned()))));
}

#[tokio::test]
async fn api_key_override_replaces_an_env_backed_credential_only() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer per-request-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SSE_BODY)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;
    let model = test_model(
        &format!("{}/", server.uri()),
        Auth::BearerEnv {
            var: "OCTET_RUNTIME_UNSET_ALIAS".to_owned(),
        },
    );
    let client = AiClient::new();
    let runtime = HostRequestOptions {
        api_key: Some(Secret::from("per-request-key")),
        ..Default::default()
    };
    client
        .complete_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // A fixed credential refuses the override rather than silently ignoring it.
    let fixed = test_model(&format!("{}/", server.uri()), Auth::bearer("fixed-key"));
    let error = client
        .complete_with_host_options(
            &fixed,
            request(),
            RequestOverrides::default(),
            HostRequestOptions {
                api_key: Some(Secret::from("per-request-key")),
                ..Default::default()
            },
        )
        .await
        .err()
        .expect("fixed credentials must refuse an override");
    assert!(matches!(error, AiError::Auth(octet_ai::AuthError::Resolve)));
}

#[tokio::test]
async fn metadata_and_bound_overrides_fail_closed_before_dispatch() {
    let server = MockServer::start().await;
    sse_mock(&server).await;
    let model = test_model(&format!("{}/", server.uri()), Auth::None);
    let client = AiClient::new();

    let mut runtime = HostRequestOptions::default();
    runtime
        .metadata
        .insert("user_id".to_owned(), serde_json::json!("abc"));
    assert!(client
        .stream_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .is_err());

    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = HostRequestOptions {
        fetch: Some(Arc::new(CountingTransport {
            calls: calls.clone(),
        })),
        ..Default::default()
    };
    // Fetch overrides are allowed on their own...
    let error = client
        .stream_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .err()
        .expect("the counting transport always fails");
    assert!(matches!(error, AiError::Provider(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // ...but they cannot be combined with wire hooks or wire overrides.
    let runtime = HostRequestOptions {
        fetch: Some(Arc::new(CountingTransport {
            calls: calls.clone(),
        })),
        transform_headers: Some(Arc::new(AddHeader("x-order", "caller"))),
        ..Default::default()
    };
    assert!(client
        .stream_with_host_options(&model, request(), RequestOverrides::default(), runtime)
        .await
        .is_err());
    let runtime = HostRequestOptions {
        fetch: Some(Arc::new(CountingTransport {
            calls: calls.clone(),
        })),
        ..Default::default()
    };
    let overrides = RequestOverrides {
        headers: BTreeMap::from([("x-order".to_owned(), "caller".to_owned())]),
        ..Default::default()
    };
    assert!(client
        .stream_with_host_options(&model, request(), overrides, runtime)
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn runtime_debug_and_validation_never_expose_credentials() {
    let runtime = HostRequestOptions {
        api_key: Some(Secret::from("super-secret-runtime-key")),
        metadata: BTreeMap::from([("k".to_owned(), serde_json::json!("v"))]),
        ..Default::default()
    };
    let debug = format!("{runtime:?}");
    assert!(!debug.contains("super-secret-runtime-key"));
    assert!(runtime.validate().is_ok());
    assert!(runtime.has_wire_hooks());
    assert!(!runtime.is_empty());
}
