//! Unit tests for `crate::protocol::openai_chat`.
//!
//! Covers offline fixture-matrix replay of the stream decoder.
//!
//! Extracted from `openai_chat.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::openai_chat`, so it reaches the same private items the
//! inline block did. The seam split the implementation into four siblings, so
//! the frame decoder this suite replays is now named through the sibling that
//! owns it; the decode counter it asserts on stays in the parent because both
//! the response and the stream half increment it.

use super::{compat::consume_qwen_xml_content, decode_stream_event};
use crate::error::{AiError, DecodeError};
use crate::protocol::harness;
use crate::protocol::sse::SseEvent;
use crate::stream::{
    ResponseBuilder, StreamEvent, MAX_RESPONSE_CONTENT_BYTES, MAX_RESPONSE_EVENTS,
    MAX_TOOL_ARGUMENT_BYTES,
};
use crate::types::{
    AssistantPart, AudioPayload, Media, Protocol, StopReason, ToolCallArgumentError, ToolDef,
};

macro_rules! fx {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/openai_chat/",
            $name
        ))
    };
}

fn joined_text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

fn content_event(content: &str) -> SseEvent {
    SseEvent {
        event: None,
        data: serde_json::json!({
            "id": "compat-adversarial",
            "choices": [{"delta": {"content": content}}]
        })
        .to_string(),
    }
}

fn content_as_one_character_events(content: &str) -> Vec<u8> {
    let mut data = String::new();
    for character in content.chars() {
        data.push_str("data: ");
        data.push_str(&content_event(&character.to_string()).data);
        data.push_str("\n\n");
    }
    data.push_str(
        "data: {\"id\":\"compat-adversarial\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    data.into_bytes()
}

#[tokio::test]
async fn plain_streamed_text() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(&model, decode_stream_event, fx!("plain_text.sse"), 0)
        .await
        .unwrap();
    assert!(matches!(events.first(), Some(StreamEvent::Started { .. })));
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
    assert_eq!(joined_text(&events), "Hello, world");
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 9);
    assert_eq!(resp.usage.output_tokens, 3);
    assert_eq!(resp.usage.total_tokens, 12);
}

#[tokio::test]
async fn identical_across_every_byte_boundary() {
    // Re-feed the same fixture at every chunk size; the event sequence must
    // be byte-boundary independent (design §19 layer 1/2).
    let model = harness::model(Protocol::OpenAiChat, None);
    let data = fx!("plain_text.sse");
    let oneshot = harness::drive(&model, decode_stream_event, data, 0)
        .await
        .unwrap();
    let baseline = format!("{oneshot:?}");
    for chunk in 1..=data.len() {
        let got = harness::drive(&model, decode_stream_event, data, chunk)
            .await
            .unwrap();
        assert_eq!(
            format!("{got:?}"),
            baseline,
            "mismatch at chunk size {chunk}"
        );
    }
}

#[tokio::test]
async fn streamed_reasoning_has_no_state() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(&model, decode_stream_event, fx!("reasoning.sse"), 3)
        .await
        .unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::ReasoningStart { .. })));
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("reasoning part present");
    assert_eq!(reasoning.text.as_deref(), Some("Let me think about it."));
    assert!(reasoning.state.is_none(), "Chat reasoning carries no state");
    assert_eq!(resp.usage.reasoning_tokens, 5);
}

#[tokio::test]
async fn mistral_structured_content_preserves_reasoning_order() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(
        &model,
        decode_stream_event,
        fx!("mistral_structured_content.sse"),
        7,
    )
    .await
    .unwrap();
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::ReasoningStart { .. })));
    assert_eq!(joined_text(&events), "The answer is 42.");
    let response = harness::finished(&events);
    assert!(matches!(
        response.message.content.as_slice(),
        [AssistantPart::Reasoning(reasoning), AssistantPart::Text(text)]
            if reasoning.text.as_deref() == Some("Inspect first.")
                && text == "The answer is 42."
    ));
}

#[tokio::test]
async fn single_tool_call_args_reassemble() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(&model, decode_stream_event, fx!("one_tool_call.sse"), 4)
        .await
        .unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let tc = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call present");
    assert_eq!(tc.name, "grep");
    assert_eq!(tc.id.0, "call_a");
    assert_eq!(
        tc.arguments_value().unwrap(),
        serde_json::json!({"pattern": "foo"})
    );
}

