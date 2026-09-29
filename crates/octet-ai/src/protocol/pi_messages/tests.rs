//! Unit tests for `crate::protocol::pi_messages`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `pi_messages.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::pi_messages`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use std::sync::Arc;

use crate::protocol::harness::{drive, model as harness_model};
use crate::types::{
    AssistantMessage, AssistantPart, ModelId, Protocol, ReasoningPart, ToolCall, ToolDef,
    UserMessage, UserPart,
};

fn fixture_model() -> crate::catalog::Model {
    harness_model(Protocol::OpenAiChat, None)
}

fn sse(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

fn request() -> Request {
    Request {
        system: Some("system-private".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("hello".to_owned())],
        })],
        tools: tools(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: Some(64),
        temperature: Some(0.5),
        stop: Vec::new(),
        reasoning: ReasoningConfig::Effort(crate::types::ReasoningEffort::High),
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: Some("session-1".to_owned()),
    }
}

fn tools() -> Vec<ToolDef> {
    vec![ToolDef {
        async_execution: false,
        name: "lookup".to_owned(),
        description: "Look up a city.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
        constrained_sampling: None,
    }]
}

#[test]
fn terminal_only_tool_arguments_obey_aggregate_response_limit() {
    let model = fixture_model();
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, None);
    let event = |value: Value| crate::protocol::sse::SseEvent {
        event: None,
        data: value.to_string(),
    };
    decode_stream_event(&model, &event(json!({"type":"start"})), &mut builder).unwrap();
    decode_stream_event(
        &model,
        &event(json!({"type":"toolcall_start", "contentIndex":0,
        "id":"call", "toolName":"lookup"})),
        &mut builder,
    )
    .unwrap();
    builder
        .reserve_buffered_content(
            crate::stream::MAX_RESPONSE_CONTENT_BYTES - builder.aggregate_content_bytes - 2,
        )
        .unwrap();
    let before = builder.aggregate_content_bytes;
    let result = decode_stream_event(
        &model,
        &event(json!({"type":"toolcall_end", "contentIndex":0,
        "toolCall":{"id":"call", "name":"lookup", "arguments":{"city":"Paris"}}})),
        &mut builder,
    );
    assert!(matches!(
        result,
        Err(AiError::Decode(crate::error::DecodeError::ResponseTooLarge))
    ));
    assert_eq!(builder.aggregate_content_bytes, before);
    assert!(builder.tool_call_builders[&0].arguments_json.is_empty());
}

#[test]
fn request_is_one_messages_post_with_a_native_context_document() {
    let model = fixture_model();
    let parts = build_request(&model, &request()).unwrap();
    assert!(parts.streaming);
    assert_eq!(parts.url.as_str(), "https://api.example.test/v1/messages");
    assert_eq!(parts.headers["accept"], "text/event-stream");
    assert_eq!(parts.headers["content-type"], "application/json");
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    assert_eq!(body["model"], "fixture-api-name");
    assert_eq!(body["context"]["systemPrompt"], "system-private");
    assert_eq!(body["context"]["messages"][0]["role"], "user");
    assert_eq!(
        body["context"]["messages"][0]["content"],
        json!([{"type": "text", "text": "hello"}])
    );
    assert!(body["context"]["messages"][0]["timestamp"].is_u64());
    assert_eq!(body["context"]["tools"][0]["name"], "lookup");
    assert_eq!(
        body["context"]["tools"][0]["parameters"]["properties"]["city"]["type"],
        "string"
    );
    assert_eq!(
        body["options"],
        json!({
            "temperature": 0.5,
            "maxTokens": 64,
            "reasoning": "high",
            "cacheRetention": "short",
            "sessionId": "session-1",
            "toolChoice": "auto",
        })
    );
    assert!(parts.diagnostics.is_empty());
}

#[test]
fn strict_mode_rejects_wire_inexpressible_controls_and_lossy_drops_them() {
    let model = fixture_model();
    let mut strict = request();
    strict.stop = vec!["STOP".to_owned()];
    strict.output_format = OutputFormat::JsonObject;
    assert!(build_request(&model, &strict).is_err());

    let mut lossy = strict.clone();
    lossy.compatibility = CompatibilityMode::Lossy;
    let parts = build_request(&model, &lossy).unwrap();
    let codes: Vec<&str> = parts
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert!(codes.contains(&"dropped_stop_sequences"));
    assert!(codes.contains(&"dropped_structured_output"));

    let mut budget_model = fixture_model();
    Arc::make_mut(&mut budget_model.spec)
        .capabilities
        .reasoning
        .as_mut()
        .unwrap()
        .control = crate::types::ReasoningControl::TokenBudget;
    let mut budget = request();
    // The budget must be a *valid* size for the request (>= 1024 and below
    // the output allowance) so the codec's "no native budget field"
    // rejection is what this exercises, not the generic range validation.
    budget.max_output_tokens = Some(8192);
    budget.reasoning = ReasoningConfig::Budget(4096);
    assert!(build_request(&budget_model, &budget).is_err());
    budget.compatibility = CompatibilityMode::Lossy;
    let parts = build_request(&budget_model, &budget).unwrap();
    assert!(parts
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "dropped_reasoning_budget"));
    assert!(!String::from_utf8(parts.body.to_vec())
        .unwrap()
        .contains("\"reasoning\""));
}

