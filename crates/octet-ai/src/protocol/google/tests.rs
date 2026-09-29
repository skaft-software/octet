//! Unit tests for `crate::protocol::google`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `google.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::google`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::catalog::Model;
use crate::types::{
    AssistantMessage, Capabilities, Endpoint, EndpointId, ModalitySet, ModelId, ModelLimits,
    ModelSpec, OutputModalities, ToolCall, ToolCallId, ToolResult, ToolResultPart, UserMessage,
};

fn model() -> Model {
    model_with_api_name("gemini-2.5-flash")
}

fn model_with_api_name(api_name: &str) -> Model {
    Model {
        spec: std::sync::Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("gemini-test".to_owned()),
            api_name: api_name.to_owned(),
            display_name: None,
            endpoint: EndpointId("google".to_owned()),
            protocol: Protocol::GoogleGenerativeAi,
            capabilities: Capabilities {
                responses_features: Default::default(),
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
            cache: Default::default(),
        }),
        endpoint: std::sync::Arc::new(Endpoint {
            id: EndpointId("google".to_owned()),
            base_url: url::Url::parse("https://example.invalid/v1beta/").unwrap(),
            auth: crate::auth::Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: Default::default(),
            runtime: Default::default(),
            timeout: std::time::Duration::from_secs(10),
        }),
    }
}

#[test]
fn maps_structured_output_to_native_json_schema() {
    let mut request = Request {
        messages: Vec::new(),
        system: None,
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: Default::default(),
        output_format: OutputFormat::JsonSchema(crate::types::JsonSchemaFormat {
            name: "answer".to_owned(),
            description: None,
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        }),
        output_modalities: OutputModalities::Text,
        session_id: None,
        cache_retention: Default::default(),
        compatibility: Default::default(),
        responses: None,
    };
    let parts = build_request(&model(), &request).unwrap();
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        body["generationConfig"]["responseJsonSchema"]["type"],
        "object"
    );

    let mut zen = model();
    std::sync::Arc::make_mut(&mut zen.spec)
        .cache
        .send_session_affinity_headers = true;
    std::sync::Arc::make_mut(&mut zen.endpoint).id = EndpointId("opencode-google".into());
    request.cache_retention = crate::types::CacheRetention::None;
    request.session_id = Some("zen-session".into());
    assert_eq!(
        build_request(&zen, &request).unwrap().headers["x-opencode-session"],
        "zen-session"
    );
    request.session_id = None;
    let parts = build_request(&zen, &request).unwrap();
    assert!(parts.headers.get("x-opencode-session").is_none());
}

#[test]
fn uses_model_specific_google_tool_call_id_shape() {
    let request = Request {
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("call-1".to_owned()),
                    name: "lookup".to_owned(),
                    arguments_json: r#"{"city":"Paris"}"#.to_owned(),
                    argument_error: None,
                })],
                model: ModelId("gemini-test".to_owned()),
                protocol: Protocol::GoogleGenerativeAi,
            }),
            Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call-1".to_owned()),
                    content: vec![ToolResultPart::Text("Paris is sunny".to_owned())],
                    is_error: false,
                    added_tool_names: None,
                })],
            }),
        ],
        system: None,
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: Default::default(),
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        session_id: None,
        cache_retention: Default::default(),
        compatibility: Default::default(),
        responses: None,
    };

    let legacy = build_request(&model(), &request).unwrap();
    assert_eq!(
        legacy.url.path(),
        "/v1beta/models/gemini-2.5-flash:streamGenerateContent"
    );
    assert_eq!(legacy.url.query(), Some("alt=sse"));
    let legacy_body: Value = serde_json::from_slice(&legacy.body).unwrap();
    let legacy_call = &legacy_body["contents"][0]["parts"][0]["functionCall"];
    assert_eq!(legacy_call["name"], "lookup");
    assert!(legacy_call.get("id").is_none());
    let legacy_response = &legacy_body["contents"][1]["parts"][0]["functionResponse"];
    assert_eq!(legacy_response["response"]["output"], "Paris is sunny");
    assert!(legacy_response.get("id").is_none());

    let modern = build_request(&model_with_api_name("gemini-3-flash"), &request).unwrap();
    let modern_body: Value = serde_json::from_slice(&modern.body).unwrap();
    assert_eq!(
        modern_body["contents"][0]["parts"][0]["functionCall"]["id"],
        "call-1"
    );
    assert_eq!(
        modern_body["contents"][1]["parts"][0]["functionResponse"]["id"],
        "call-1"
    );
    assert!(!google_requires_tool_call_id("gemini-2.5-flash"));
    assert!(google_requires_tool_call_id("gemini-3-flash"));
    assert!(google_requires_tool_call_id("claude-3-7-sonnet"));
    assert!(!google_requires_tool_call_id("gemma-3-27b"));
}

