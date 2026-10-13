//! Unit tests for `crate::transform`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::transform`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use std::sync::Arc;

use super::*;
use crate::types::{
    AudioFormat, AudioMedia, Capabilities, Endpoint, EndpointId, ImageMedia, ModalitySet, ModelId,
    ModelLimits, ModelSpec, Protocol, ProviderMediaRef, ReasoningPart, ReasoningState,
    ReasoningStateKind, ToolCall,
};

fn model(id: &str, protocol: Protocol, images: bool) -> Model {
    let input_modalities = if images {
        ModalitySet::none().with(Modality::Image)
    } else {
        ModalitySet::none()
    };
    Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId(id.to_string()),
            endpoint: EndpointId("test".to_string()),
            api_name: id.to_string(),
            display_name: None,
            protocol,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities,
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 16_384,
                max_output_tokens: 4096,
            },
            pricing: None,
            cache: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("test".to_string()),
            base_url: url::Url::parse("https://example.invalid/v1/").unwrap(),
            auth: crate::Auth::none(),
            default_headers: http::HeaderMap::new(),
            transport: crate::types::EndpointTransport::Http,
            runtime: crate::types::RequestRuntime::default(),
            timeout: std::time::Duration::from_secs(1),
        }),
    }
}

fn call(id: &str) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        async_execution: false,
        id: ToolCallId(id.to_string()),
        name: "read".to_string(),
        arguments_json: "{}".to_string(),
        argument_error: None,
    })
}

#[test]
fn cross_model_reasoning_becomes_text_but_empty_and_redacted_are_dropped() {
    let messages = vec![Message::Assistant(AssistantMessage {
        model: ModelId("source".to_string()),
        protocol: Protocol::AnthropicMessages,
        content: vec![
            AssistantPart::Reasoning(ReasoningPart {
                text: Some("use the cache".to_string()),
                state: None,
            }),
            AssistantPart::Reasoning(ReasoningPart {
                text: Some("  ".to_string()),
                state: None,
            }),
            AssistantPart::Reasoning(ReasoningPart {
                text: None,
                state: Some(ReasoningState {
                    protocol: Protocol::AnthropicMessages,
                    model: ModelId("source".to_string()),
                    kind: ReasoningStateKind::AnthropicRedacted {
                        data: "opaque".to_string(),
                    },
                }),
            }),
        ],
    })];

    let transformed = transform_messages(
        &messages,
        &model("target", Protocol::OpenAiResponses, false),
    );
    let Message::Assistant(assistant) = &transformed[0] else {
        panic!("expected assistant")
    };
    assert_eq!(assistant.content.len(), 1);
    assert!(matches!(
        &assistant.content[0],
        AssistantPart::Text(text) if text == "use the cache"
    ));
    // The canonical source is immutable.
    let Message::Assistant(source) = &messages[0] else {
        unreachable!()
    };
    assert_eq!(source.content.len(), 3);
}

#[test]
fn same_model_reasoning_state_is_preserved() {
    let state = ReasoningState {
        protocol: Protocol::AnthropicMessages,
        model: ModelId("same".to_string()),
        kind: ReasoningStateKind::AnthropicSignature {
            signature: "sig".to_string(),
        },
    };
    let messages = vec![Message::Assistant(AssistantMessage {
        model: ModelId("same".to_string()),
        protocol: Protocol::AnthropicMessages,
        content: vec![AssistantPart::Reasoning(ReasoningPart {
            text: Some("thought".to_string()),
            state: Some(state),
        })],
    })];
    let transformed = transform_messages(
        &messages,
        &model("same", Protocol::AnthropicMessages, false),
    );
    assert!(matches!(
        &transformed[0],
        Message::Assistant(AssistantMessage { content, .. })
            if matches!(&content[0], AssistantPart::Reasoning(_))
    ));
}

