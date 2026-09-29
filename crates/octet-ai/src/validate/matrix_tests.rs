//! Unit tests for `crate::validate`.
//!
//! Covers the capability and limit validation matrix.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::validate`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::{normalize_request_reasoning, validate_request};
use crate::error::{AiError, UnsupportedError, ValidationError};
use crate::test_fixtures::base_request;
use crate::types::{
    AssistantMessage, AssistantPart, AudioFormat, AudioOutputOptions, AudioVoice, Capabilities,
    ImageDetail, ImageMedia, ImageSource, JsonSchemaFormat, Media, Message, Modality, ModalitySet,
    ModelId, ModelLimits, OutputFormat, OutputModalities, Protocol, ProviderMediaRef,
    ReasoningConfig, ReasoningEffort, ReasoningPart, ReasoningState, ReasoningStateKind, Request,
    ToolCall, ToolCallId, ToolChoice, ToolDef, ToolResult, ToolResultPart, UserMessage, UserPart,
};
use crate::CompatibilityMode::{Lossy, Strict};

fn caps(
    image: bool,
    audio_in: bool,
    audio_out: bool,
    tools: bool,
    reasoning: bool,
    structured: bool,
) -> Capabilities {
    let mut input = ModalitySet::none();
    if image {
        input = input.with(Modality::Image);
    }
    if audio_in {
        input = input.with(Modality::Audio);
    }
    let mut output = ModalitySet::none();
    if audio_out {
        output = output.with(Modality::Audio);
    }
    Capabilities {
        responses_features: Default::default(),
        input_modalities: input,
        output_modalities: output,
        tools,
        parallel_tool_calls: tools,
        reasoning: if reasoning {
            Some(crate::types::ReasoningCapability {
                options: None,
                control: crate::types::ReasoningControl::Effort,
                exposes_text: true,
                preserves_state: true,
                effort_budgets: None,
                openai_chat_mode: crate::types::OpenAiChatReasoningMode::Standard,
                min_effort: crate::types::ReasoningEffort::Minimal,
                max_effort: crate::types::ReasoningEffort::High,
            })
        } else {
            None
        },
        responses_lite: false,
        agent_delegation: None,
        structured_output: structured,
        deferred_tool_loading: false,
    }
}

fn limits() -> ModelLimits {
    ModelLimits {
        context_window: 100_000,
        max_output_tokens: 1000,
    }
}

fn user(parts: Vec<UserPart>) -> Message {
    Message::User(UserMessage { content: parts })
}

#[test]
fn responses_options_fail_closed_on_other_protocols() {
    let mut req = base_request();
    req.responses = Some(crate::responses::ResponsesOptions::full_replay(
        crate::responses::ResponsesInput::default(),
    ));
    for protocol in [Protocol::OpenAiChat, Protocol::AnthropicMessages] {
        assert!(matches!(
            run(
                &req,
                &caps(false, false, false, true, false, false),
                protocol
            ),
            Err(AiError::Unsupported(UnsupportedError::ResponsesOptions))
        ));
    }
}

fn run(req: &Request, c: &Capabilities, p: Protocol) -> Result<Vec<crate::Diagnostic>, AiError> {
    validate_request(
        req,
        c,
        &limits(),
        p,
        &ModelId("target".into()),
        req.compatibility,
    )
}

fn has_code(diags: &[crate::Diagnostic], code: &str) -> bool {
    diags.iter().any(|d| d.code == code)
}

// --- image input gate ---
#[test]
fn image_without_capability() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::Media(Media::Image(ImageMedia {
        source: ImageSource::Inline(bytes::Bytes::from_static(b"x")),
        media_type: Some("image/png".parse().unwrap()),
        detail: Some(ImageDetail::Auto),
    }))])];
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Unsupported(UnsupportedError::Image))
    ));
    req.compatibility = Lossy;
    let diags = run(
        &req,
        &caps(false, false, false, false, false, false),
        Protocol::OpenAiChat,
    )
    .unwrap();
    assert!(has_code(&diags, "dropped_image"));
}

// --- Anthropic documents URL image sources ---
#[test]
fn image_url_on_anthropic_is_supported() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::Media(Media::image_url(
        url::Url::parse("https://example.test/a.png").unwrap(),
        None,
    ))])];
    let c = caps(true, false, false, false, false, false);
    assert!(run(&req, &c, Protocol::AnthropicMessages)
        .unwrap()
        .is_empty());
    req.compatibility = Lossy;
    assert!(run(&req, &c, Protocol::AnthropicMessages)
        .unwrap()
        .is_empty());
}