#[test]
fn base_url_must_be_tls_and_credential_free() {
    let mut model = fixture_model();
    Arc::make_mut(&mut model.endpoint).base_url =
        url::Url::parse("http://api.example.test/v1/").unwrap();
    assert!(build_request(&model, &request()).is_err());
    let mut loopback = fixture_model();
    Arc::make_mut(&mut loopback.endpoint).base_url =
        url::Url::parse("http://127.0.0.1:8080/v1/").unwrap();
    assert!(build_request(&loopback, &request()).is_ok());
    let mut userinfo = fixture_model();
    Arc::make_mut(&mut userinfo.endpoint).base_url =
        url::Url::parse("https://user:secret@api.example.test/v1/").unwrap();
    assert!(build_request(&userinfo, &request()).is_err());
}

#[test]
fn replayed_context_keeps_reasoning_signatures_and_tool_identity() {
    // The codec under test is the pi-messages route: a route-native
    // reasoning state (the codec stores the provider's opaque continuation
    // payload with its own protocol/model) must survive validation.
    let mut model = fixture_model();
    Arc::make_mut(&mut model.spec).protocol = Protocol::PiMessages;
    let mut req = request();
    req.reasoning = ReasoningConfig::Off;
    req.messages.push(Message::Assistant(AssistantMessage {
        content: vec![
            AssistantPart::Reasoning(ReasoningPart {
                text: Some("prior thinking".to_owned()),
                state: Some(ReasoningState {
                    protocol: Protocol::PiMessages,
                    model: ModelId("fixture-model".to_owned()),
                    kind: ReasoningStateKind::AnthropicSignature {
                        signature: "opaque-signature".to_owned(),
                    },
                }),
            }),
            AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId("call_1".to_owned()),
                name: "lookup".to_owned(),
                arguments_json: r#"{"city":"Paris"}"#.to_owned(),
                argument_error: None,
            }),
        ],
        model: ModelId("fixture-model".to_owned()),
        protocol: Protocol::PiMessages,
    }));
    req.messages.push(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(crate::types::ToolResult {
            tool_call_id: ToolCallId("call_1".to_owned()),
            content: vec![ToolResultPart::Text("found".to_owned())],
            is_error: false,
            added_tool_names: Some(vec!["extra".to_owned()]),
        })],
    }));
    let parts = build_request(&model, &req).unwrap();
    let body: Value = serde_json::from_slice(&parts.body).unwrap();
    let messages = body["context"]["messages"].as_array().unwrap();
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["content"][0],
        json!({"type": "thinking", "thinking": "prior thinking", "thinkingSignature": "opaque-signature"})
    );
    assert_eq!(
        messages[1]["content"][1],
        json!({"type": "toolCall", "id": "call_1", "name": "lookup", "arguments": {"city": "Paris"}})
    );
    assert_eq!(messages[1]["stopReason"], "toolUse");
    assert_eq!(messages[2]["role"], "toolResult");
    assert_eq!(messages[2]["toolCallId"], "call_1");
    assert_eq!(messages[2]["toolName"], "lookup");
    assert_eq!(
        messages[2]["content"][0],
        json!({"type": "text", "text": "found"})
    );
    assert_eq!(messages[2]["addedToolNames"], json!(["extra"]));
}

#[tokio::test]
async fn text_stream_settles_with_terminal_usage_and_response_id() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "lo"}))
        + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}))
        + &sse(json!({
            "type": "done", "reason": "stop", "responseId": "resp_1",
            "providerThinkingLevel": "high",
            "usage": {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15}
        }));
    let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap();
    assert!(matches!(&events[0], StreamEvent::Started { response_id } if response_id.is_none()));
    assert!(matches!(&events[2], StreamEvent::TextDelta { delta, .. } if delta == "Hel"));
    let response = match events.last().unwrap() {
        StreamEvent::Finished(response) => response,
        other => panic!("expected Finished, got {other:?}"),
    };
    assert_eq!(response.response_id.as_deref(), Some("resp_1"));
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(response.usage.input_tokens, 10);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.usage.total_tokens, 15);
    assert!(
        matches!(&response.message.content[..], [AssistantPart::Text(text)] if text == "Hello")
    );
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "pi_messages_provider_thinking_level"));
}

