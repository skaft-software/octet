#![allow(missing_docs)]

//! Deterministic native Conversations fixtures, derived from client-python
//! 3653cd9a5169fc151a0787232aed70eeea88e52a models. Not live qualification.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use octet_ai::{
    AiClient, AiError, AssistantMessage, AssistantPart, AudioFormat, AudioMedia,
    AudioOutputOptions, AudioPayload, AudioVoice, Auth, AuthError, CacheCompatibility,
    CacheRetention, Capabilities, CompatibilityMode, ConfigError, CredentialResolver, Endpoint,
    EndpointId, EndpointTransport, ImageMedia, ImageSource, Media, Message, ModalitySet, Model,
    ModelId, ModelLimits, ModelSpec, OutputFormat, OutputModalities, Protocol, ReasoningConfig,
    ReasoningMode, ReasoningPart, Request, ResolvedCredential, Response, StopReason, StreamEvent,
    StreamProtocolError, ToolCall, ToolCallArgumentError, ToolCallId, ToolChoice, ToolDef,
    ToolResult, ToolResultPart, UserMessage, UserPart,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct CountingCredentials(AtomicUsize);

#[async_trait::async_trait]
impl CredentialResolver for CountingCredentials {
    async fn resolve(&self) -> Result<ResolvedCredential, AuthError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(AuthError::Resolve)
    }
}

fn fixture_auth() -> Auth {
    Auth::bearer("fixture-secret")
}

fn fixture_model(base_url: &str, auth: Auth) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            id: ModelId("mistral-current-fixture".to_owned()),
            api_name: "mistral-fixture".to_owned(),
            display_name: None,
            endpoint: EndpointId("mistral-current".to_owned()),
            protocol: Protocol::MistralConversations,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: false,
                reasoning: None,
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
            id: EndpointId("mistral-current".to_owned()),
            base_url: url::Url::parse(base_url).expect("fixture URL"),
            auth,
            default_headers: http::HeaderMap::new(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(2),
        }),
    }
}

fn fixture_request(compatibility: CompatibilityMode, with_tools: bool) -> Request {
    Request {
        system: Some("request-private-instructions".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("request-private-input".to_owned())],
        })],
        tools: if with_tools {
            vec![ToolDef {
                constrained_sampling: None,
                name: "lookup".to_owned(),
                description: "Look up a city.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }),
            }]
        } else {
            Vec::new()
        },
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(128),
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}

fn sse(value: Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}

fn started() -> String {
    sse(json!({"type": "conversation.response.started", "conversation_id": "conv-fixture"}))
}

fn done() -> String {
    sse(json!({
        "type": "conversation.response.done",
        "usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17}
    }))
}

fn text_delta(content: Value, content_index: u64) -> String {
    sse(json!({
        "type": "message.output.delta", "id": "message-1", "role": "assistant",
        "output_index": 0, "content_index": content_index, "content": content
    }))
}

fn call_delta(output: u64, id: &str, arguments: &str) -> String {
    sse(json!({
        "type": "function.call.delta", "id": format!("entry-{output}"),
        "output_index": output, "tool_call_id": id, "name": "lookup", "arguments": arguments
    }))
}

fn text_stream() -> String {
    started()
        + &text_delta(json!("Bon"), 0)
        + &text_delta(json!({"type": "text", "text": "jour 🦀"}), 0)
        + &done()
}