// --- audio input gate (always fails on Responses & Anthropic) ---
#[test]
fn audio_input_rejected_on_responses_and_anthropic() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::Media(Media::audio_bytes(
        bytes::Bytes::from_static(b"RIFF"),
        AudioFormat::Wav,
    ))])];
    for p in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        assert!(
            matches!(
                run(&req, &caps(true, false, false, false, false, false), p),
                Err(AiError::Unsupported(UnsupportedError::Audio))
            ),
            "strict audio reject on {p:?}"
        );
    }
    req.compatibility = Lossy;
    let diags = run(
        &req,
        &caps(true, false, false, false, false, false),
        Protocol::AnthropicMessages,
    )
    .unwrap();
    assert!(has_code(&diags, "dropped_audio"));
}

// --- Chat audio non-inline format gate ---
#[test]
fn chat_audio_non_wav_mp3_rejected() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::Media(Media::audio_bytes(
        bytes::Bytes::from_static(b"OggS"),
        AudioFormat::Opus,
    ))])];
    let c = caps(false, true, false, false, false, false);
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Unsupported(UnsupportedError::Audio))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(&req, &c, Protocol::OpenAiChat).unwrap(),
        "dropped_audio_format"
    ));
}

// --- audio output on non-audio models / non-Chat protocols ---
#[test]
fn audio_output_requires_chat_and_capability() {
    let mut req = base_request();
    req.output_modalities = OutputModalities::TextAndAudio(AudioOutputOptions {
        format: AudioFormat::Wav,
        voice: AudioVoice::Named("alloy".into()),
    });
    // Responses cannot do audio output even with the cap bit.
    assert!(matches!(
        run(
            &req,
            &caps(false, false, true, false, false, false),
            Protocol::OpenAiResponses
        ),
        Err(AiError::Unsupported(UnsupportedError::AudioOutput))
    ));
    // Chat model lacking the audio-out cap.
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Unsupported(UnsupportedError::AudioOutput))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::AnthropicMessages
        )
        .unwrap(),
        "downgraded_audio_output"
    ));
}

#[test]
fn audio_output_rejects_empty_voices() {
    for voice in [
        AudioVoice::Named("   ".into()),
        AudioVoice::ProviderRef(String::new()),
    ] {
        let mut req = base_request();
        req.output_modalities = OutputModalities::TextAndAudio(AudioOutputOptions {
            format: AudioFormat::Wav,
            voice,
        });
        let error = run(
            &req,
            &caps(false, false, true, false, false, false),
            Protocol::OpenAiChat,
        )
        .unwrap_err();
        let AiError::Validation(validation) = &error else {
            panic!("expected validation error, got {error}");
        };
        assert!(matches!(validation, ValidationError::InvalidAudioVoice));
        assert_eq!(
            validation.to_string(),
            "Audio output voice must not be empty"
        );
    }
}

// --- deferred tool loading is not implemented: reject, never hide schemas ---
#[test]
fn deferred_tool_loading_is_rejected_instead_of_hiding_schemas() {
    let req = base_request();
    let mut c = caps(false, false, false, true, false, false);
    c.deferred_tool_loading = true;
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Config(crate::error::ConfigError::InvalidModel(_)))
    ));
}

// --- tools without capability ---
#[test]
fn tools_without_capability() {
    let mut req = base_request();
    req.tools = vec![ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "grep".into(),
        description: "search".into(),
        parameters: serde_json::json!({"type":"object"}),
    }];
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Unsupported(UnsupportedError::Tools))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        )
        .unwrap(),
        "dropped_tools"
    ));
}

// --- tool_choice without capability ---
#[test]
fn tool_choice_without_capability() {
    let mut req = base_request();
    req.tool_choice = ToolChoice::Required;
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Unsupported(UnsupportedError::ToolChoice))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        )
        .unwrap(),
        "dropped_tool_choice"
    ));
}

#[test]
fn explicit_reasoning_choices_fail_instead_of_clamping() {
    let mut req = base_request();
    let mut capabilities = caps(false, false, false, false, true, false);
    let cap = capabilities.reasoning.as_mut().unwrap();
    cap.min_effort = ReasoningEffort::Low;
    cap.options = Some(crate::types::ReasoningOptions {
        values: vec!["low".into(), "high".into()],
        default: Some("high".into()),
    });
    for choice in [
        ReasoningConfig::Off,
        ReasoningConfig::Effort(ReasoningEffort::Medium),
        ReasoningConfig::Effort(ReasoningEffort::Max),
    ] {
        req.reasoning = choice.clone();
        assert_eq!(
            normalize_request_reasoning(&req, &capabilities).reasoning,
            choice
        );
        assert!(run(&req, &capabilities, Protocol::OpenAiChat).is_err());
    }
    req.reasoning = ReasoningConfig::Effort(ReasoningEffort::High);
    assert!(run(&req, &capabilities, Protocol::OpenAiChat).is_ok());
}