#[tokio::test]
async fn terminal_text_completes_missing_suffix_but_never_splices() {
    let model = fixture_model();
    let ok = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
        + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}))
        + &sse(
            json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
        );
    let events = drive(&model, decode_stream_event, ok.as_bytes(), 0)
        .await
        .unwrap();
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "lo")));

    let bad = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
        + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "World"}))
        + &sse(
            json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
        );
    assert!(drive(&model, decode_stream_event, bad.as_bytes(), 0)
        .await
        .is_err());
}

#[tokio::test]
async fn thinking_end_retains_opaque_reasoning_state() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({"type": "thinking_start", "contentIndex": 0}))
        + &sse(json!({"type": "thinking_delta", "contentIndex": 0, "delta": "think"}))
        + &sse(json!({
            "type": "thinking_end", "contentIndex": 0, "content": "thinking",
            "contentSignature": "sig-1"
        }))
        + &sse(
            json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
        );
    let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap();
    let response = match events.last().unwrap() {
        StreamEvent::Finished(response) => response,
        other => panic!("expected Finished, got {other:?}"),
    };
    let AssistantPart::Reasoning(reasoning) = &response.message.content[0] else {
        panic!("expected reasoning part");
    };
    assert_eq!(reasoning.text.as_deref(), Some("thinking"));
    assert!(matches!(
        reasoning.state.as_ref(),
        Some(ReasoningState {
            kind: ReasoningStateKind::AnthropicSignature { signature },
            ..
        }) if signature == "sig-1"
    ));
}

#[tokio::test]
async fn terminal_tool_call_replaces_the_streamed_preview() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(
            json!({"type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "lookup"}),
        )
        + &sse(json!({"type": "toolcall_delta", "contentIndex": 0, "delta": "{\"city\":"}))
        + &sse(json!({"type": "toolcall_delta", "contentIndex": 0, "delta": "\"Pa"}))
        + &sse(json!({
            "type": "toolcall_end", "contentIndex": 0,
            "toolCall": {"type": "toolCall", "id": "call_1", "name": "lookup", "arguments": {"city": "Paris"}}
        }))
        + &sse(
            json!({"type": "done", "reason": "toolUse", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
        );
    let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap();
    let response = match events.last().unwrap() {
        StreamEvent::Finished(response) => response,
        other => panic!("expected Finished, got {other:?}"),
    };
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
async fn tool_identity_change_is_rejected() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(
            json!({"type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "lookup"}),
        )
        + &sse(json!({
            "type": "toolcall_end", "contentIndex": 0,
            "toolCall": {"type": "toolCall", "id": "call_2", "name": "lookup", "arguments": {}}
        }))
        + &sse(
            json!({"type": "done", "reason": "toolUse", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
        );
    assert!(drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .is_err());
}

#[tokio::test]
async fn in_band_error_is_typed_and_never_echoes_provider_prose() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "partial"}))
        + &sse(json!({
            "type": "error", "reason": "error",
            "errorMessage": "provider-private request-private",
            "usage": {"input": 3, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 4}
        }));
    let error = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap_err();
    assert!(matches!(&error, AiError::Provider(provider)
        if provider.kind.as_deref() == Some("pi_messages_error")));
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(!rendered.contains("provider-private"));
        assert!(!rendered.contains("request-private"));
    }
}

#[tokio::test]
async fn aborted_error_event_is_a_canceled_failure() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({
            "type": "error", "reason": "aborted",
            "usage": {"input": 3, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 4}
        }));
    let error = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap_err();
    assert!(matches!(error, AiError::Canceled));
}

#[tokio::test]
async fn a_done_without_start_still_guards_a_started_stream() {
    let model = fixture_model();
    let wire = sse(json!({
        "type": "done", "reason": "stop",
        "usage": {"input": 1, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1}
    }));
    let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap();
    assert!(matches!(&events[0], StreamEvent::Started { .. }));
}