#[test]
fn unsupported_user_and_tool_images_become_visible_placeholders() {
    let image = Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from_static(b"image")),
        media_type: Some(mime::IMAGE_PNG),
        detail: None,
    });
    let messages = vec![Message::User(UserMessage {
        content: vec![
            UserPart::Media(image.clone()),
            UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId("call_1".to_string()),
                content: vec![ToolResultPart::Media(image)],
                is_error: false,
                added_tool_names: None,
            }),
        ],
    })];
    let transformed =
        transform_messages(&messages, &model("text-only", Protocol::OpenAiChat, false));
    let Message::User(user) = &transformed[0] else {
        panic!("expected user")
    };
    assert!(matches!(&user.content[0], UserPart::Text(text) if text == IMAGE_PLACEHOLDER));
    let UserPart::ToolResult(result) = &user.content[1] else {
        panic!("expected result")
    };
    assert!(
        matches!(&result.content[0], ToolResultPart::Text(text) if text == TOOL_IMAGE_PLACEHOLDER)
    );
}

#[test]
fn audio_transcripts_replace_unreplayable_user_tool_and_assistant_media() {
    let user_audio = Media::Audio(AudioMedia {
        payload: AudioPayload::Inline(bytes::Bytes::from_static(b"wav")),
        format: AudioFormat::Wav,
        transcript: Some("user said hello".into()),
    });
    let tool_audio = Media::Audio(AudioMedia {
        payload: AudioPayload::Inline(bytes::Bytes::from_static(b"wav")),
        format: AudioFormat::Wav,
        transcript: Some("tool heard a warning".into()),
    });
    let assistant_audio = Media::Audio(AudioMedia {
        payload: AudioPayload::ProviderRef(ProviderMediaRef {
            protocol: Protocol::OpenAiChat,
            id: "expired-audio".into(),
            expires_at: Some(std::time::SystemTime::UNIX_EPOCH),
        }),
        format: AudioFormat::Wav,
        transcript: Some("assistant spoke the answer".into()),
    });
    let messages = vec![
        Message::User(UserMessage {
            content: vec![
                UserPart::Media(user_audio),
                UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call_1".into()),
                    content: vec![ToolResultPart::Media(tool_audio)],
                    is_error: false,
                    added_tool_names: None,
                }),
            ],
        }),
        Message::Assistant(AssistantMessage {
            model: ModelId("source".into()),
            protocol: Protocol::OpenAiChat,
            content: vec![AssistantPart::Media(assistant_audio)],
        }),
    ];
    let target = model("target", Protocol::AnthropicMessages, false);

    let transformed = transform_messages(&messages, &target);
    let Message::User(user) = &transformed[0] else {
        panic!("expected user")
    };
    assert!(matches!(&user.content[0], UserPart::Text(text) if text == "user said hello"));
    let UserPart::ToolResult(result) = &user.content[1] else {
        panic!("expected tool result")
    };
    assert!(
        matches!(&result.content[0], ToolResultPart::Text(text) if text == "tool heard a warning")
    );
    assert!(matches!(
        &transformed[1],
        Message::Assistant(AssistantMessage { content, .. })
            if matches!(&content[0], AssistantPart::Text(text) if text == "assistant spoke the answer")
    ));

    let owned = transform_request_messages_owned(messages, &target);
    assert_eq!(
        serde_json::to_value(owned).unwrap(),
        serde_json::to_value(transformed).unwrap()
    );
}

