//! Unit tests for `crate::assistant_frame`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::assistant_frame`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::types::Usage;

fn model() -> ModelId {
    ModelId("frame-model".to_string())
}

fn stream() -> Vec<StreamEvent> {
    vec![
        StreamEvent::Started {
            response_id: Some("resp-1".to_string()),
        },
        StreamEvent::ReasoningStart { index: 0 },
        StreamEvent::ReasoningDelta {
            index: 0,
            delta: "think".to_string(),
        },
        StreamEvent::ReasoningEnd { index: 0 },
        StreamEvent::TextStart { index: 1 },
        StreamEvent::TextDelta {
            index: 1,
            delta: "Hel".to_string(),
        },
        StreamEvent::TextDelta {
            index: 1,
            delta: "lo".to_string(),
        },
        StreamEvent::TextEnd { index: 1 },
        StreamEvent::ToolCallStart {
            async_execution: false,
            index: 2,
            id: ToolCallId("call-1".to_string()),
            name: "lookup".to_string(),
        },
        StreamEvent::ToolCallArgsDelta {
            index: 2,
            delta: "{\"city\":".to_string(),
        },
        StreamEvent::ToolCallArgsDelta {
            index: 2,
            delta: "\"Paris\"}".to_string(),
        },
        StreamEvent::ToolCallEnd {
            index: 2,
            argument_error: None,
        },
        StreamEvent::Usage(Usage::default()),
    ]
}

#[test]
fn frames_round_trip_through_serde_and_reduce_to_the_partial_message() {
    let mut encoder = AssistantMessageFrameEncoder::new(model(), Protocol::OpenAiChat);
    let frames = encoder.encode_all(&stream()).unwrap();

    // Durability: the frames survive a JSON round trip.
    let json = serde_json::to_string(&frames).unwrap();
    let restored: Vec<AssistantMessageFrame> = serde_json::from_str(&json).unwrap();
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&frames).unwrap()
    );

    let message = reduce_assistant_message_frames(&restored).unwrap().unwrap();
    assert_eq!(message.model, model());
    assert_eq!(message.protocol, Protocol::OpenAiChat);
    assert_eq!(message.content.len(), 3);
    match &message.content[0] {
        AssistantPart::Reasoning(reasoning) => {
            assert_eq!(reasoning.text.as_deref(), Some("think"));
        }
        other => panic!("expected reasoning, got {other:?}"),
    }
    match &message.content[1] {
        AssistantPart::Text(text) => assert_eq!(text, "Hello"),
        other => panic!("expected text, got {other:?}"),
    }
    match &message.content[2] {
        AssistantPart::ToolCall(call) => {
            assert_eq!(call.id, ToolCallId("call-1".to_string()));
            assert_eq!(call.name, "lookup");
            assert_eq!(call.arguments_json, "{\"city\":\"Paris\"}");
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn truncated_prefix_reduces_to_partial_progress() {
    let mut encoder = AssistantMessageFrameEncoder::new(model(), Protocol::OpenAiChat);
    let frames = encoder.encode_all(&stream()).unwrap();
    // A crash after the first text delta: only the prefix is durable.
    let prefix = &frames[..6];
    let message = reduce_assistant_message_frames(prefix).unwrap().unwrap();
    assert_eq!(message.content.len(), 2);
    match &message.content[1] {
        AssistantPart::Text(text) => assert_eq!(text, "Hel"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn empty_before_start_is_not_a_message() {
    assert!(reduce_assistant_message_frames(&[]).unwrap().is_none());
}

#[test]
fn delta_before_block_start_is_rejected() {
    let mut encoder = AssistantMessageFrameEncoder::new(model(), Protocol::OpenAiChat);
    assert!(encoder
        .encode(&StreamEvent::Started { response_id: None })
        .unwrap()
        .is_some());
    let error = encoder
        .encode(&StreamEvent::TextDelta {
            index: 0,
            delta: "x".to_string(),
        })
        .unwrap_err();
    assert!(matches!(
        error,
        AiError::StreamProtocol(StreamProtocolError::UnexpectedEvent(_))
    ));
}

#[test]
fn delta_after_block_end_is_rejected_by_the_reducer() {
    let frames = vec![
        AssistantMessageFrame::Start {
            model: model(),
            protocol: Protocol::OpenAiChat,
        },
        AssistantMessageFrame::TextStart { index: 0 },
        AssistantMessageFrame::TextEnd { index: 0 },
        AssistantMessageFrame::TextDelta {
            index: 0,
            delta: "late".to_string(),
        },
    ];
    assert!(reduce_assistant_message_frames(&frames).is_err());
}