async fn mount_native(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/conversations"))
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

fn assert_no_secret(error: &AiError) {
    for rendered in [error.to_string(), format!("{error:?}")] {
        for secret in ["request-private", "fixture-secret", "provider-private"] {
            assert!(!rendered.contains(secret), "error leaked fixture data");
        }
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
    // Invalid post-terminal data is ignored by the shared body reader.
    mount_native(&server, text_stream() + "data: not-json\n\n").await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
    let mut request = fixture_request(CompatibilityMode::Strict, false);
    request.temperature = Some(0.25);
    request.stop = vec!["STOP".into()];
    request.output_format = OutputFormat::JsonObject;
    request.session_id = Some("cache-session-not-a-conversation".into());
    request.cache_retention = CacheRetention::Long;
    let (events, error) = collect(&model, request).await;
    assert!(error.is_none(), "{error:?}");
    assert!(matches!(&events[0], StreamEvent::Started { response_id }
        if response_id.as_deref() == Some("conv-fixture")));
    assert!(matches!(&events[1], StreamEvent::TextStart { index: 0 }));
    assert!(matches!(&events[2], StreamEvent::TextDelta { index: 0, delta } if delta == "Bon"));
    assert!(matches!(&events[3], StreamEvent::TextDelta { index: 0, delta } if delta == "jour 🦀"));
    assert!(matches!(&events[4], StreamEvent::TextEnd { index: 0 }));
    assert!(matches!(&events[5], StreamEvent::Usage(_)));
    assert_eq!(events.len(), 7);
    let response = finished(&events);
    assert_eq!(response.message.protocol, Protocol::MistralConversations);
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert!(
        matches!(&response.message.content[..], [AssistantPart::Text(text)] if text == "Bonjour 🦀")
    );
    assert_eq!(response.usage.input_tokens, 12);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.usage.total_tokens, 17);
    assert_eq!(response.usage.cache_read_tokens, 0);
    assert_eq!(response.usage.cache_write_tokens, 0);
    assert_eq!(response.usage.reasoning_tokens, 0);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.fragment().is_none());
    assert!(requests[0].url.query().is_none());
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body,
        json!({
            "model": "mistral-fixture", "stream": true, "store": false,
            "handoff_execution": "client",
            "instructions": "request-private-instructions",
            "inputs": [{"object": "entry", "type": "message.input", "role": "user", "content": "request-private-input"}],
            "completion_args": {"max_tokens": 128, "temperature": 0.25, "stop": ["STOP"],
                "tool_choice": "auto", "response_format": {"type": "json_object"}}
        })
    );
    assert!(!body.to_string().contains("cache-session"));
}

