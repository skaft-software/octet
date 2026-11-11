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
use crate::types::{
    AssistantMessage, AudioFormat, Message, ModalitySet, ToolCall, ToolCallId, UserMessage,
};

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

fn fresh_stamps(count: usize) -> Vec<u64> {
    (0..count)
        .map(|_| crate::RequestHistory::fresh_stamp())
        .collect()
}

fn user_text(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserPart::Text(text.to_string())],
    })
}

fn assistant_text(text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::Text(text.to_string())],
        model: ModelId("model".to_string()),
        protocol: Protocol::OpenAiChat,
    })
}

fn assistant_call(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::ToolCall(ToolCall {
            async_execution: false,
            id: ToolCallId(id.to_string()),
            name: "tool".to_string(),
            arguments_json: "{}".to_string(),
            argument_error: None,
        })],
        model: ModelId("model".to_string()),
        protocol: Protocol::OpenAiChat,
    })
}

fn tool_result(id: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserPart::ToolResult(crate::types::ToolResult {
            tool_call_id: ToolCallId(id.to_string()),
            content: vec![],
            is_error: false,
            added_tool_names: None,
        })],
    })
}

/// Validates `messages` as the history lent through `history` under `stamps`.
/// Debug builds also compare the outcome with a full validation.
fn validate_lent(
    history: &crate::RequestHistory,
    messages: &[Message],
    stamps: &[u64],
    protocol: Protocol,
    mode: CompatibilityMode,
) -> Result<Vec<Diagnostic>, AiError> {
    let req = Request {
        messages: messages.to_vec(),
        ..base_request()
    };
    history.lend(stamps.to_vec());
    let mut state = history.lock();
    validate_lent_request(
        &req,
        &dummy_caps(false, false, false, true, false, false),
        &dummy_limits(),
        protocol,
        &ModelId("model".to_string()),
        mode,
        state.lent(&req.messages),
    )
}

#[test]
fn a_lent_history_is_validated_once_per_message() {
    let history = crate::RequestHistory::new();
    let strict = CompatibilityMode::Strict;
    let mut messages = vec![
        user_text("u1"),
        assistant_call("call_1"),
        tool_result("call_1"),
        assistant_text("a1"),
    ];
    let mut stamps = fresh_stamps(messages.len());

    take_validated_messages();
    validate_lent(&history, &messages, &stamps, Protocol::OpenAiChat, strict).unwrap();
    assert_eq!(take_validated_messages(), 4);

    // A new turn that reuses a completed call identity, as fallback
    // identities may: only the three new messages are walked.
    messages.extend([
        user_text("u2"),
        assistant_call("call_1"),
        tool_result("call_1"),
    ]);
    stamps.extend(fresh_stamps(3));
    validate_lent(&history, &messages, &stamps, Protocol::OpenAiChat, strict).unwrap();
    assert_eq!(take_validated_messages(), 3);

    validate_lent(&history, &messages, &stamps, Protocol::OpenAiChat, strict).unwrap();
    assert_eq!(take_validated_messages(), 0);

    // Pairing facts cannot be rewound, so a changed message starts over.
    stamps[3] = crate::RequestHistory::fresh_stamp();
    validate_lent(&history, &messages, &stamps, Protocol::OpenAiChat, strict).unwrap();
    assert_eq!(take_validated_messages(), 7);

    // Other rules share nothing with the kept facts.
    validate_lent(
        &history,
        &messages,
        &stamps,
        Protocol::OpenAiChat,
        CompatibilityMode::Lossy,
    )
    .unwrap();
    assert_eq!(take_validated_messages(), 7);
}

#[test]
fn a_rejected_history_keeps_nothing_new_and_an_unfinished_one_only_what_is_settled() {
    let history = crate::RequestHistory::new();
    let strict = CompatibilityMode::Strict;
    let protocol = Protocol::AnthropicMessages;
    let mut messages = vec![user_text("u1"), assistant_call("call_1")];
    let mut stamps = fresh_stamps(messages.len());

    // The call is unanswered, which this protocol rejects.
    assert!(matches!(
        validate_lent(&history, &messages, &stamps, protocol, strict),
        Err(AiError::Validation(ValidationError::MissingToolResult(_)))
    ));

    // A second result for the same call is rejected, and the rejected walk
    // leaves no fact behind: not the call, not its first result.
    messages.extend([tool_result("call_1"), tool_result("call_1")]);
    stamps.extend(fresh_stamps(2));
    take_validated_messages();
    assert!(validate_lent(&history, &messages, &stamps, protocol, strict).is_err());
    assert_eq!(take_validated_messages(), 4);

    messages.pop();
    stamps.pop();
    validate_lent(&history, &messages, &stamps, protocol, strict).unwrap();
    assert_eq!(take_validated_messages(), 3);

    // A protocol that accepts an unanswered call keeps the facts only up to
    // the message before it, and walks the call again with its result.
    let chat = Protocol::OpenAiChat;
    messages.extend([
        assistant_text("a1"),
        user_text("u2"),
        assistant_call("call_3"),
    ]);
    stamps.extend(fresh_stamps(3));
    validate_lent(&history, &messages, &stamps, chat, strict).unwrap();
    assert_eq!(take_validated_messages(), 6);
    messages.push(tool_result("call_3"));
    stamps.extend(fresh_stamps(1));
    validate_lent(&history, &messages, &stamps, chat, strict).unwrap();
    assert_eq!(take_validated_messages(), 2);

    // A result for a call that was never made is still an orphan.
    messages.push(tool_result("call_2"));
    stamps.extend(fresh_stamps(1));
    assert!(matches!(
        validate_lent(&history, &messages, &stamps, protocol, strict),
        Err(AiError::Validation(ValidationError::OrphanToolResult(_)))
    ));
}

#[test]
fn lossy_diagnostics_keep_their_order_when_a_history_grows() {
    let history = crate::RequestHistory::new();
    let lossy = CompatibilityMode::Lossy;
    let protocol = Protocol::AnthropicMessages;
    let image = || {
        Message::User(UserMessage {
            content: vec![UserPart::Media(Media::Image(crate::types::ImageMedia {
                source: ImageSource::Inline(bytes::Bytes::from_static(b"png")),
                media_type: None,
                detail: None,
            }))],
        })
    };
    let mut messages = vec![image(), assistant_call("call_1"), assistant_text("a1")];
    let mut stamps = fresh_stamps(messages.len());
    let codes = |diagnostics: Vec<Diagnostic>| -> Vec<(String, String)> {
        diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.message))
            .collect()
    };
    let full = |messages: &[Message]| {
        codes(
            validate_request(
                &Request {
                    messages: messages.to_vec(),
                    ..base_request()
                },
                &dummy_caps(false, false, false, true, false, false),
                &dummy_limits(),
                protocol,
                &ModelId("model".to_string()),
                lossy,
            )
            .unwrap(),
        )
    };

    let first = codes(validate_lent(&history, &messages, &stamps, protocol, lossy).unwrap());
    assert_eq!(first, full(&messages));
    assert_eq!(first[0].0, "missing_tool_result");
    assert!(
        first.len() > 1,
        "the fixture needs a modality diagnostic too"
    );

    messages.extend([image(), assistant_call("call_2"), assistant_text("a2")]);
    stamps.extend(fresh_stamps(3));
    take_validated_messages();
    let second = codes(validate_lent(&history, &messages, &stamps, protocol, lossy).unwrap());
    assert_eq!(take_validated_messages(), 3);
    assert_eq!(second, full(&messages));
    assert_eq!(second.len(), 2 * first.len());
}