#[tokio::test]
async fn rewrite_and_malformed_events_are_bounded() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "x"}))
        + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "x"}))
        + &sse(json!({
            "type": "done", "reason": "stop",
            "rewrite": {"policyId": "gateway-policy", "policyVersion": 3, "changed": true,
                "tokenCountChange": -2, "messageCountChange": 1, "systemPromptChanged": false},
            "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}
        }));
    let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap();
    let response = match events.last().unwrap() {
        StreamEvent::Finished(response) => response,
        other => panic!("expected Finished, got {other:?}"),
    };
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "pi_messages_rewrite"
            && diagnostic.message.contains("gateway-policy v3")));

    for bad in [
        sse(json!({"type": "start"})) + &sse(json!({"type": "unknown_event"})),
        sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "x"})),
        sse(json!({"type": "start"}))
            + &sse(json!({"type": "done", "reason": "weird", "usage": {}})),
        sse(json!({"type": "start"}))
            + &sse(
                json!({"type": "done", "reason": "toolUse", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
            ),
        sse(json!({"type": "start"})) + &sse(json!({"type": "start"})),
    ] {
        assert!(
            drive(&model, decode_stream_event, bad.as_bytes(), 0)
                .await
                .is_err(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn signature_free_text_blocks_settle_without_reasoning_state() {
    let model = fixture_model();
    let wire = sse(json!({"type": "start"}))
        + &sse(json!({"type": "text_start", "contentIndex": 0}))
        + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "done"}))
        + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "done"}))
        + &sse(
            json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}),
        );
    let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
        .await
        .unwrap();
    assert!(matches!(events.last().unwrap(), StreamEvent::Finished(_)));
}
const FIXTURE_TEXT_TOOL: &str =
    include_str!("../../../tests/fixtures/pi_messages/text_tool_done.sse");
const FIXTURE_ERROR: &str = include_str!("../../../tests/fixtures/pi_messages/error_terminal.sse");
const FIXTURE_THINKING: &str =
    include_str!("../../../tests/fixtures/pi_messages/thinking_signature_done.sse");

#[tokio::test]
async fn fixture_text_and_tool_settle_once_and_are_chunking_invariant() {
    let model = fixture_model();
    let events = drive(&model, decode_stream_event, FIXTURE_TEXT_TOOL.as_bytes(), 0)
        .await
        .unwrap();
    let response = match events.last().unwrap() {
        StreamEvent::Finished(response) => response,
        other => panic!("expected Finished, got {other:?}"),
    };
    assert_eq!(response.response_id.as_deref(), Some("resp_fixture"));
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.usage.input_tokens, 12);
    assert_eq!(response.usage.cache_read_tokens, 3);
    assert_eq!(response.usage.total_tokens, 20);
    assert!(matches!(&response.message.content[..], [
        AssistantPart::Text(text), AssistantPart::ToolCall(call)
    ] if text == "Reading " && call.id.0 == "call_1"
        && call.arguments_value().unwrap() == json!({"city": "Paris"})));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
            .count(),
        1
    );
    // The trailing `[DONE]` frame is not a provider event and the missing
    // explicit text end is completed exactly once at the terminal.
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::TextEnd { .. }))
            .count(),
        1
    );
    let chunked = drive(&model, decode_stream_event, FIXTURE_TEXT_TOOL.as_bytes(), 1)
        .await
        .unwrap();
    assert_eq!(chunked.len(), events.len());
}

#[tokio::test]
async fn fixture_error_terminal_is_typed_and_prose_free() {
    let error = drive(
        &fixture_model(),
        decode_stream_event,
        FIXTURE_ERROR.as_bytes(),
        0,
    )
    .await
    .unwrap_err();
    assert!(matches!(&error, AiError::Provider(provider)
        if provider.kind.as_deref() == Some("pi_messages_error")));
    assert!(!format!("{error:?} {error}").contains("provider-private"));
}

#[tokio::test]
async fn fixture_thinking_signature_and_terminal_metadata_survive() {
    let events = drive(
        &fixture_model(),
        decode_stream_event,
        FIXTURE_THINKING.as_bytes(),
        0,
    )
    .await
    .unwrap();
    let response = match events.last().unwrap() {
        StreamEvent::Finished(response) => response,
        other => panic!("expected Finished, got {other:?}"),
    };
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(response.usage.cache_write_tokens, 1);
    assert_eq!(response.usage.total_tokens, 13);
    match &response.message.content[0] {
        AssistantPart::Reasoning(reasoning) => {
            assert_eq!(reasoning.text.as_deref(), Some("weighing options"));
            assert!(matches!(
                reasoning.state.as_ref(),
                Some(ReasoningState {
                    kind: ReasoningStateKind::AnthropicSignature { signature },
                    ..
                }) if signature == "opaque-sig"
            ));
        }
        other => panic!("expected reasoning, got {other:?}"),
    }
    assert!(matches!(&response.message.content[1], AssistantPart::Text(text) if text == "answer"));
    let codes: Vec<&str> = response
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert!(codes.contains(&"pi_messages_provider_thinking_level"));
    assert!(codes.contains(&"pi_messages_rewrite"));
}