#[tokio::test]
async fn interleaved_native_calls_round_trip_as_entries_with_exact_ids() {
    let server = MockServer::start().await;
    let wire = started()
        + &text_delta(json!("Looking"), 0)
        + &call_delta(1, "call:paris", r#"{"city":"Pa"#)
        + &text_delta(json!({"type": "text", "text": " up"}), 1)
        + &call_delta(2, "call:london", r#"{"city":"London"}"#)
        + &sse(
            json!({"type": "function.call.delta", "id": "entry-1", "output_index": 1,
            "tool_call_id": "", "name": "", "arguments": "r"}),
        )
        + &call_delta(1, "call:paris", r#"is"}"#)
        + &done();
    mount_native(&server, wire).await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
    let mut request = fixture_request(CompatibilityMode::Strict, true);
    request.tool_choice = ToolChoice::Required;
    let (events, error) = collect(&model, request.clone()).await;
    assert!(error.is_none(), "{error:?}");
    let response = finished(&events);
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    let calls: Vec<_> = response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(matches!(&response.message.content[..], [
        AssistantPart::Text(text), AssistantPart::ToolCall(_), AssistantPart::ToolCall(_)
    ] if text == "Looking up"));
    assert_eq!(calls[0].id.0, "call:paris");
    assert_eq!(
        calls[0].arguments_value().unwrap(),
        json!({"city": "Paris"})
    );
    assert_eq!(calls[1].id.0, "call:london");
    assert_eq!(
        calls[1].arguments_value().unwrap(),
        json!({"city": "London"})
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolCallStart { .. }))
            .count(),
        2
    );
    let last_delta = events
        .iter()
        .rposition(|event| matches!(event, StreamEvent::ToolCallArgsDelta { .. }))
        .unwrap();
    assert!(
        events
            .iter()
            .position(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
            .unwrap()
            > last_delta
    );
    let results = calls
        .iter()
        .map(|call| {
            UserPart::ToolResult(ToolResult {
                tool_call_id: call.id.clone(),
                content: vec![ToolResultPart::Text("found".into())],
                is_error: false,
                added_tool_names: None,
            })
        })
        .collect();
    request
        .messages
        .push(Message::Assistant(response.message.clone()));
    request
        .messages
        .push(Message::User(UserMessage { content: results }));
    let replay_server = MockServer::start().await;
    mount_native(&replay_server, text_stream()).await;
    let replay_model = fixture_model(&format!("{}/v1/", replay_server.uri()), fixture_auth());
    AiClient::new()
        .complete(&replay_model, request)
        .await
        .unwrap();
    let requests = replay_server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["tools"][0],
        json!({"type": "function", "function": {
            "name": "lookup", "description": "Look up a city.", "parameters": {
                "type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]
            }
        }})
    );
    assert_eq!(body["completion_args"]["tool_choice"], "required");
    assert_eq!(body["inputs"][1]["type"], "message.output");
    assert_eq!(body["inputs"][1]["content"], "Looking up");
    assert_eq!(body["inputs"][2]["type"], "function.call");
    let entries = body["inputs"].as_array().unwrap();
    for id in ["call:paris", "call:london"] {
        assert!(entries
            .iter()
            .any(|entry| entry["type"] == "function.call" && entry["tool_call_id"] == id));
        assert!(entries
            .iter()
            .any(|entry| entry["type"] == "function.result"
                && entry["tool_call_id"] == id
                && entry["result"] == "found"));
    }
    assert!(body.get("messages").is_none());
}

#[tokio::test]
async fn interrupted_native_history_repairs_results_without_changing_call_ids() {
    let server = MockServer::start().await;
    mount_native(&server, text_stream()).await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
    let mut request = fixture_request(CompatibilityMode::Strict, true);
    request.tool_choice = ToolChoice::None;
    request.messages.push(Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId("native:call/🦀".into()),
            name: "lookup".into(),
            arguments_json: r#"{ "city": "Paris" }"#.into(),
            argument_error: None,
        })],
        model: model.spec.id.clone(),
        protocol: Protocol::MistralConversations,
    }));
    AiClient::new().complete(&model, request).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["completion_args"]["tool_choice"], "none");
    assert_eq!(body["inputs"][1]["tool_call_id"], "native:call/🦀");
    assert_eq!(body["inputs"][1]["arguments"], r#"{ "city": "Paris" }"#);
    assert_eq!(
        body["inputs"][2],
        json!({
            "object": "entry", "type": "function.result", "tool_call_id": "native:call/🦀",
            "result": "Error: No result provided"
        })
    );
}

