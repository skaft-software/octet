//! Unit tests for `crate::validate`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::validate`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::test_fixtures::{base_request, reasoning_capability};
use crate::types::{AssistantMessage, Message, ModalitySet, ToolCall, ToolCallId, UserMessage};

fn dummy_caps(
    image: bool,
    audio_in: bool,
    audio_out: bool,
    tools: bool,
    reasoning: bool,
    structured: bool,
) -> Capabilities {
    let mut input = ModalitySet::none();
    if image {
        input = input.with(crate::types::Modality::Image);
    }
    if audio_in {
        input = input.with(crate::types::Modality::Audio);
    }

    let mut output = ModalitySet::none();
    if audio_out {
        output = output.with(crate::types::Modality::Audio);
    }

    Capabilities {
        responses_features: Default::default(),
        input_modalities: input,
        output_modalities: output,
        tools,
        parallel_tool_calls: tools,
        reasoning: if reasoning {
            Some(reasoning_capability())
        } else {
            None
        },
        responses_lite: false,
        agent_delegation: None,
        structured_output: structured,
        deferred_tool_loading: false,
    }
}

fn dummy_limits() -> ModelLimits {
    ModelLimits {
        context_window: 10000,
        max_output_tokens: 1000,
    }
}

#[test]
fn test_orphan_tool_result() {
    let req = Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::ToolResult(crate::types::ToolResult {
                tool_call_id: ToolCallId("orphan".to_string()),
                content: vec![],
                is_error: false,
                added_tool_names: None,
            })],
        })],
        ..base_request()
    };
    let caps = dummy_caps(false, false, false, true, false, false);
    let limits = dummy_limits();

    let res = validate_request(
        &req,
        &caps,
        &limits,
        Protocol::OpenAiChat,
        &ModelId("model".to_string()),
        CompatibilityMode::Strict,
    );
    assert!(matches!(
        res,
        Err(AiError::Validation(ValidationError::OrphanToolResult(_)))
    ));
}

#[test]
fn test_missing_tool_result() {
    let req = Request {
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: ToolCallId("call_1".to_string()),
                    name: "tool".to_string(),
                    arguments_json: r#"{}"#.to_string(),
                    argument_error: None,
                })],
                model: ModelId("model".to_string()),
                protocol: Protocol::OpenAiChat,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("consecutive assistant".to_string())],
                model: ModelId("model".to_string()),
                protocol: Protocol::OpenAiChat,
            }),
        ],
        ..base_request()
    };
    let caps = dummy_caps(false, false, false, true, false, false);
    let limits = dummy_limits();

    // Anthropic requires tool pairing. In Strict mode, consecutive Assistant message before resolving call_1 is error.
    let res_strict = validate_request(
        &req,
        &caps,
        &limits,
        Protocol::AnthropicMessages,
        &ModelId("model".to_string()),
        CompatibilityMode::Strict,
    );
    assert!(matches!(
        res_strict,
        Err(AiError::Validation(ValidationError::MissingToolResult(_)))
    ));

    // In Lossy mode, it emits a Diagnostic
    let res_lossy = validate_request(
        &req,
        &caps,
        &limits,
        Protocol::AnthropicMessages,
        &ModelId("model".to_string()),
        CompatibilityMode::Lossy,
    )
    .unwrap();
    assert_eq!(res_lossy[0].code, "missing_tool_result");
}

#[test]
fn test_image_input_gate() {
    let req = Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Media(Media::image_url(
                url::Url::parse("https://example.com/a.jpg").unwrap(),
                None,
            ))],
        })],
        ..base_request()
    };
    let caps_no_image = dummy_caps(false, false, false, false, false, false);
    let limits = dummy_limits();

    let res_strict = validate_request(
        &req,
        &caps_no_image,
        &limits,
        Protocol::OpenAiChat,
        &ModelId("model".to_string()),
        CompatibilityMode::Strict,
    );
    assert!(matches!(
        res_strict,
        Err(AiError::Unsupported(UnsupportedError::Image))
    ));

    let res_lossy = validate_request(
        &req,
        &caps_no_image,
        &limits,
        Protocol::OpenAiChat,
        &ModelId("model".to_string()),
        CompatibilityMode::Lossy,
    )
    .unwrap();
    assert_eq!(res_lossy[0].code, "dropped_image");
}

#[test]
fn test_audio_input_gate() {
    let req = Request {
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Media(Media::audio_bytes(
                bytes::Bytes::from("wav"),
                AudioFormat::Flac,
            ))],
        })],
        ..base_request()
    };
    // Anthropic has no audio capability
    let caps_anthropic = dummy_caps(true, false, false, false, false, false);
    let limits = dummy_limits();

    let res_strict = validate_request(
        &req,
        &caps_anthropic,
        &limits,
        Protocol::AnthropicMessages,
        &ModelId("model".to_string()),
        CompatibilityMode::Strict,
    );
    assert!(matches!(
        res_strict,
        Err(AiError::Unsupported(UnsupportedError::Audio))
    ));

    let res_lossy = validate_request(
        &req,
        &caps_anthropic,
        &limits,
        Protocol::AnthropicMessages,
        &ModelId("model".to_string()),
        CompatibilityMode::Lossy,
    )
    .unwrap();
    assert_eq!(res_lossy[0].code, "dropped_audio");
}