#[test]
fn thought_signature_is_metadata_not_reasoning_classification() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"text":"visible","thoughtSignature":"opaque"}]},"finishReason":"STOP"}]}"#.to_owned(),
    };
    let events = decode_stream_event(&stream_model, &frame, &mut builder).unwrap();
    let response = events
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        response.message.content.as_slice(),
        [
            AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature { .. }),
            AssistantPart::Text(text)
        ] if text == "visible"
    ));
}

#[test]
fn incremental_argument_size_matches_recursive_merge_and_escaping() {
    let mut prior = serde_json::json!({});
    let mut size = argument_json_size(&prior).unwrap();
    for update in [
        serde_json::json!({"nested": {"a": "long prefix", "b": 1}, "array": [1, 2]}),
        serde_json::json!({"nested": {"a": "x", "quoted\"": "\n\t\"é"}}),
        serde_json::json!({"array": {"empty": {}}, "new": null}),
        serde_json::json!({"nested": false, "array": {"empty": {"x": 3}}}),
        serde_json::json!({}),
    ] {
        size = size
            .checked_add_signed(merged_size_change(&prior, &update).unwrap())
            .unwrap();
        merge_json_object(&mut prior, update);
        assert_eq!(size, serde_json::to_vec(&prior).unwrap().len());
    }
}

#[test]
fn argument_update_reserves_before_mutating_the_retained_object() {
    let mut builder =
        ResponseBuilder::new(model().spec.id.clone(), Protocol::GoogleGenerativeAi, None);
    let mut events = Vec::new();
    let call = |args| GoogleFunctionCall {
        id: None,
        name: "read".to_owned(),
        args,
    };
    decode_google_function_call(
        &mut events,
        &mut builder,
        0,
        call(serde_json::json!({"path": "a"})),
        None,
    )
    .unwrap();
    let prior = builder.google_function_args[&0].clone();
    assert_eq!(builder.buffered_content_bytes, prior.1);
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::ToolCallArgsDelta { .. })));
    builder.aggregate_content_bytes = crate::stream::MAX_RESPONSE_CONTENT_BYTES - prior.1;
    assert!(matches!(
        decode_google_function_call(
            &mut events,
            &mut builder,
            0,
            call(serde_json::json!({"line": 2})),
            None
        ),
        Err(AiError::Decode(DecodeError::ResponseTooLarge))
    ));
    assert_eq!(builder.google_function_args[&0], prior);
    assert_eq!(builder.buffered_content_bytes, prior.1);
}

#[test]
fn google_argument_size_enforces_the_tool_limit_before_retention() {
    assert!(matches!(
        argument_json_size(&"x".repeat(MAX_TOOL_ARGUMENT_BYTES)),
        Err(AiError::Decode(DecodeError::ToolArgumentsTooLarge))
    ));
}

#[test]
fn merges_cumulative_function_arguments_before_finish() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let first = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"read","args":{"path":"a"}}}]}}]}"#.to_owned(),
    };
    let last = SseEvent {
        event: None,
        data: r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"read","args":{"path":"a","line":2}}}]},"finishReason":"STOP"}]}"#.to_owned(),
    };
    decode_stream_event(&stream_model, &first, &mut builder).unwrap();
    let response = decode_stream_event(&stream_model, &last, &mut builder)
        .unwrap()
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Finished(response) => Some(response),
            _ => None,
        })
        .unwrap();
    assert!(matches!(response.stop_reason, StopReason::ToolUse));
    assert!(matches!(
        response.message.content.as_slice(),
        [AssistantPart::ToolCall(call)] if call.arguments_json == r#"{"line":2,"path":"a"}"#
    ));
}

#[test]
fn decodes_thought_usage_and_terminal_text() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"responseId":"response-1","candidates":[{"content":{"parts":[{"text":"private thought","thought":true,"thoughtSignature":"sig-1"},{"text":"visible answer"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"cachedContentTokenCount":2,"candidatesTokenCount":1,"thoughtsTokenCount":2,"totalTokenCount":0}}"#.to_owned(),
    };
    let events = decode_stream_event(&stream_model, &frame, &mut builder).unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ReasoningDelta { delta, .. } if delta == "private thought"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::TextDelta { delta, .. } if delta == "visible answer"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Usage(usage) if usage.input_tokens == 8
            && usage.cache_read_tokens == 2
            && usage.output_tokens == 3
            && usage.reasoning_tokens == 2
            && usage.total_tokens == 13
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Finished(response) if response.response_id.as_deref() == Some("response-1")
    )));
}

#[test]
fn reports_native_provider_errors_without_stream_events() {
    let stream_model = model();
    let mut builder = ResponseBuilder::new(
        stream_model.spec.id.clone(),
        Protocol::GoogleGenerativeAi,
        None,
    );
    let frame = SseEvent {
        event: None,
        data: r#"{"responseId":"response-error","error":{"code":400,"status":"INVALID_ARGUMENT","message":"invalid request"}}"#.to_owned(),
    };
    let error = decode_stream_event(&stream_model, &frame, &mut builder).unwrap_err();
    assert!(matches!(error, AiError::Provider(_)));
    assert!(!builder.started);
}