#[tokio::test]
async fn native_http_errors_preserve_status_without_client_replay() {
    for status in [401_u16, 422, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/conversations"))
            .and(header("accept", "text/event-stream"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("x-request-id", "request-id-fixture")
                    .set_body_json(json!({"detail": "fixture validation echoed fixture-secret"})),
            )
            .mount(&server)
            .await;
        let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
        let error = AiClient::new()
            .complete(&model, fixture_request(CompatibilityMode::Strict, false))
            .await
            .unwrap_err();
        assert_no_secret(&error);
        assert!(matches!(&error, AiError::Http(http_error)
            if http_error.status.as_u16() == status
                && http_error.request_id.as_deref() == Some("request-id-fixture")));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn native_calls_retain_schema_mismatch_without_execution_authority() {
    let server = MockServer::start().await;
    mount_native(
        &server,
        started() + &call_delta(1, "call-1", r#"{"city":42}"#) + &done(),
    )
    .await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
    let (events, error) = collect(&model, fixture_request(CompatibilityMode::Strict, true)).await;
    assert!(error.is_none(), "{error:?}");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallEnd {
            argument_error: Some(ToolCallArgumentError::SchemaMismatch),
            ..
        }
    )));
    assert!(
        matches!(&finished(&events).message.content[0], AssistantPart::ToolCall(call)
        if call.argument_error == Some(ToolCallArgumentError::SchemaMismatch))
    );
}

#[tokio::test]
async fn native_error_is_terminal_sanitized_and_preserves_progress() {
    for prefix in [String::new(), started() + &text_delta(json!("partial"), 0)] {
        let server = MockServer::start().await;
        mount_native(
            &server,
            prefix
                + &sse(json!({
                    "type": "conversation.response.error", "code": 429,
                    "message": "provider-private request-private fixture-secret"
                }))
                + &done(),
        )
        .await;
        let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
        let (events, error) =
            collect(&model, fixture_request(CompatibilityMode::Strict, false)).await;
        let error = error.unwrap();
        assert_no_secret(&error);
        assert!(matches!(root_error(&error), AiError::Provider(provider)
            if provider.code.as_deref() == Some("429")
                && provider.kind.as_deref() == Some("conversation.response.error")));
        let AiError::StreamFailure { progress, .. } = &error else {
            panic!("missing stream progress")
        };
        assert!(progress.first_body_seen);
        assert!(progress.provider_events >= 1);
        assert!(!events.iter().any(|event| matches!(
            event,
            StreamEvent::Finished(_) | StreamEvent::ToolCallEnd { .. }
        )));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn native_stream_rejects_malformed_foreign_and_unterminated_events() {
    let cases = vec![
        started() + "data: [DONE]\n\n",
        started() + "data: {\n\n",
        started() + "data: {\"choices\":[]}\n\n",
        started() + &sse(json!({"type": "response.completed"})),
        started() + &sse(json!({"type": "function.result", "tool_call_id": "call-1", "result": "not a server event"})),
        started() + "event: message.output.delta\ndata: {\"type\":\"conversation.response.done\",\"usage\":{}}\n\n",
        started() + &started(),
        done(),
        text_delta(json!("missing start"), 0),
        started() + &sse(json!({"type": "conversation.response.done"})),
        started() + &sse(json!({"type": "conversation.response.done", "usage": {"prompt_tokens": -1}})),
        started() + &call_delta(1, "call-1", "{\"city\":") + &done(),
        started() + &call_delta(1, "call-1", "[]") + &done(),
        started() + &call_delta(1, "call-1", "") + &done(),
        started() + &sse(json!({"type": "tool.execution.started", "id": "entry-1", "output_index": 1,
            "name": "web_search", "arguments": "{}"})) + &call_delta(1, "call-1", "{}") + &done(),
        started() + &sse(json!({"type": "agent.handoff.started", "id": "entry-1", "output_index": 1,
            "previous_agent_id": "agent-1", "previous_agent_name": "first"})) + &call_delta(1, "call-1", "{}") + &done(),
        started() + &call_delta(1, "call-1", "{}") + &call_delta(1, "changed", "") + &done(),
        started() + &call_delta(1, "same", "{}") + &call_delta(2, "same", "{}") + &done(),
        started() + &call_delta(2, "later", "{}") + &call_delta(1, "earlier", "{}") + &done(),
        started() + &sse(json!({"type": "function.call.delta", "id": "entry-1", "name": "lookup",
            "tool_call_id": "call-1", "arguments": "{}", "confirmation_status": "allowed"})) + &done(),
        started() + &sse(json!({"type": "function.call.delta", "id": "entry-1", "name": "lookup",
            "tool_call_id": "call-1", "arguments": "{}", "confirmation_status": "denied"})) + &done(),
        started() + &sse(json!({"type": "function.call.delta", "id": "entry-1", "name": "lookup",
            "tool_call_id": "call-1", "arguments": "{}", "confirmation_status": "pending"})) + &done(),
        started() + &text_delta(json!("truncated"), 0),
    ];
    for wire in cases {
        let server = MockServer::start().await;
        mount_native(&server, wire).await;
        let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
        for mode in [CompatibilityMode::Strict, CompatibilityMode::Lossy] {
            let (events, error) = collect(&model, fixture_request(mode, true)).await;
            let error = error.expect("invalid native stream must fail in both modes");
            assert!(matches!(
                root_error(&error),
                AiError::Decode(_) | AiError::StreamProtocol(_)
            ));
            assert_no_secret(&error);
            assert!(!events.iter().any(|event| matches!(
                event,
                StreamEvent::Finished(_) | StreamEvent::ToolCallEnd { .. }
            )));
        }
    }
}

#[tokio::test]
async fn eof_is_not_native_done_even_after_valid_arguments() {
    let server = MockServer::start().await;
    mount_native(
        &server,
        started() + &call_delta(1, "call-1", r#"{"city":"Paris"}"#),
    )
    .await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
    let (events, error) = collect(&model, fixture_request(CompatibilityMode::Strict, true)).await;
    assert!(matches!(
        root_error(&error.unwrap()),
        AiError::StreamProtocol(StreamProtocolError::MissingFinish)
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::ToolCallEnd { .. })));
}

#[tokio::test]
async fn server_tools_handoffs_and_nontext_are_never_local_calls() {
    let unsupported = [
        (
            json!({"type": "tool.execution.started", "id": "server-tool", "name": "web_search", "arguments": "{}"}),
            "dropped_mistral_server_tool",
        ),
        (
            json!({"type": "tool.execution.delta", "id": "server-tool", "name": "web_search", "arguments": "{}"}),
            "dropped_mistral_server_tool",
        ),
        (
            json!({"type": "tool.execution.done", "id": "server-tool", "name": "web_search"}),
            "dropped_mistral_server_tool",
        ),
        (
            json!({"type": "agent.handoff.started", "id": "handoff", "previous_agent_id": "agent-1", "previous_agent_name": "first"}),
            "dropped_mistral_handoff",
        ),
        (
            json!({"type": "agent.handoff.done", "id": "handoff", "next_agent_id": "agent-2", "next_agent_name": "second"}),
            "dropped_mistral_handoff",
        ),
        (
            json!({"type": "message.output.delta", "id": "message-1", "role": "assistant", "content": {"type": "image_url", "image_url": "https://invalid.example/provider-private.png"}}),
            "dropped_mistral_content",
        ),
        (
            json!({"type": "message.output.delta", "id": "message-1", "role": "assistant", "content": {"type": "thinking", "thinking": [{"type": "text", "text": "provider-private"}]}}),
            "dropped_mistral_content",
        ),
    ];
    for (event, code) in unsupported {
        let server = MockServer::start().await;
        mount_native(&server, started() + &sse(event) + &done()).await;
        let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
        let (_, error) = collect(&model, fixture_request(CompatibilityMode::Strict, true)).await;
        assert_no_secret(&error.unwrap());
        let (events, error) =
            collect(&model, fixture_request(CompatibilityMode::Lossy, true)).await;
        assert!(error.is_none(), "{error:?}");
        assert!(finished(&events).message.content.is_empty());
        assert!(finished(&events)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == code));
        assert!(!events.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallStart { .. }
                | StreamEvent::ReasoningStart { .. }
                | StreamEvent::MediaCompleted { .. }
        )));
    }
}