#[test]
fn request_transform_preserves_pending_media_but_normalizes_history() {
    let image = Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from_static(b"image")),
        media_type: Some(mime::IMAGE_PNG),
        detail: None,
    });
    let messages = vec![
        Message::User(UserMessage {
            content: vec![UserPart::Media(image.clone())],
        }),
        Message::Assistant(AssistantMessage {
            model: ModelId("source".to_string()),
            protocol: Protocol::OpenAiResponses,
            content: vec![AssistantPart::Text("seen".to_string())],
        }),
        Message::User(UserMessage {
            content: vec![UserPart::Media(image)],
        }),
    ];

    let transformed =
        transform_request_messages(&messages, &model("text-only", Protocol::OpenAiChat, false));
    assert!(matches!(
        &transformed[0],
        Message::User(UserMessage { content })
            if matches!(&content[0], UserPart::Text(text) if text == IMAGE_PLACEHOLDER)
    ));
    assert!(matches!(
        &transformed[2],
        Message::User(UserMessage { content })
            if matches!(&content[0], UserPart::Media(_))
    ));
}

#[test]
fn owned_request_transform_matches_borrowed_and_moves_pending_text() {
    let invalid_id = "call_x|item_y/invalid";
    let pending_text = String::from("large pending prompt stays owned");
    let pending_allocation = pending_text.as_ptr();
    let messages = vec![
        Message::Assistant(AssistantMessage {
            model: ModelId("source".to_string()),
            protocol: Protocol::OpenAiResponses,
            content: vec![call(invalid_id)],
        }),
        Message::User(UserMessage {
            content: vec![
                UserPart::Text(pending_text),
                UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId(invalid_id.to_string()),
                    content: vec![ToolResultPart::Text("done".to_string())],
                    is_error: false,
                    added_tool_names: None,
                }),
            ],
        }),
    ];
    let target = model("target", Protocol::AnthropicMessages, false);
    let expected = transform_request_messages(&messages, &target);
    let actual = transform_request_messages_owned(messages, &target);
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let Message::User(user) = &actual[1] else {
        panic!("expected pending user message")
    };
    let UserPart::Text(text) = &user.content[0] else {
        panic!("expected pending text")
    };
    assert_eq!(text.as_ptr(), pending_allocation);
}

#[test]
fn tool_ids_are_normalized_and_missing_results_are_inserted_in_position() {
    let invalid_id = format!("call_{}|item_{}", "a".repeat(100), "b".repeat(100));
    let messages = vec![
        Message::Assistant(AssistantMessage {
            model: ModelId("source".to_string()),
            protocol: Protocol::OpenAiResponses,
            content: vec![call(&invalid_id)],
        }),
        Message::Assistant(AssistantMessage {
            model: ModelId("source".to_string()),
            protocol: Protocol::OpenAiResponses,
            content: vec![AssistantPart::Text("continued".to_string())],
        }),
    ];
    let transformed = transform_messages(
        &messages,
        &model("target", Protocol::AnthropicMessages, false),
    );
    assert_eq!(transformed.len(), 3);
    let Message::Assistant(first) = &transformed[0] else {
        panic!("expected assistant")
    };
    let AssistantPart::ToolCall(call) = &first.content[0] else {
        panic!("expected call")
    };
    assert!(call.id.0.len() <= 64);
    assert!(call
        .id
        .0
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'));

    let Message::User(result_message) = &transformed[1] else {
        panic!("expected synthetic result")
    };
    let UserPart::ToolResult(result) = &result_message.content[0] else {
        panic!("expected result")
    };
    assert_eq!(result.tool_call_id, call.id);
    assert!(result.is_error);
    assert!(
        matches!(&result.content[0], ToolResultPart::Text(text) if text == MISSING_TOOL_RESULT)
    );
}

#[test]
fn existing_tool_result_prevents_synthetic_result() {
    let messages = vec![
        Message::Assistant(AssistantMessage {
            model: ModelId("source".to_string()),
            protocol: Protocol::OpenAiResponses,
            content: vec![call("call_1")],
        }),
        Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId("call_1".to_string()),
                content: vec![ToolResultPart::Text("ok".to_string())],
                is_error: false,
                added_tool_names: None,
            })],
        }),
    ];
    let transformed = transform_messages(
        &messages,
        &model("target", Protocol::AnthropicMessages, false),
    );
    assert_eq!(transformed.len(), 2);
}