#[tokio::test]
async fn schema_mismatch_is_marked_before_tool_call_end() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let tools = [ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "grep".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"pattern": {"type": "integer"}},
            "required": ["pattern"],
            "additionalProperties": false,
        }),
    }];
    let events = harness::drive_with_tools(
        &model,
        decode_stream_event,
        fx!("one_tool_call.sse"),
        4,
        &tools,
    )
    .await
    .unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallEnd {
                argument_error: Some(ToolCallArgumentError::SchemaMismatch),
                ..
            }
        )),
        "{events:#?}"
    );
    let call = harness::finished(&events)
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("schema-rejected call is retained");
    assert_eq!(call.id.0, "call_a");
    assert_eq!(call.arguments_json, r#"{"pattern":"foo"}"#);
    assert_eq!(
        call.argument_error,
        Some(ToolCallArgumentError::SchemaMismatch)
    );
}

#[tokio::test]
async fn parallel_tool_calls_interleaved() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(
        &model,
        decode_stream_event,
        fx!("parallel_tool_calls.sse"),
        0,
    )
    .await
    .unwrap();
    let resp = harness::finished(&events);
    let calls: Vec<_> = resp
        .message
        .content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::ToolCall(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "alpha");
    assert_eq!(calls[1].name, "beta");
    assert_eq!(
        calls[0].arguments_value().unwrap(),
        serde_json::json!({"a":1})
    );
    assert_eq!(
        calls[1].arguments_value().unwrap(),
        serde_json::json!({"b":2})
    );
}

#[tokio::test]
async fn malformed_tool_json_keeps_a_marked_call_envelope() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(
        &model,
        decode_stream_event,
        fx!("malformed_tool_json.sse"),
        0,
    )
    .await
    .unwrap();
    let response = harness::finished(&events);
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "malformed_tool_arguments"));
    let AssistantPart::ToolCall(call) = &response.message.content[0] else {
        panic!("expected the marked tool call");
    };
    assert_eq!(call.id.0, "call_bad");
    assert_eq!(call.name, "foo");
    assert_eq!(call.arguments_json, "{}");
    assert_eq!(call.argument_error, Some(ToolCallArgumentError::Malformed));
}

#[tokio::test]
async fn length_finish_maps_to_max_tokens() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(&model, decode_stream_event, fx!("length_stop.sse"), 0)
        .await
        .unwrap();
    assert_eq!(
        harness::finished(&events).stop_reason,
        StopReason::MaxTokens
    );
}

#[tokio::test]
async fn duplicate_finish_reason_closes_parts_once() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(
        &model,
        decode_stream_event,
        fx!("duplicate_finish_reason.sse"),
        0,
    )
    .await
    .unwrap();
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
    assert_eq!(
        joined_text(&events),
        "I'll help you fix the model picker filter. Let me first explore the repository."
    );
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.output_tokens, 20);
}

#[tokio::test]
async fn missing_finish_reason_closes_parts_at_done() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(&model, decode_stream_event, fx!("no_finish_reason.sse"), 0)
        .await
        .unwrap();
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
    assert_eq!(joined_text(&events), "Done without a finish chunk");
    let resp = harness::finished(&events);
    assert_eq!(resp.usage.output_tokens, 4);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.diagnostics.len(), 1);
    assert_eq!(resp.diagnostics[0].code, "chat_defaulted_stop_reason");
}

#[tokio::test]
async fn chat_completion_diagnostics_distinguish_missing_usage_from_reported_zero() {
    let model = harness::model(Protocol::OpenAiChat, None);
    for (report_stop, report_usage) in [(true, true), (false, true), (true, false), (false, false)]
    {
        let mut chunk = serde_json::json!({
            "id": "private-response-id",
            "choices": [{"delta": {"reasoning": "private reasoning"}}],
        });
        if report_stop {
            chunk["choices"][0]["finish_reason"] = "stop".into();
        }
        if report_usage {
            chunk["usage"] = serde_json::json!({"prompt_tokens": 0, "completion_tokens": 0});
        }
        let data = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        let events = harness::drive(&model, decode_stream_event, data.as_bytes(), 0)
            .await
            .unwrap();
        let response = harness::finished(&events);
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(response.usage, crate::Usage::default());
        let codes: Vec<_> = response
            .diagnostics
            .iter()
            .map(|diag| diag.code.as_str())
            .collect();
        assert_eq!(codes.contains(&"chat_defaulted_stop_reason"), !report_stop);
        assert_eq!(codes.contains(&"chat_usage_missing"), !report_usage);
        assert_eq!(
            codes.len(),
            usize::from(!report_stop) + usize::from(!report_usage)
        );
        for diagnostic in &response.diagnostics {
            assert!(!diagnostic.message.contains("private"));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::Usage(_)))
                .count(),
            usize::from(report_usage)
        );
    }
}