#[tokio::test]
async fn native_usage_distinguishes_omission_from_invalid_model_counters() {
    for usage in [
        json!({}),
        json!({"connector_tokens": null, "connectors": null}),
    ] {
        let server = MockServer::start().await;
        mount_native(
            &server,
            started()
                + &sse(json!({
                    "type": "conversation.response.done", "usage": usage
                })),
        )
        .await;
        let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
        for mode in [CompatibilityMode::Strict, CompatibilityMode::Lossy] {
            let (events, error) = collect(&model, fixture_request(mode, false)).await;
            assert!(error.is_none(), "{error:?}");
            let response = finished(&events);
            assert_eq!(response.usage.input_tokens, 0);
            assert_eq!(response.usage.output_tokens, 0);
            assert_eq!(response.usage.total_tokens, 0);
            assert!(!response
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "dropped_mistral_connector_usage"));
        }
    }
    for field in [
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "connector_tokens",
    ] {
        for invalid in [Value::Null, json!(-1), json!(1.5), json!("5")] {
            if field == "connector_tokens" && invalid.is_null() {
                continue; // Explicitly nullable; covered by the successful fixture above.
            }
            let mut usage = json!({});
            usage[field] = invalid;
            let server = MockServer::start().await;
            mount_native(
                &server,
                started()
                    + &sse(json!({
                        "type": "conversation.response.done", "usage": usage
                    })),
            )
            .await;
            let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
            for mode in [CompatibilityMode::Strict, CompatibilityMode::Lossy] {
                let (events, error) = collect(&model, fixture_request(mode, false)).await;
                assert!(matches!(
                    root_error(&error.expect("invalid counter")),
                    AiError::Decode(_)
                ));
                assert!(!events.iter().any(|event| matches!(
                    event,
                    StreamEvent::Usage(_) | StreamEvent::Finished(_)
                )));
            }
        }
    }
}

