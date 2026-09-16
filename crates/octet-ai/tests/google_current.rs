#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use octet_ai::{
    AiClient, AiError, Auth, CacheCompatibility, CacheRetention, Capabilities,
    CompatibilityMode::Strict, Endpoint, EndpointId, EndpointTransport, Message, ModalitySet,
    Model, ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities, Protocol,
    ReasoningConfig, ReasoningMode, Request, Response, StreamEvent, ToolChoice, ToolDef,
    UserMessage, UserPart,
};

fn fixture_model(base_url: &str) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("gemini-current-fixture".to_owned()),
            api_name: "gemini-2.5-flash".to_owned(),
            display_name: Some("Gemini current fixture".to_owned()),
            endpoint: EndpointId("google-current".to_owned()),
            protocol: Protocol::GoogleGenerativeAi,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: true,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 1_000_000,
                max_output_tokens: 65_536,
            },
            pricing: None,
            cache: CacheCompatibility::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("google-current".to_owned()),
            base_url: url::Url::parse(base_url).expect("fixture endpoint URL"),
            auth: Auth::header(
                http::HeaderName::from_static("x-goog-api-key"),
                "fixture-key",
            ),
            default_headers: http::HeaderMap::new(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(2),
        }),
    }
}

fn fixture_request() -> Request {
    Request {
        system: Some("Use the available tool when needed.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Look up Paris.".to_owned())],
        })],
        tools: vec![ToolDef {
            constrained_sampling: None,
            name: "lookup".to_owned(),
            description: "Look up a city.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }],
        tool_choice: ToolChoice::Named("lookup".to_owned()),
        max_output_tokens: Some(128),
        temperature: Some(0.2),
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: Strict,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}

fn plain_fixture_request() -> Request {
    let mut request = fixture_request();
    request.tools.clear();
    request.tool_choice = ToolChoice::Auto;
    request.system = None;
    request
}

#[tokio::test]
async fn gemini_current_request_tool_stream_signature_and_usage_fixture() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .and(header("x-goog-api-key", "fixture-key"))
        .and(body_string_contains("systemInstruction"))
        .and(body_string_contains("functionDeclarations"))
        .and(body_string_contains("parametersJsonSchema"))
        .and(body_string_contains("allowedFunctionNames"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"responseId\":\"gemini-current-1\",\"candidates\":[{\"index\":0,\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"lookup\",\"id\":\"call-1\",\"args\":{\"city\":\"Paris\"}},\"thoughtSignature\":\"opaque-signature\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":12,\"cachedContentTokenCount\":2,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":1,\"totalTokenCount\":18}}\n\n",
                ),
        )
        .mount(&server)
        .await;

    let response: Response = AiClient::new()
        .complete(
            &fixture_model(&format!("{}/v1beta/", server.uri())),
            fixture_request(),
        )
        .await
        .expect("Gemini current loopback fixture");

    assert_eq!(response.response_id.as_deref(), Some("gemini-current-1"));
    assert_eq!(response.usage.input_tokens, 10);
    assert_eq!(response.usage.cache_read_tokens, 2);
    assert_eq!(response.usage.output_tokens, 6);
    assert_eq!(response.usage.reasoning_tokens, 1);
    assert_eq!(response.usage.total_tokens, 18);
    assert!(matches!(
        response.stop_reason,
        octet_ai::StopReason::ToolUse
    ));
    assert!(response.message.content.iter().any(|part| matches!(
        part,
        octet_ai::AssistantPart::ProviderMetadata(
            octet_ai::ProviderPartMetadata::GoogleThoughtSignature { signature }
        ) if signature == "opaque-signature"
    )));
    assert!(response.message.content.iter().any(|part| matches!(
        part,
        octet_ai::AssistantPart::ToolCall(call)
            if call.name == "lookup" && call.arguments_json == r#"{"city":"Paris"}"#
    )));
}

#[tokio::test]
async fn gemini_current_error_and_drop_cancellation_fixtures() {
    let error_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"responseId\":\"error-1\",\"error\":{\"code\":400,\"status\":\"INVALID_ARGUMENT\",\"message\":\"fixture rejection\"}}\n\n",
                ),
        )
        .mount(&error_server)
        .await;
    let error = AiClient::new()
        .complete(
            &fixture_model(&format!("{}/v1beta/", error_server.uri())),
            plain_fixture_request(),
        )
        .await
        .expect_err("provider error must not become a successful response");
    let AiError::StreamFailure { inner, progress } = error else {
        panic!("expected annotated Google stream failure");
    };
    assert!(matches!(
        inner.as_ref(),
        AiError::Provider(provider)
            if provider.code.as_deref() == Some("400")
                && provider.kind.as_deref() == Some("INVALID_ARGUMENT")
                && provider.message == "fixture rejection"
                && provider.request_id.as_deref() == Some("error-1")
    ));
    assert_eq!(progress.provider_events, 1);
    assert_eq!(progress.decoded_events, 0);
    assert_eq!(progress.content_bytes, 0);
    assert_eq!(progress.buffered_bytes, 0);
    assert!(progress.first_body_seen);
    let last_event_ms = progress
        .last_event_ms
        .expect("provider error must retain stream timing metadata");
    assert!(progress.elapsed_ms >= last_event_ms);

    let cancel_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"responseId\":\"cancel-1\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"first\"}]}}]}\n\ndata: {not-json}\n\n",
                ),
        )
        .mount(&cancel_server)
        .await;
    let mut stream = AiClient::new()
        .stream(
            &fixture_model(&format!("{}/v1beta/", cancel_server.uri())),
            plain_fixture_request(),
        )
        .await
        .expect("stream headers");
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::Started { .. }))
    ));
    // ResponseStream owns the HTTP body; dropping it is the provider-neutral
    // cancellation boundary and must not decode the trailing malformed frame.
    drop(stream);
}