#[tokio::test]
async fn openrouter_reasoning_alias_decodes() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(&model, decode_stream_event, fx!("reasoning_alias.sse"), 0)
        .await
        .unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::ReasoningStart { .. })));
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("reasoning part present");
    assert_eq!(reasoning.text.as_deref(), Some("Let me think about it."));
    assert_eq!(joined_text(&events), "Answer: 42");
    assert!(resp.diagnostics.is_empty());
}

#[tokio::test]
async fn duplicate_finish_reason_closes_tool_calls_once() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(
        &model,
        decode_stream_event,
        fx!("duplicate_finish_tool_calls.sse"),
        0,
    )
    .await
    .unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(joined_text(&events), "Let me explore the repository.");
    let calls: Vec<_> = resp
        .message
        .content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::ToolCall(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "grep");
    assert_eq!(
        calls[0].arguments_value().unwrap(),
        serde_json::json!({"pattern": "foo"})
    );
    // Each part ended exactly once (§8).
    let ends = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                StreamEvent::TextEnd { .. } | StreamEvent::ToolCallEnd { .. }
            )
        })
        .count();
    assert_eq!(ends, 2);
}

#[tokio::test]
async fn deltas_after_finish_reopen_a_fresh_segment() {
    // A provider that keeps streaming after a finish chunk must not trip
    // the §8 guard; the late text becomes a fresh canonical segment.
    let model = harness::model(Protocol::OpenAiChat, None);
    let data: &[u8] = b"data: {\"id\":\"gen-x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"}}]}\n\ndata: {\"id\":\"gen-x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"gen-x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"second\"}}]}\n\ndata: {\"id\":\"gen-x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let events = harness::drive(&model, decode_stream_event, data, 0)
        .await
        .unwrap();
    assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
    let texts: Vec<&str> = harness::finished(&events)
        .message
        .content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["first", "second"]);
}

#[tokio::test]
async fn qwen_xml_tool_call_is_recovered_from_content() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let events = harness::drive(
        &model,
        decode_stream_event,
        fx!("qwen_xml_tool_call.sse"),
        1,
    )
    .await
    .unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert!(!joined_text(&events).contains("<tool_call>"));
    let call = resp
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("Qwen XML call present");
    assert_eq!(call.name, "read");
    assert_eq!(call.id.0, "qwen_xml_call_1");
    assert_eq!(
        call.arguments_value().unwrap(),
        serde_json::json!({"path": "src/main.rs"})
    );
}

#[test]
fn top_level_error_event_becomes_provider_error() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let data = b"data: {\"error\": {\"message\": \"Quota exceeded\", \"type\": \"rate_limit_error\", \"code\": \"429\"}}\n\n";
    let (_events, error) = harness::drive_raw(&model, decode_stream_event, data, 1);
    let error = error.expect("error payload should fail this stream");
    match error {
        AiError::Provider(provider) => {
            assert_eq!(provider.code.as_deref(), Some("429"));
            assert_eq!(provider.kind.as_deref(), Some("rate_limit_error"));
            assert_eq!(provider.message, "Quota exceeded");
        }
        other => panic!("expected provider error, got {other:?}"),
    }
}