#[tokio::test]
async fn connector_usage_is_not_reclassified_as_model_or_cache_tokens() {
    let server = MockServer::start().await;
    mount_native(&server, started() + &sse(json!({
        "type": "conversation.response.done", "usage": {"prompt_tokens": 12, "completion_tokens": 5,
            "total_tokens": 20, "connector_tokens": 3, "connectors": {"web_search": 1}}
    }))).await;
    let model = fixture_model(&format!("{}/v1/", server.uri()), fixture_auth());
    let (_, error) = collect(&model, fixture_request(CompatibilityMode::Strict, false)).await;
    assert!(error.is_some());
    let (events, error) = collect(&model, fixture_request(CompatibilityMode::Lossy, false)).await;
    assert!(error.is_none(), "{error:?}");
    let response = finished(&events);
    assert_eq!(response.usage.input_tokens, 12);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.usage.total_tokens, 20);
    assert_eq!(response.usage.cache_read_tokens, 0);
    assert_eq!(response.usage.reasoning_tokens, 0);
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "dropped_mistral_connector_usage"));
}

fn private_image() -> Media {
    Media::Image(ImageMedia {
        source: ImageSource::Url(
            url::Url::parse("https://invalid.example/request-private.png").unwrap(),
        ),
        media_type: None,
        detail: None,
    })
}

#[tokio::test]
async fn explicit_controls_reject_before_credentials_in_both_modes() {
    let server = MockServer::start().await;
    let credentials = Arc::new(CountingCredentials::default());
    let model = fixture_model(
        &format!("{}/v1/", server.uri()),
        Auth::dynamic(credentials.clone()),
    );
    for mode in [CompatibilityMode::Strict, CompatibilityMode::Lossy] {
        for control in ["named", "reasoning", "mode"] {
            let mut request = fixture_request(mode, true);
            match control {
                "named" => request.tool_choice = ToolChoice::Named("lookup".into()),
                "reasoning" => request.reasoning = ReasoningConfig::Budget(64),
                "mode" => request.reasoning_mode = ReasoningMode::Pro,
                _ => unreachable!(),
            }
            let error = AiClient::new().complete(&model, request).await.unwrap_err();
            assert!(matches!(error, AiError::Unsupported(_)));
            assert_no_secret(&error);
        }
    }
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_request_media_and_reasoning_are_explicit_not_replay_placeholders() {
    let strict_server = MockServer::start().await;
    let credentials = Arc::new(CountingCredentials::default());
    let strict_model = fixture_model(
        &format!("{}/v1/", strict_server.uri()),
        Auth::dynamic(credentials.clone()),
    );
    let lossy_server = MockServer::start().await;
    mount_native(&lossy_server, text_stream()).await;
    let lossy_model = fixture_model(&format!("{}/v1/", lossy_server.uri()), fixture_auth());
    for (case, code) in [
        ("user_image", "dropped_image"),
        ("assistant_image", "dropped_image"),
        ("reasoning", "dropped_reasoning_state"),
        ("tool_media", "dropped_tool_result_media"),
        ("user_audio", "dropped_audio"),
        ("audio_output", "dropped_audio_output"),
    ] {
        let mut request = fixture_request(CompatibilityMode::Strict, false);
        let mut history = Vec::new();
        match case {
            "user_image" | "user_audio" => {
                let media = if case == "user_image" {
                    private_image()
                } else {
                    Media::Audio(AudioMedia {
                        payload: AudioPayload::Inline(bytes::Bytes::from_static(b"private-audio")),
                        format: AudioFormat::Wav,
                        transcript: Some("request-private-transcript".into()),
                    })
                };
                let Message::User(user) = &mut request.messages[0] else {
                    unreachable!()
                };
                user.content.push(UserPart::Media(media));
            }
            "assistant_image" => history.push(AssistantPart::Media(private_image())),
            "reasoning" => history.push(AssistantPart::Reasoning(ReasoningPart {
                text: Some("request-private-reasoning".into()),
                state: None,
            })),
            "tool_media" => history.push(AssistantPart::ToolCall(ToolCall {
                id: ToolCallId("history:call".into()),
                name: "lookup".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            })),
            "audio_output" => {
                request.output_modalities = OutputModalities::TextAndAudio(AudioOutputOptions {
                    format: AudioFormat::Wav,
                    voice: AudioVoice::Named("request-private-voice".into()),
                })
            }
            _ => unreachable!(),
        }
        if !history.is_empty() {
            history.push(AssistantPart::Text("history".into()));
            request.messages.push(Message::Assistant(AssistantMessage {
                content: history,
                model: strict_model.spec.id.clone(),
                protocol: Protocol::MistralConversations,
            }));
        }
        if case == "tool_media" {
            request.messages.push(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("history:call".into()),
                    content: vec![
                        ToolResultPart::Text("visible".into()),
                        ToolResultPart::Media(private_image()),
                    ],
                    is_error: true,
                    added_tool_names: None,
                })],
            }));
        }
        let error = AiClient::new()
            .complete(&strict_model, request.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(error, AiError::Unsupported(_)),
            "{case}: {error:?}"
        );
        assert_no_secret(&error);
        request.compatibility = CompatibilityMode::Lossy;
        let response = AiClient::new()
            .complete(&lossy_model, request)
            .await
            .unwrap();
        assert!(
            response
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == code),
            "{case}"
        );
    }
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
    assert!(strict_server.received_requests().await.unwrap().is_empty());
    let requests = lossy_server.received_requests().await.unwrap();
    for request in &requests {
        let body = String::from_utf8(request.body.clone()).unwrap();
        for omitted in [
            "request-private.png",
            "request-private-reasoning",
            "request-private-transcript",
            "request-private-voice",
            "image omitted",
            "audio omitted",
        ] {
            assert!(!body.contains(omitted));
        }
    }
    let tool_media: Value = serde_json::from_slice(&requests[3].body).unwrap();
    assert!(tool_media["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["type"] == "function.result"
            && entry["tool_call_id"] == "history:call"
            && entry["result"] == "Error: visible"));
}

