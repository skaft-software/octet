#![allow(missing_docs)]

//! Deterministic native `pi-messages` (Radius gateway) fixtures.
//!
//! These drive the codec through the real [`octet_ai::AiClient`] request and
//! stream dispatch, so they fail if the `Protocol::PiMessages` route is ever
//! un-wired from the client instead of only if the codec's own unit tests break.
//! Not live qualification.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use octet_ai::{
    AiClient, AiError, AssistantPart, Auth, CacheCompatibility, CacheRetention, Capabilities,
    CompatibilityMode, Endpoint, EndpointId, EndpointTransport, Message, ModalitySet, Model,
    ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities, Protocol, ReasoningCapability,
    ReasoningConfig, ReasoningEffort, ReasoningMode, Request, Response, StopReason, StreamEvent,
    StreamProtocolError, ToolChoice, ToolDef, UserMessage, UserPart,
};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_auth() -> Auth {
    Auth::bearer("fixture-secret")
}

/// Pi's `thinkingLevel` is an effort control, not a token budget.
fn reasoning_capability() -> ReasoningCapability {
    serde_json::from_value(json!({
        "control": "effort",
        "exposes_text": true,
        "preserves_state": true,
        "min_effort": "low",
        "max_effort": "high"
    }))
    .expect("reasoning capability")
}

fn fixture_model(base_url: &str, reasoning: bool) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("pi-messages-fixture".to_owned()),
            api_name: "radius-fixture".to_owned(),
            display_name: None,
            endpoint: EndpointId("pi-messages".to_owned()),
            protocol: Protocol::PiMessages,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: reasoning.then(reasoning_capability),
                responses_lite: false,
                agent_delegation: None,
                structured_output: true,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 4096,
                max_output_tokens: 512,
            },
            pricing: None,
            cache: CacheCompatibility::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("pi-messages".to_owned()),
            base_url: url::Url::parse(base_url).expect("fixture URL"),
            auth: fixture_auth(),
            default_headers: http::HeaderMap::new(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(2),
        }),
    }
}

fn fixture_request(reasoning: ReasoningConfig) -> Request {
    Request {
        system: Some("request-private-instructions".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("request-private-input".to_owned())],
        })],
        tools: vec![ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".to_owned(),
            description: "Look up a city.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(64),
        temperature: Some(0.25),
        stop: Vec::new(),
        reasoning,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: Some("session-fixture".to_owned()),
    }
}

fn sse(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn text_stream() -> String {
    sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "lo"}))
        + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}))
        + &sse(json!({
            "type": "done", "reason": "stop", "responseId": "resp_fixture",
            "providerThinkingLevel": "high",
            "rewrite": {"policyId": "gateway-policy", "policyVersion": 3, "changed": true},
            "usage": {"input": 12, "output": 5, "cacheRead": 3, "cacheWrite": 0, "totalTokens": 20}
        }))
}

async fn mount_native(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("accept", "text/event-stream"))
        .and(header("content-type", "application/json"))
        .and(header("authorization", "Bearer fixture-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn collect(model: &Model, request: Request) -> (Vec<StreamEvent>, Option<AiError>) {
    let mut stream = AiClient::new().stream(model, request).await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => events.push(event),
            Err(error) => {
                assert!(stream.next().await.is_none());
                return (events, Some(error));
            }
        }
    }
    (events, None)
}

fn root_error(error: &AiError) -> &AiError {
    match error {
        AiError::StreamFailure { inner, .. } => root_error(inner),
        error => error,
    }
}

fn finished(events: &[StreamEvent]) -> &Response {
    match events.last().expect("terminal event") {
        StreamEvent::Finished(response) => response,
        event => panic!("expected Finished, got {event:?}"),
    }
}