#[test]
fn stream_error_fields_keep_permissive_fallbacks_and_precedence() {
    for (data, message, code, kind, request_id) in [
        (
            r#"{"error":{"message":"nested","code":429,"type":"inner","request_id":"nested-id"},"message":"outer","code":"outer","type":"outer","request_id":"outer-id"}"#,
            "nested",
            Some("429"),
            Some("inner"),
            Some("outer-id"),
        ),
        (
            r#"{"error":{"message":[],"code":{},"type":false,"request_id":"nested-id"},"message":"fallback","code":"outer","type":"outer","request_id":null}"#,
            "fallback",
            Some("outer"),
            Some("outer"),
            None,
        ),
        (
            r#"{"error":{"message":"","code":18446744073709551615,"request_id":"nested-id"}}"#,
            "",
            Some("18446744073709551615"),
            None,
            Some("nested-id"),
        ),
        (
            r#"{"error":{"message":"error","code":0},"request_id":42}"#,
            "error",
            Some("0"),
            None,
            None,
        ),
        (
            r#"{"error":{"message":"error","code":-1},"code":"fallback"}"#,
            "error",
            Some("fallback"),
            None,
            None,
        ),
        (
            r#"{"error":{"message":"error","code":1.0},"code":429}"#,
            "error",
            None,
            None,
            None,
        ),
        (
            r#"{"error":{"message":false},"message":"fallback"}"#,
            "fallback",
            None,
            None,
            None,
        ),
        (
            r#"{"error":{},"message":"old","message":"last","code":"old","code":"last","type":{},"type":"last","request_id":"old","request_id":"last"}"#,
            "last",
            Some("last"),
            Some("last"),
            Some("last"),
        ),
        (
            r#"{"error":null,"error":{"message":"old","message":"last","code":1,"code":2}}"#,
            "last",
            Some("2"),
            None,
            None,
        ),
        (
            r#"{"choices":[{"delta":{"content":"must not escape"}}],"error":{"message":"denied"}}"#,
            "denied",
            None,
            None,
            None,
        ),
    ] {
        super::CHAT_STREAM_JSON_DECODES.with(|count| count.set(0));
        let Some(AiError::Provider(error)) = super::stream::decode_chat_chunk(data).err() else {
            panic!("expected provider error for {data}");
        };
        assert_eq!(error.message, message, "{data}");
        assert_eq!(error.code.as_deref(), code, "{data}");
        assert_eq!(error.kind.as_deref(), kind, "{data}");
        assert_eq!(error.request_id.as_deref(), request_id, "{data}");
        assert_eq!(super::CHAT_STREAM_JSON_DECODES.with(|count| count.get()), 2);
    }
}

#[test]
fn malformed_chunk_fields_cannot_hide_a_provider_error() {
    for malformed in [
        r#""id":null"#,
        r#""id":"first","id":"second""#,
        r#""choices":null"#,
        r#""choices":[{}]"#,
        r#""usage":{"prompt_tokens":"bad"}"#,
    ] {
        for data in [
            format!(r#"{{{malformed},"error":{{"message":"denied"}}}}"#),
            format!(r#"{{"error":{{"message":"denied"}},{malformed}}}"#),
        ] {
            super::CHAT_STREAM_JSON_DECODES.with(|count| count.set(0));
            assert!(
                matches!(super::stream::decode_chat_chunk(&data), Err(AiError::Provider(error)) if error.message == "denied"),
                "{data}"
            );
            assert_eq!(super::CHAT_STREAM_JSON_DECODES.with(|count| count.get()), 2);
        }
        assert!(matches!(
            super::stream::decode_chat_chunk(&format!("{{{malformed}}}")),
            Err(AiError::Decode(DecodeError::Json(_)))
        ));
    }
    for data in [
        r#"{"error":{"message":"denied"},"choices":["#,
        "[DONE] ",
        "null",
    ] {
        assert!(matches!(
            super::stream::decode_chat_chunk(data),
            Err(AiError::Decode(DecodeError::Json(_)))
        ));
    }
}

#[test]
fn defaulted_chunks_and_malformed_error_metadata_stay_permissive() {
    for data in [
        "{}",
        "[]",
        r#"["id",[],null]"#,
        r#"{"error":null,"message":"not an error"}"#,
        r#"{"error":false,"message":"not an error"}"#,
        r#"{"error":[],"message":"not an error"}"#,
        r#"{"error":"not an object","message":"not an error"}"#,
        r#"{"error":{"message":{},"code":[]},"message":false}"#,
        r#"{"error":{"code":429},"message":[],"type":{},"request_id":[]}"#,
        r#"{"error":{"message":"overwritten"},"error":null}"#,
    ] {
        super::CHAT_STREAM_JSON_DECODES.with(|count| count.set(0));
        let chunk = super::stream::decode_chat_chunk(data).unwrap();
        let legacy: super::stream::ChatChunk = serde_json::from_str(data).unwrap();
        assert_eq!(chunk.id, legacy.id, "{data}");
        assert_eq!(chunk.choices.len(), legacy.choices.len(), "{data}");
        assert!(chunk.usage.is_none());
        let expected_decodes = if data.contains("\"error\"") { 2 } else { 1 };
        assert_eq!(
            super::CHAT_STREAM_JSON_DECODES.with(|count| count.get()),
            expected_decodes
        );
    }
}

#[test]
fn unrepresentable_error_metadata_keeps_the_legacy_chunk_fallback() {
    // Ignored metadata may be syntactically skippable even when Value
    // cannot represent it. This is not a reason to reject a valid chunk.
    for data in [
        r#"{"error":{"message":1e400}}"#,
        r#"{"message":1e400,"choices":[]}"#,
        r#"{"error":{"message":"ignored"},"unknown":1e400}"#,
    ] {
        let legacy: super::stream::ChatChunk = serde_json::from_str(data).unwrap();
        let chunk = super::stream::decode_chat_chunk(data).unwrap();
        assert_eq!(chunk.id, legacy.id);
        assert_eq!(chunk.choices.len(), legacy.choices.len());
    }
}

#[test]
fn chat_sse_decodes_once_per_json_frame_and_counts_done_before_finishing() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let mut builder = ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, None);
    super::CHAT_STREAM_JSON_DECODES.with(|count| count.set(0));
    let mut events = Vec::new();
    for data in [
        content_event("ordinary 🙂 text").data,
        "{}".to_owned(),
        r#"{"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}}"#.to_owned(),
    ] {
        events.extend(
            decode_stream_event(&model, &SseEvent { event: None, data }, &mut builder).unwrap(),
        );
    }
    assert_eq!(super::CHAT_STREAM_JSON_DECODES.with(|count| count.get()), 3);
    assert_eq!(builder.provider_event_count, 3);
    events.extend(
        decode_stream_event(
            &model,
            &SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
            &mut builder,
        )
        .unwrap(),
    );
    assert_eq!(super::CHAT_STREAM_JSON_DECODES.with(|count| count.get()), 3);
    // finish_mut replaces the consumed response builder with an empty one.
    assert_eq!(builder.provider_event_count, 0);
    assert_eq!(joined_text(&events), "ordinary 🙂 text");
    assert_eq!(harness::finished(&events).usage.total_tokens, 13);

    let mut builder = ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, None);
    let error = SseEvent {
        event: None,
        data: r#"{"error":{"message":"denied"}}"#.to_owned(),
    };
    assert!(matches!(
        decode_stream_event(&model, &error, &mut builder),
        Err(AiError::Provider(_))
    ));
    assert_eq!(builder.provider_event_count, 1);
    assert!(!builder.started);

    builder.provider_event_count = MAX_RESPONSE_EVENTS;
    super::CHAT_STREAM_JSON_DECODES.with(|count| count.set(0));
    let done = SseEvent {
        event: None,
        data: "[DONE]".to_owned(),
    };
    assert!(matches!(
        decode_stream_event(&model, &done, &mut builder),
        Err(AiError::Decode(DecodeError::TooManyStreamEvents))
    ));
    assert_eq!(super::CHAT_STREAM_JSON_DECODES.with(|count| count.get()), 0);
}