#[tokio::test]
async fn invalid_destinations_reject_before_credentials_without_echoing_url_secrets() {
    let credentials = Arc::new(CountingCredentials::default());
    for base in [
        "http://example.com/v1/",
        "https://fixture-secret@example.com/v1/",
        "https://example.com/v1/?api_key=fixture-secret",
        "https://example.com/v1/#fixture-secret",
    ] {
        let model = fixture_model(base, Auth::dynamic(credentials.clone()));
        let error = AiClient::new()
            .complete(&model, fixture_request(CompatibilityMode::Strict, false))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AiError::Config(ConfigError::InvalidBaseUrl(_))
        ));
        assert_no_secret(&error);
    }
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dropping_native_stream_closes_http_body_without_replaying_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(request.starts_with(b"POST /v1/conversations HTTP/1.1\r\n"));
        let event = started() + &call_delta(1, "call-cancelled", "{\"city\":");
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n", event.len()).as_bytes()).await.unwrap();
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let model = fixture_model(&format!("http://{address}/v1/"), fixture_auth());
    let client = AiClient::new();
    let mut stream = client
        .stream(&model, fixture_request(CompatibilityMode::Strict, true))
        .await
        .unwrap();
    loop {
        let event = stream.next().await.unwrap().unwrap();
        assert!(!matches!(
            event,
            StreamEvent::ToolCallEnd { .. } | StreamEvent::Finished(_)
        ));
        if matches!(event, StreamEvent::ToolCallArgsDelta { .. }) {
            break;
        }
    }
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("body was not closed")
        .unwrap();
}

#[test]
fn mistral_current_protocol_identity_is_not_openai_chat() {
    let encoded = serde_json::to_string(&Protocol::MistralConversations).unwrap();
    assert_eq!(encoded, r#""mistral_conversations""#);
    let decoded: Protocol = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, Protocol::MistralConversations);
    assert_ne!(decoded, Protocol::OpenAiChat);
}
