//! Unit tests for `crate::protocol::anthropic`.
//!
//! Covers offline fixture-matrix replay of the stream decoder.
//!
//! Extracted from `anthropic.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::anthropic`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::MAX_ANTHROPIC_REFUSAL_EXPLANATION_BYTES;
use super::{bounded_refusal_explanation, decode_stream_event};
use crate::declarations::{AnthropicCompatPreset, AnthropicFallbackCost, AnthropicFallbackModel};
use crate::error::{AiError, StreamProtocolError};
use crate::pricing::{Pricing, TokenRate};
use crate::protocol::harness;
use crate::stream::StreamEvent;
use crate::types::{
    AssistantPart, Protocol, ReasoningStateKind, StopReason, ToolCallArgumentError, ToolDef,
};
use std::sync::Arc;

macro_rules! fx {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/anthropic/",
            $name
        ))
    };
}

async fn run(name: &'static [u8], chunk: usize) -> Result<Vec<StreamEvent>, AiError> {
    let model = harness::model(Protocol::AnthropicMessages, None);
    harness::drive(&model, decode_stream_event, name, chunk).await
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn text_with_ping_keepalive() {
    let events = run(fx!("text.sse"), 0).await.unwrap();
    assert_eq!(text_of(&events), "Hello there");
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
    assert_eq!(resp.usage.total_tokens, 15);
}

// pi anthropic-messages.ts: a `fallback` content block before any content is
// transparent; after content it is an unsupported mid-output model switch.
#[tokio::test]
async fn pre_content_fallback_is_transparent() {
    let events = run(fx!("fallback_pre_content.sse"), 0).await.unwrap();
    assert_eq!(text_of(&events), "Hi");
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
}

fn fallback_model() -> crate::Model {
    let mut model = harness::model(
        Protocol::AnthropicMessages,
        Some(Pricing {
            input: TokenRate(9_000_000),
            output: TokenRate(9_000_000),
            cache_read: TokenRate(9_000_000),
            cache_write_5m: TokenRate(9_000_000),
            cache_write_1h: None,
            reasoning: None,
            tiers: vec![],
        }),
    );
    Arc::make_mut(&mut model.spec).preset.anthropic_compat = Some(AnthropicCompatPreset {
        allowed_fallback_models: vec![AnthropicFallbackModel {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            cost: Some(AnthropicFallbackCost {
                input: 1.0,
                output: 5.0,
                cache_read: 0.1,
                cache_write: 1.25,
            }),
        }],
        ..Default::default()
    });
    model
}

#[tokio::test]
async fn declared_fallback_uses_terminal_model_and_exact_declared_price() {
    let model = fallback_model();
    for chunk in [0, 1] {
        let events = harness::drive(
            &model,
            decode_stream_event,
            fx!("fallback_priced.sse"),
            chunk,
        )
        .await
        .unwrap();
        let response = harness::finished(&events);
        assert_eq!(response.message.model.0, "claude-haiku-4-5");
        assert_eq!(response.usage.total_tokens, 2320);
        assert_eq!(response.cost.unwrap().total, 3035);
        assert_eq!(text_of(&events), "Done.");
        let state = response
            .message
            .content
            .iter()
            .find_map(|part| match part {
                AssistantPart::Reasoning(reasoning) => reasoning.state.as_ref(),
                _ => None,
            })
            .expect("signed fallback reasoning");
        assert_eq!(state.model.0, "claude-haiku-4-5");
    }
}

#[tokio::test]
async fn fallback_with_unmatched_or_incomplete_pricing_never_uses_requested_tariff() {
    let mut model = fallback_model();
    let fixture = std::str::from_utf8(fx!("fallback_priced.sse")).unwrap();
    for data in [
        fixture.replace("claude-haiku-4-5", "claude-haiku-unknown"),
        fixture.replace("\"model\":\"claude-haiku-4-5\",", ""),
        fixture.replace(
            "\"cache_creation_input_tokens\":20",
            "\"cache_creation_input_tokens\":20,\"cache_creation\":{\"ephemeral_1h_input_tokens\":20}",
        ),
    ] {
        let events = harness::drive(&model, decode_stream_event, data.as_bytes(), 0)
            .await
            .unwrap();
        assert!(harness::finished(&events).cost.is_none());
    }
    Arc::make_mut(&mut model.spec)
        .preset
        .anthropic_compat
        .as_mut()
        .unwrap()
        .allowed_fallback_models[0]
        .cost = None;
    let events = harness::drive(&model, decode_stream_event, fx!("fallback_priced.sse"), 0)
        .await
        .unwrap();
    assert_eq!(
        harness::finished(&events).message.model.0,
        "claude-haiku-4-5"
    );
    assert!(harness::finished(&events).cost.is_none());
}

#[tokio::test]
async fn mid_output_fallback_is_rejected() {
    let error = run(fx!("fallback_mid_output.sse"), 0).await.unwrap_err();
    assert!(matches!(
        error,
        AiError::Unsupported(crate::error::UnsupportedError::MidOutputModelFallback)
    ));
}

// f8: final usage merges the cumulative cache buckets and the documented
// thinking-token subset from `message_delta`, not just `output_tokens`.
#[tokio::test]
async fn final_usage_merges_cache_and_thinking_tokens() {
    let events = run(fx!("thinking_usage.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let u = &resp.usage;
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.cache_read_tokens, 40);
    assert_eq!(u.cache_write_tokens, 10);
    assert_eq!(u.output_tokens, 50);
    assert_eq!(
        u.reasoning_tokens, 30,
        "thinking_tokens must not be discarded"
    );
    assert_eq!(u.total_tokens, 100 + 40 + 10 + 50);
}

#[tokio::test]
async fn text_identical_across_byte_boundaries() {
    let data = fx!("text.sse");
    let base = format!("{:?}", run(data, 0).await.unwrap());
    for chunk in 1..=data.len() {
        assert_eq!(
            format!("{:?}", run(data, chunk).await.unwrap()),
            base,
            "chunk {chunk}"
        );
    }
}

#[tokio::test]
async fn thinking_preserves_signature_state() {
    let events = run(fx!("thinking.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(reasoning.text.as_deref(), Some("Consider the options."));
    let state = reasoning.state.as_ref().expect("signature state");
    assert_eq!(state.protocol, Protocol::AnthropicMessages);
    match &state.kind {
        ReasoningStateKind::AnthropicSignature { signature } => {
            assert_eq!(signature, "c2lnbmF0dXJl");
        }
        other => panic!("expected AnthropicSignature, got {other:?}"),
    }
    assert_eq!(text_of(&events), "Final answer.");
}

#[tokio::test]
async fn relay_model_mismatch_keeps_signed_thinking_bound_to_requested_model() {
    // A relay can report its upstream model in message_start. Its unsigned
    // label must not replace the requested model attached to opaque state.
    let data = br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_relay","model":"relay-upstream-model","usage":{"input_tokens":15,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Consider the options."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"c2lnbmF0dXJl"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Final answer."}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}

event: message_stop
data: {"type":"message_stop"}

"#;
    for chunk in [0, 1] {
        let events = run(data, chunk).await.unwrap();
        let response = harness::finished(&events);
        assert_eq!(response.message.model.0, "fixture-model");
        let reasoning = response
            .message
            .content
            .iter()
            .find_map(|part| match part {
                AssistantPart::Reasoning(reasoning) => Some(reasoning),
                _ => None,
            })
            .expect("signed thinking block");
        assert_eq!(reasoning.text.as_deref(), Some("Consider the options."));
        let state = reasoning.state.as_ref().expect("signature state");
        assert_eq!(state.model.0, "fixture-model");
        assert_eq!(state.protocol, Protocol::AnthropicMessages);
        assert!(matches!(
            &state.kind,
            ReasoningStateKind::AnthropicSignature { signature }
                if signature == "c2lnbmF0dXJl"
        ));
        assert_eq!(text_of(&events), "Final answer.");
    }
}

#[tokio::test]
async fn redacted_thinking_has_no_text() {
    let events = run(fx!("redacted_thinking.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    let reasoning = resp
        .message
        .content
        .iter()
        .find_map(|p| match p {
            AssistantPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(
        reasoning.text.is_none(),
        "redacted reasoning has no visible text"
    );
    match &reasoning.state.as_ref().unwrap().kind {
        ReasoningStateKind::AnthropicRedacted { data } => {
            assert_eq!(data, "RW5jcnlwdGVkQmxvYg==");
        }
        other => panic!("expected AnthropicRedacted, got {other:?}"),
    }
    assert_eq!(text_of(&events), "Done.");
}

#[tokio::test]
async fn single_tool_call() {
    let events = run(fx!("tool_call.sse"), 0).await.unwrap();
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
        .unwrap();
    assert_eq!(tc.name, "grep");
    assert_eq!(tc.id.0, "toolu_1");
    assert_eq!(
        tc.arguments_value().unwrap(),
        serde_json::json!({"pattern":"foo"})
    );
}

#[tokio::test]
async fn schema_mismatch_is_marked_before_tool_call_end() {
    let model = harness::model(Protocol::AnthropicMessages, None);
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
    let events =
        harness::drive_with_tools(&model, decode_stream_event, fx!("tool_call.sse"), 0, &tools)
            .await
            .unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallEnd {
            argument_error: Some(ToolCallArgumentError::SchemaMismatch),
            ..
        }
    )));
    let call = harness::finished(&events)
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("schema-rejected call is retained");
    assert_eq!(call.id.0, "toolu_1");
    assert_eq!(call.arguments_json, r#"{"pattern":"foo"}"#);
    assert_eq!(
        call.argument_error,
        Some(ToolCallArgumentError::SchemaMismatch)
    );
}

#[tokio::test]
async fn parallel_tool_calls() {
    let events = run(fx!("parallel_tool_calls.sse"), 0).await.unwrap();
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
}

#[tokio::test]
async fn malformed_tool_json_keeps_a_marked_call_envelope() {
    let events = run(fx!("malformed_tool_json.sse"), 0).await.unwrap();
    let response = harness::finished(&events);
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(response
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "malformed_tool_arguments"));
    let call = response
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("the malformed call keeps its envelope");
    assert_eq!(call.id.0, "toolu_bad");
    assert_eq!(call.name, "foo");
    assert_eq!(call.arguments_json, "{}");
    assert_eq!(call.argument_error, Some(ToolCallArgumentError::Malformed));
}

#[tokio::test]
async fn stop_reason_variants() {
    assert_eq!(
        harness::finished(&run(fx!("max_tokens.sse"), 0).await.unwrap()).stop_reason,
        StopReason::MaxTokens
    );
    assert_eq!(
        harness::finished(&run(fx!("stop_sequence.sse"), 0).await.unwrap()).stop_reason,
        StopReason::StopSequence
    );
    assert_eq!(
        harness::finished(&run(fx!("pause_turn.sse"), 0).await.unwrap()).stop_reason,
        StopReason::PauseTurn
    );
}

#[tokio::test]
async fn refusal_stop_details_explanation_is_surfaced_and_bounded() {
    let events = run(fx!("refusal.sse"), 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::Refusal);
    let diagnostic = resp
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "anthropic_refusal")
        .expect("a refusal must carry its stop_details explanation");
    assert!(
        diagnostic.message.contains("usage policy prohibits"),
        "{diagnostic:?}"
    );

    // A refusal without stop_details still carries the canonical reason and
    // fabricates no explanation.
    let data = br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_r2","usage":{"input_tokens":1,"output_tokens":0}}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"refusal"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}

"#;
    let events = run(data, 0).await.unwrap();
    let resp = harness::finished(&events);
    assert_eq!(resp.stop_reason, StopReason::Refusal);
    assert!(resp.diagnostics.is_empty(), "{:?}", resp.diagnostics);

    // Provider prose is untrusted: the diagnostic is truncated on a
    // character boundary, never copied whole.
    let long = "é".repeat(MAX_ANTHROPIC_REFUSAL_EXPLANATION_BYTES);
    let bounded = bounded_refusal_explanation(&long);
    assert!(bounded.len() <= MAX_ANTHROPIC_REFUSAL_EXPLANATION_BYTES + 3);
    assert!(bounded.ends_with('…'));
    assert_eq!(
        bounded_refusal_explanation("short").as_str(),
        "short",
        "a short explanation is copied unchanged"
    );
}

#[tokio::test]
async fn error_event_becomes_provider_error() {
    let err = run(fx!("error_event.sse"), 0).await.unwrap_err();
    match err {
        AiError::Provider(p) => {
            assert_eq!(p.kind.as_deref(), Some("overloaded_error"));
            assert_eq!(p.message, "Overloaded");
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

#[tokio::test]
async fn premature_eof() {
    let err = run(fx!("premature_eof.sse"), 0).await.unwrap_err();
    assert!(
        matches!(
            err,
            AiError::StreamProtocol(StreamProtocolError::PrematureEof)
        ),
        "got {err:?}"
    );
}