#[test]
fn ordinary_json_is_visible_before_stream_completion_by_default() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let event = content_event(r#"{"status":"working"}"#);
    let data = format!("data: {}\n\n", event.data);
    let (events, error) = harness::drive_raw(&model, decode_stream_event, data.as_bytes(), 1);
    assert!(error.is_none(), "{error:?}");
    assert_eq!(joined_text(&events), r#"{"status":"working"}"#);
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::TextDelta { .. })));
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::Finished(_))));
}

#[tokio::test]
async fn xml_compatibility_parser_recovers_one_character_events() {
    let model = harness::model(Protocol::OpenAiChat, None);
    for content in [
        r#"<tool_call><function name="read"><parameter name="path">README.md</parameter></function></tool_call>"#,
        r#"<function name="read"><parameter name="path">README.md</parameter></function></tool_call>"#,
    ] {
        let data = content_as_one_character_events(content);
        let events = harness::drive(&model, decode_stream_event, &data, 1)
            .await
            .unwrap();
        let call = harness::finished(&events)
            .message
            .content
            .iter()
            .find_map(|part| match part {
                AssistantPart::ToolCall(call) => Some(call),
                _ => None,
            })
            .expect("one-character XML stream should recover a tool call");
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments_value().unwrap()["path"], "README.md");
        assert!(!joined_text(&events).contains("</tool_call>"));
    }
}

#[tokio::test]
async fn explicitly_buffered_json_recovers_one_character_events() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let data =
        content_as_one_character_events(r#"{"name":"read","arguments":{"path":"README.md"}}"#);
    let events = harness::drive_with_compatibility_buffering(&model, decode_stream_event, &data, 1)
        .await
        .unwrap();
    let call = harness::finished(&events)
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("one-character JSON stream should recover a tool call");
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments_value().unwrap()["path"], "README.md");
}