#[test]
fn reasoning_budget_must_fit_the_effective_request_output_limit() {
    let mut req = base_request();
    req.max_output_tokens = Some(2_000);
    req.reasoning = ReasoningConfig::Budget(3_000);
    let mut capabilities = caps(false, false, false, false, true, false);
    capabilities.reasoning.as_mut().unwrap().control = crate::types::ReasoningControl::TokenBudget;
    let model_limits = ModelLimits {
        context_window: 100_000,
        max_output_tokens: 8_000,
    };

    assert!(matches!(
        validate_request(
            &req,
            &capabilities,
            &model_limits,
            Protocol::AnthropicMessages,
            &ModelId("target".into()),
            Strict,
        ),
        Err(AiError::Validation(
            ValidationError::ReasoningBudgetOutOfRange
        ))
    ));
}

// --- reasoning without capability ---
#[test]
fn reasoning_without_capability() {
    let mut req = base_request();
    req.reasoning = ReasoningConfig::Effort(ReasoningEffort::High);
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Unsupported(UnsupportedError::Reasoning))
    ));
    req.compatibility = Lossy;
    assert!(run(
        &req,
        &caps(false, false, false, false, false, false),
        Protocol::OpenAiChat
    )
    .is_err());
}

// --- reasoning-state protocol/model mismatch ---
#[test]
fn reasoning_state_mismatch() {
    let mut req = base_request();
    req.messages = vec![Message::Assistant(AssistantMessage {
        content: vec![AssistantPart::Reasoning(ReasoningPart {
            text: Some("prior".into()),
            state: Some(ReasoningState {
                protocol: Protocol::AnthropicMessages,
                model: ModelId("other-model".into()),
                kind: ReasoningStateKind::AnthropicSignature {
                    signature: "s".into(),
                },
            }),
        })],
        model: ModelId("other-model".into()),
        protocol: Protocol::AnthropicMessages,
    })];
    let c = caps(false, false, false, false, true, false);
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiResponses),
        Err(AiError::Unsupported(
            UnsupportedError::ReasoningStateMismatch { .. }
        ))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(&req, &c, Protocol::OpenAiResponses).unwrap(),
        "dropped_reasoning_state"
    ));
}

// --- structured output without capability ---
#[test]
fn structured_output_without_capability() {
    let mut req = base_request();
    req.output_format = OutputFormat::JsonObject;
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Unsupported(UnsupportedError::StructuredOutput))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(
            &req,
            &caps(false, false, false, false, false, false),
            Protocol::OpenAiChat
        )
        .unwrap(),
        "downgraded_output_format"
    ));
}

// --- JsonObject on Anthropic (unsupported) ---
#[test]
fn json_object_unsupported_on_anthropic() {
    let mut req = base_request();
    req.output_format = OutputFormat::JsonObject;
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, true),
            Protocol::AnthropicMessages
        ),
        Err(AiError::Unsupported(UnsupportedError::StructuredOutput))
    ));
}

#[test]
fn structured_output_is_unsupported_on_bedrock_even_if_misadvertised() {
    let mut req = base_request();
    req.output_format = OutputFormat::JsonObject;
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, true),
            Protocol::BedrockConverse
        ),
        Err(AiError::Unsupported(UnsupportedError::StructuredOutput))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(
            &req,
            &caps(false, false, false, false, false, true),
            Protocol::BedrockConverse
        )
        .unwrap(),
        "downgraded_output_format"
    ));

    req.compatibility = Strict;
    req.output_format = OutputFormat::JsonSchema(JsonSchemaFormat {
        name: "output".into(),
        description: None,
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, true),
            Protocol::BedrockConverse
        ),
        Err(AiError::Unsupported(UnsupportedError::StructuredOutput))
    ));
}

// --- invalid JSON schema name / non-object schema ---
#[test]
fn invalid_schema_name_and_shape() {
    let mut req = base_request();
    req.output_format = OutputFormat::JsonSchema(JsonSchemaFormat {
        name: "bad name!".into(),
        description: None,
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, true),
            Protocol::OpenAiChat
        ),
        Err(AiError::Validation(
            ValidationError::InvalidOutputFormatName(_)
        ))
    ));

    req.output_format = OutputFormat::JsonSchema(JsonSchemaFormat {
        name: "ok".into(),
        description: None,
        schema: serde_json::json!([1, 2, 3]),
        strict: true,
    });
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, false, false, true),
            Protocol::OpenAiChat
        ),
        Err(AiError::Validation(ValidationError::InvalidOutputSchema(_)))
    ));
}