#[tokio::test]
async fn native_text_request_sse_usage_and_terminal_fixture() {
    let server = MockServer::start().await;
    // Post-terminal data is ignored by the shared body reader.
    mount_native(&server, text_stream() + "data: not-json\n\n").await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), false);
    let (events, error) = collect(&model, fixture_request(ReasoningConfig::Off)).await;
    assert!(error.is_none(), "{error:?}");
    assert!(matches!(&events[0], StreamEvent::Started { response_id } if response_id.is_none()));
    assert!(matches!(&events[1], StreamEvent::TextStart { index: 0 }));
    assert!(matches!(&events[2], StreamEvent::TextDelta { index: 0, delta } if delta == "Hel"));
    assert!(matches!(&events[3], StreamEvent::TextDelta { index: 0, delta } if delta == "lo"));
    assert!(matches!(&events[4], StreamEvent::TextEnd { index: 0 }));
    assert!(matches!(&events[5], StreamEvent::Usage(_)));
    assert_eq!(events.len(), 7);
    let response = finished(&events);
    assert_eq!(response.message.protocol, Protocol::PiMessages);
    assert_eq!(response.response_id.as_deref(), Some("resp_fixture"));
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(response.usage.input_tokens, 12);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.usage.cache_read_tokens, 3);
    assert_eq!(response.usage.total_tokens, 20);
    assert!(
        matches!(&response.message.content[..], [AssistantPart::Text(text)] if text == "Hello")
    );
    let codes: Vec<&str> = response
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert!(codes.contains(&"pi_messages_provider_thinking_level"));
    assert!(codes.contains(&"pi_messages_rewrite"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/v1/messages");
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "radius-fixture");
    assert_eq!(
        body["context"]["systemPrompt"],
        "request-private-instructions"
    );
    assert_eq!(body["context"]["messages"][0]["role"], "user");
    assert_eq!(
        body["context"]["messages"][0]["content"],
        json!([{"type": "text", "text": "request-private-input"}])
    );
    assert!(body["context"]["messages"][0]["timestamp"].is_u64());
    assert_eq!(body["context"]["tools"][0]["name"], "lookup");
    assert_eq!(
        body["options"],
        json!({
            "temperature": 0.25,
            "maxTokens": 64,
            "cacheRetention": "short",
            "sessionId": "session-fixture",
            "toolChoice": "auto",
        })
    );
}

#[tokio::test]
async fn effort_reasoning_is_admitted_and_encoded_as_a_thinking_level() {
    let server = MockServer::start().await;
    mount_native(&server, text_stream()).await;
    // Catalog validation must admit an effort control on this route, and the
    // codec must encode it as Pi's own thinking level.
    let model = fixture_model(&format!("{}/v1/", server.uri()), true);
    let mut request = fixture_request(ReasoningConfig::Effort(ReasoningEffort::High));
    request.temperature = None;
    let (events, error) = collect(&model, request).await;
    assert!(error.is_none(), "{error:?}");
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["options"]["reasoning"], "high");
    assert!(body["options"].get("temperature").is_none());
}

#[tokio::test]
async fn terminal_tool_call_replaces_the_streamed_preview() {
    let server = MockServer::start().await;
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({
            "type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "lookup"
        }))
        + &sse(json!({"type": "toolcall_delta", "contentIndex": 0, "delta": "{\"city\":"}))
        + &sse(json!({
            "type": "toolcall_end", "contentIndex": 0,
            "toolCall": {"type": "toolCall", "id": "call_1", "name": "lookup",
                "arguments": {"city": "Paris"}}
        }))
        + &sse(json!({
            "type": "done", "reason": "toolUse",
            "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}
        }));
    mount_native(&server, wire).await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), false);
    let (events, error) = collect(&model, fixture_request(ReasoningConfig::Off)).await;
    assert!(error.is_none(), "{error:?}");
    let response = finished(&events);
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(
        matches!(&response.message.content[..], [AssistantPart::ToolCall(call)]
        if call.id.0 == "call_1" && call.arguments_value().unwrap() == json!({"city": "Paris"}))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_body_without_a_native_terminal_is_a_missing_finish() {
    let server = MockServer::start().await;
    // `done`/`error` are the only terminals; a body that closes first must not
    // read as a settled turn even when its deltas are already complete.
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "partial"}));
    mount_native(&server, wire).await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), false);
    let (_, error) = collect(&model, fixture_request(ReasoningConfig::Off)).await;
    let error = error.expect("a pi-messages body must settle on its own terminal event");
    let error = root_error(&error);
    assert!(
        matches!(
            error,
            AiError::StreamProtocol(StreamProtocolError::MissingFinish)
        ),
        "{error:?}"
    );
}