#[test]
fn endless_ambiguous_candidate_hits_provider_event_cap_incrementally() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let mut builder = ResponseBuilder::new(
        model.spec.id.clone(),
        model.spec.protocol,
        model.spec.pricing.clone(),
    );
    builder.set_buffer_ambiguous_compatibility_content(true);

    decode_stream_event(&model, &content_event("{"), &mut builder).unwrap();
    let continuation = content_event(" ");
    for _ in 1..MAX_RESPONSE_EVENTS {
        decode_stream_event(&model, &continuation, &mut builder).unwrap();
    }
    let error = decode_stream_event(&model, &continuation, &mut builder).unwrap_err();
    assert!(matches!(
        error,
        AiError::Decode(DecodeError::TooManyStreamEvents)
    ));
    assert_eq!(builder.qwen_xml_pending.len(), MAX_RESPONSE_EVENTS);
}

#[test]
fn qwen_pending_reserves_aggregate_bytes_before_append() {
    let mut builder = ResponseBuilder::new(
        crate::types::ModelId("m".to_string()),
        Protocol::OpenAiChat,
        None,
    );
    builder
        .reserve_buffered_content(MAX_RESPONSE_CONTENT_BYTES)
        .unwrap();
    let mut events = Vec::new();
    let error = consume_qwen_xml_content(&mut events, &mut builder, "{").unwrap_err();
    assert!(matches!(
        error,
        AiError::Decode(DecodeError::ResponseTooLarge)
    ));
    assert!(builder.qwen_xml_pending.is_empty());
}

#[test]
fn arguments_before_tool_id_are_capped_before_append() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let mut builder = ResponseBuilder::new(
        model.spec.id.clone(),
        model.spec.protocol,
        model.spec.pricing.clone(),
    );
    let key = "tool_args_0".to_string();
    builder
        .reserve_buffered_content(MAX_TOOL_ARGUMENT_BYTES)
        .unwrap();
    builder
        .temp_buffers
        .insert(key.clone(), "x".repeat(MAX_TOOL_ARGUMENT_BYTES));
    let event = SseEvent {
        event: None,
        data: serde_json::json!({
            "id": "pre-id",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": "x"}
                    }]
                }
            }]
        })
        .to_string(),
    };
    let error = decode_stream_event(&model, &event, &mut builder).unwrap_err();
    assert!(matches!(
        error,
        AiError::Decode(DecodeError::ToolArgumentsTooLarge)
    ));
    assert_eq!(builder.temp_buffers[&key].len(), MAX_TOOL_ARGUMENT_BYTES);
}

#[tokio::test]
async fn explicitly_buffered_json_and_xml_variants_are_recovered_across_boundaries() {
    let model = harness::model(Protocol::OpenAiChat, None);
    for content in [
        r#"{"name":"read","arguments":{"path":"README.md",}}"#,
        r#"<tool_call><function name="read"><parameter name="path">README.md</parameter></function></tool_call>"#,
    ] {
        let escaped = serde_json::to_string(content).unwrap();
        let data = format!(
            "data: {{\"id\":\"compat\",\"choices\":[{{\"delta\":{{\"content\":{escaped}}}}}]}}\n\ndata: {{\"id\":\"compat\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        for chunk in [1, 2, 7, data.len()] {
            let events = harness::drive_with_compatibility_buffering(
                &model,
                decode_stream_event,
                data.as_bytes(),
                chunk,
            )
            .await
            .unwrap();
            let response = harness::finished(&events);
            assert_eq!(response.stop_reason, StopReason::ToolUse, "{content}");
            let call = response
                .message
                .content
                .iter()
                .find_map(|part| match part {
                    AssistantPart::ToolCall(call) => Some(call),
                    _ => None,
                })
                .expect("compatibility call");
            assert_eq!(call.name, "read");
            assert_eq!(call.arguments_value().unwrap()["path"], "README.md");
        }
    }
}

#[tokio::test]
async fn native_tool_call_supersedes_earlier_compatibility_content() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let xml = "<tool_call><function=read><parameter=path>duplicate.txt</parameter></function></tool_call>";
    let xml = serde_json::to_string(xml).unwrap();
    let data = format!(
        "data: {{\"id\":\"native-wins\",\"choices\":[{{\"delta\":{{\"content\":{xml}}}}}]}}\n\ndata: {{\"id\":\"native-wins\",\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"native_1\",\"type\":\"function\",\"function\":{{\"name\":\"read\",\"arguments\":\"{{\\\"path\\\":\\\"native.txt\\\"}}\"}}}}]}}}}]}}\n\ndata: {{\"id\":\"native-wins\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
    );
    let events = harness::drive(&model, decode_stream_event, data.as_bytes(), 1)
        .await
        .unwrap();
    let calls = harness::finished(&events)
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.0, "native_1");
    assert_eq!(calls[0].arguments_value().unwrap()["path"], "native.txt");
    assert!(!joined_text(&events).contains("duplicate.txt"));
}