// --- max_output_tokens bounds ---
#[test]
fn max_output_tokens_bounds() {
    let c = caps(false, false, false, false, false, false);
    let mut req = base_request();
    req.max_output_tokens = Some(0);
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Validation(
            ValidationError::InvalidMaxOutputTokens { .. }
        ))
    ));
    req.max_output_tokens = Some(5000); // over model max 1000
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Validation(
            ValidationError::InvalidMaxOutputTokens { .. }
        ))
    ));
}

// --- temperature bounds ---
#[test]
fn temperature_bounds() {
    let c = caps(false, false, false, false, false, false);
    let mut req = base_request();
    req.temperature = Some(f32::NAN);
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Validation(ValidationError::InvalidTemperature))
    ));
    req.temperature = Some(2.5);
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Validation(ValidationError::InvalidTemperature))
    ));
}

// --- tool-result media on Chat (text-only tool results) ---
#[test]
fn tool_result_media_rejected() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::ToolResult(ToolResult {
        tool_call_id: ToolCallId("call_1".into()),
        content: vec![ToolResultPart::Media(Media::image_url(
            url::Url::parse("https://example.test/a.png").unwrap(),
            None,
        ))],
        is_error: false,
        added_tool_names: None,
    })])];
    // Provide a preceding assistant tool call so pairing passes and we reach the media check.
    req.messages.insert(
        0,
        Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId("call_1".into()),
                name: "t".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            })],
            model: ModelId("target".into()),
            protocol: Protocol::OpenAiChat,
        }),
    );
    let c = caps(true, false, false, true, false, false);
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Unsupported(UnsupportedError::ToolResultMedia))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(&req, &c, Protocol::OpenAiChat).unwrap(),
        "dropped_tool_result_media"
    ));
}

// --- provider media ref: wrong protocol + expired ---
#[test]
fn provider_media_ref_mismatch_and_expiry() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::Media(Media::Image(ImageMedia {
        source: ImageSource::ProviderRef(ProviderMediaRef {
            protocol: Protocol::OpenAiResponses,
            id: "img_1".into(),
            expires_at: None,
        }),
        media_type: None,
        detail: None,
    }))])];
    let c = caps(true, false, false, false, false, false);
    // Wrong protocol (ref is Responses, target Chat).
    assert!(matches!(
        run(&req, &c, Protocol::OpenAiChat),
        Err(AiError::Unsupported(UnsupportedError::ProviderMediaRef))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(&req, &c, Protocol::OpenAiChat).unwrap(),
        "dropped_mismatched_media_ref"
    ));

    // Expired ref on the matching protocol.
    let mut req2 = base_request();
    req2.compatibility = Lossy;
    req2.messages = vec![user(vec![UserPart::Media(Media::Image(ImageMedia {
        source: ImageSource::ProviderRef(ProviderMediaRef {
            protocol: Protocol::OpenAiChat,
            id: "img_2".into(),
            expires_at: Some(std::time::UNIX_EPOCH),
        }),
        media_type: None,
        detail: None,
    }))])];
    assert!(has_code(
        &run(&req2, &c, Protocol::OpenAiChat).unwrap(),
        "dropped_expired_media_ref"
    ));
}

// --- orphan tool result ---
#[test]
fn orphan_tool_result() {
    let mut req = base_request();
    req.messages = vec![user(vec![UserPart::ToolResult(ToolResult {
        tool_call_id: ToolCallId("nope".into()),
        content: vec![ToolResultPart::Text("r".into())],
        is_error: false,
        added_tool_names: None,
    })])];
    assert!(matches!(
        run(
            &req,
            &caps(false, false, false, true, false, false),
            Protocol::OpenAiChat
        ),
        Err(AiError::Validation(ValidationError::OrphanToolResult(_)))
    ));
}

// --- missing tool result (paired protocols) ---
#[test]
fn missing_tool_result_paired_protocols() {
    let mut req = base_request();
    req.messages = vec![
        Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId("call_1".into()),
                name: "t".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            })],
            model: ModelId("target".into()),
            protocol: Protocol::AnthropicMessages,
        }),
        Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text("next".into())],
            model: ModelId("target".into()),
            protocol: Protocol::AnthropicMessages,
        }),
    ];
    let c = caps(false, false, false, true, false, false);
    assert!(matches!(
        run(&req, &c, Protocol::AnthropicMessages),
        Err(AiError::Validation(ValidationError::MissingToolResult(_)))
    ));
    req.compatibility = Lossy;
    assert!(has_code(
        &run(&req, &c, Protocol::AnthropicMessages).unwrap(),
        "missing_tool_result"
    ));
}