#[tokio::test]
async fn streamed_tool_output_locked_marker_never_reaches_text() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let content = "about to inspect\n[tool_output_locked]";
    let escaped = serde_json::to_string(content).unwrap();
    let data = format!(
        "data: {{\"id\":\"locked\",\"choices\":[{{\"delta\":{{\"content\":{escaped}}}}}]}}\n\ndata: {{\"id\":\"locked\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
    );
    let events = harness::drive(&model, decode_stream_event, data.as_bytes(), 1)
        .await
        .unwrap();
    assert!(!joined_text(&events).contains("tool_output_locked"));
    assert_eq!(
        harness::finished(&events).stop_reason,
        StopReason::Other("tool_output_locked".to_string())
    );
}

#[test]
fn tools_disabled_dense_locked_markers_scan_once_and_compact_once() {
    let mut model = harness::model(Protocol::OpenAiChat, None);
    std::sync::Arc::make_mut(&mut model.spec).capabilities.tools = false;
    for count in [128, 4096] {
        for text in ["", "🙂é"] {
            let suffix = "[tool_out";
            let content = format!(
                "{}{suffix}",
                format!("{text}{}", super::compat::TOOL_OUTPUT_LOCKED).repeat(count)
            );
            let mut builder =
                ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, None);
            super::compat::PENDING_WORK
                .with(|work| work.set(super::compat::PendingWork::default()));
            let mut events =
                decode_stream_event(&model, &content_event(&content), &mut builder).unwrap();
            let work = super::compat::PENDING_WORK.with(|work| work.get());
            assert_eq!(work.scanned_bytes, content.len());
            assert_eq!(work.compactions, 1);
            assert_eq!(work.shifted_bytes, suffix.len());
            assert_eq!(builder.qwen_xml_pending, suffix);
            assert_eq!(builder.buffered_content_bytes, suffix.len());
            assert_eq!(builder.aggregate_content_bytes, text.len() * count);
            assert_eq!(joined_text(&events), text.repeat(count));
            events.extend(
                decode_stream_event(
                    &model,
                    &SseEvent {
                        event: None,
                        data: "[DONE]".to_owned(),
                    },
                    &mut builder,
                )
                .unwrap(),
            );
            assert_eq!(
                joined_text(&events),
                format!("{}{suffix}", text.repeat(count))
            );
            assert_eq!(builder.buffered_content_bytes, 0);
            // Finalization consumes these counters along with the content;
            // the pre-finish assertions above verify the retained budget.
            assert_eq!(builder.aggregate_content_bytes, 0);
            assert_eq!(
                harness::finished(&events).stop_reason,
                StopReason::Other("tool_output_locked".to_owned())
            );
        }
    }
}

#[test]
fn tools_disabled_locked_markers_preserve_unicode_splits_and_end_flush() {
    let mut model = harness::model(Protocol::OpenAiChat, None);
    std::sync::Arc::make_mut(&mut model.spec).capabilities.tools = false;
    for (content, expected, locked) in [
        (
            "α[tool_output_locked]β[tool_output_locked]🙂[tool_out",
            "αβ🙂[tool_out",
            true,
        ),
        ("é[tool_out", "é[tool_out", false),
    ] {
        for split in content
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(content.len()))
        {
            for finish_chunk in [false, true] {
                let mut builder =
                    ResponseBuilder::new(model.spec.id.clone(), model.spec.protocol, None);
                let mut events = Vec::new();
                for delta in [&content[..split], &content[split..]] {
                    events.extend(
                        decode_stream_event(&model, &content_event(delta), &mut builder).unwrap(),
                    );
                    assert_eq!(
                        builder.buffered_content_bytes,
                        builder.qwen_xml_pending.len()
                    );
                }
                if finish_chunk {
                    events.extend(
                        decode_stream_event(
                            &model,
                            &SseEvent {
                                event: None,
                                data: r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#
                                    .to_owned(),
                            },
                            &mut builder,
                        )
                        .unwrap(),
                    );
                }
                assert_eq!(
                    builder.aggregate_content_bytes + builder.buffered_content_bytes,
                    expected.len(),
                    "split={split}, finish={finish_chunk}"
                );
                events.extend(
                    decode_stream_event(
                        &model,
                        &SseEvent {
                            event: None,
                            data: "[DONE]".to_owned(),
                        },
                        &mut builder,
                    )
                    .unwrap(),
                );
                assert_eq!(
                    joined_text(&events),
                    expected,
                    "split={split}, finish={finish_chunk}"
                );
                assert_eq!(builder.buffered_content_bytes, 0);
                assert_eq!(builder.aggregate_content_bytes, 0);
                assert_eq!(
                    harness::finished(&events).stop_reason,
                    if locked {
                        StopReason::Other("tool_output_locked".to_owned())
                    } else {
                        StopReason::EndTurn
                    }
                );
            }
        }
    }
}

#[test]
fn tools_disabled_marker_filter_reserves_before_appending() {
    let mut builder = ResponseBuilder::new(
        crate::types::ModelId("m".to_owned()),
        Protocol::OpenAiChat,
        None,
    );
    builder
        .reserve_buffered_content(MAX_RESPONSE_CONTENT_BYTES)
        .unwrap();
    let error = super::compat::emit_text_without_locked_marker(
        &mut Vec::new(),
        &mut builder,
        super::compat::TOOL_OUTPUT_LOCKED,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        AiError::Decode(DecodeError::ResponseTooLarge)
    ));
    assert!(builder.qwen_xml_pending.is_empty());
    assert_eq!(builder.buffered_content_bytes, MAX_RESPONSE_CONTENT_BYTES);
    assert!(!builder.tool_output_locked_seen);
}

#[test]
fn tools_disabled_marker_filter_compacts_released_bytes_on_event_limit() {
    for (content, remaining) in [
        ("é[tool_output_locked]tail", "[tool_output_locked]tail"),
        ("[tool_output_locked]é[tool_out", "[tool_out"),
    ] {
        let mut builder = ResponseBuilder::new(
            crate::types::ModelId("m".to_owned()),
            Protocol::OpenAiChat,
            None,
        );
        builder.event_count = MAX_RESPONSE_EVENTS;
        let error =
            super::compat::emit_text_without_locked_marker(&mut Vec::new(), &mut builder, content)
                .unwrap_err();
        assert!(matches!(
            error,
            AiError::Decode(DecodeError::TooManyStreamEvents)
        ));
        assert_eq!(builder.qwen_xml_pending, remaining);
        assert_eq!(builder.buffered_content_bytes, remaining.len());
    }
}

#[tokio::test]
async fn incomplete_qwen_xml_tool_call_is_not_rendered_as_text() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let data: &[u8] = b"data: {\"id\":\"qwen-xml-2\",\"choices\":[{\"delta\":{\"content\":\"<tool_call><function=read>\"}}]}\n\ndata: [DONE]\n\n";
    let error = harness::drive(&model, decode_stream_event, data, 0)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::error::AiError::Decode(crate::error::DecodeError::InvalidProviderField(_))
    ));
}

#[tokio::test]
async fn premature_eof_before_done() {
    let model = harness::model(Protocol::OpenAiChat, None);
    let err = harness::drive(&model, decode_stream_event, fx!("premature_eof.sse"), 0)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::AiError::StreamProtocol(crate::error::StreamProtocolError::PrematureEof)
        ),
        "expected PrematureEof, got {err:?}"
    );
}

#[test]
fn completed_audio_output_decodes_to_single_media() {
    // Non-streaming `message.audio` path (design §12.1): one completed
    // AssistantPart::Media carrying data + provider ref + transcript.
    let model = harness::model(Protocol::OpenAiChat, None);
    let resp = super::decode_response(
        &model,
        fx!("audio_output.json"),
        Some(crate::types::AudioFormat::Wav),
    )
    .unwrap();
    let audio = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Media(Media::Audio(a)) => Some(a),
            _ => None,
        })
        .expect("audio media present");
    match &audio.payload {
        AudioPayload::InlineWithProviderRef { data, reference } => {
            assert!(data.starts_with(b"RIFF"), "decoded WAV bytes");
            assert_eq!(reference.id, "audio_abc123");
            assert_eq!(reference.protocol, Protocol::OpenAiChat);
            assert!(reference.expires_at.is_some());
        }
        other => panic!("expected InlineWithProviderRef, got {other:?}"),
    }
    assert_eq!(audio.transcript.as_deref(), Some("Hello from audio."));
}
