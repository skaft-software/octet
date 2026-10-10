//! Request half of the OpenAI Chat Completions codec.
//!
//! Owns the private `ChatCompletionsRequest` tree that is serialized onto the
//! wire and `build_request`, the single place a validated `Request` becomes
//! `HttpRequestParts`. It is separate from the response half because the two
//! fail in opposite directions: everything here fails before a byte leaves the
//! process — reasoning normalization, capability validation, cache-control
//! placement, tool-schema replay — and can therefore be exercised end to end
//! with no server, no stream, and no response DTO in scope.

use serde::{Deserialize, Serialize};

use crate::error::{AiError, ConfigError, DecodeError};
use crate::protocol::{
    cache_control, cache_session_id, prompt_cache_key, Base64Bytes, CacheControl, HttpRequestParts,
    WireImageUrl,
};
use crate::types::{
    AssistantPart, AudioFormat, AudioPayload, AudioVoice, ImageSource, Media, Message,
    OpenAiChatReasoningMode, OutputFormat, OutputModalities, Protocol, ReasoningConfig, Request,
    ToolChoice, ToolResultPart, UserPart,
};
use crate::validate::{normalize_request_reasoning, validate_request};

// --- Request DTOs ---

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatCompletionsMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ChatReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ChatThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ChatResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<ChatAudioOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_retention: Option<&'static str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<ChatStreamOptions>,
}

#[derive(Serialize)]
struct ChatStreamOptions {
    include_usage: bool,
}

/// DeepSeek's documented OpenAI-compatible thinking toggle.
///
/// See <https://api-docs.deepseek.com/guides/thinking_mode>.
#[derive(Serialize)]
struct ChatReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}

#[derive(Serialize)]
struct ChatTemplateThinking {
    enable_thinking: bool,
    preserve_thinking: bool,
}

#[derive(Serialize)]
struct ChatThinkingConfig {
    r#type: &'static str,
}

#[derive(Serialize)]
#[serde(tag = "role")]
enum ChatCompletionsMessage {
    #[serde(rename = "developer")]
    Developer { content: ChatInstructionContent },
    #[serde(rename = "system")]
    System { content: ChatInstructionContent },
    #[serde(rename = "user")]
    User { content: ChatInstructionContent },
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<ChatInstructionContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ChatToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        audio: Option<ChatAssistantAudioRef>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatInstructionContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatContentPart {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ImageUrl {
        image_url: ChatImageUrl,
    },
    InputAudio {
        input_audio: ChatInputAudio,
    },
    /// Mistral's replayable reasoning-content chunk.
    Thinking {
        thinking: Vec<ChatThinkingPart>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatThinkingPart {
    Text { text: String },
}

#[derive(Serialize)]
struct ChatImageUrl {
    url: WireImageUrl,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct ChatInputAudio {
    data: Base64Bytes,
    format: String,
}

#[derive(Serialize)]
struct ChatAssistantAudioRef {
    id: String,
}

#[derive(Serialize)]
struct ChatToolCall {
    id: String,
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    function: Option<ChatFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom: Option<ChatCustomCall>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ChatCustomCall {
    pub(super) name: String,
    pub(super) input: String,
}

#[derive(Serialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatTool {
    Function(ChatFunctionTool),
    Custom(ChatCustomTool),
}

#[derive(Serialize)]
struct ChatFunctionTool {
    r#type: &'static str,
    function: ChatFunctionDef,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
struct ChatFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
    /// Always emitted: providers that reject unknown fields are excluded from
    /// strict routes; the flag is `false` unless the tool requested and the
    /// route could enforce the rewritten schema.
    strict: bool,
}

/// OpenAI `custom` tool constrained by a Lark/regex grammar.
#[derive(Serialize)]
struct ChatCustomTool {
    r#type: &'static str,
    custom: ChatCustomDef,
}

#[derive(Serialize)]
struct ChatCustomDef {
    name: String,
    description: String,
    format: ChatCustomFormat,
}

#[derive(Serialize)]
struct ChatCustomFormat {
    r#type: &'static str,
    grammar: ChatGrammar,
}

#[derive(Serialize)]
struct ChatGrammar {
    syntax: String,
    definition: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatResponseFormat {
    JsonObject,
    JsonSchema { json_schema: ChatJsonSchema },
}

#[derive(Serialize)]
struct ChatJsonSchema {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    schema: serde_json::Value,
    strict: bool,
}

#[derive(Serialize)]
struct ChatAudioOptions {
    voice: serde_json::Value, // string or object
    format: String,
}

// --- Request construction ---

/// Mistral accepts tool-call IDs with exactly nine ASCII alphanumeric bytes.
/// Keep a valid existing ID, otherwise derive a deterministic opaque ID without
/// changing canonical IDs used by other endpoints.
pub(super) fn mistral_tool_call_id(id: &str) -> String {
    let compact: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    if compact.len() == 9 {
        return compact;
    }

    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")[..9].to_owned()
}

/// Builds the OpenAI Chat Completions HTTP request parts.
pub(crate) fn build_request(
    model: &crate::catalog::Model,
    req: &Request,
) -> Result<HttpRequestParts, AiError> {
    // 1. Normalize model-gated reasoning, then run validation.
    let defaults = crate::protocol::preset::request_defaults(model, req)?;
    let req = normalize_request_reasoning(&defaults, &model.spec.capabilities);
    let diagnostics = validate_request(
        &req,
        &model.spec.capabilities,
        &model.spec.limits,
        Protocol::OpenAiChat,
        &model.spec.id,
        req.compatibility,
    )?;
    let mistral_profile = model.endpoint.runtime.openai_chat_profile
        == crate::types::OpenAiChatRuntimeProfile::Mistral;

    // 2. Map system prompt
    let reasoning_capability = model.spec.capabilities.reasoning.as_ref();
    let has_reasoning = reasoning_capability.is_some();
    let reasoning_mode = reasoning_capability.map(|capability| &capability.openai_chat_mode);
    let deepseek_thinking = matches!(
        reasoning_mode,
        Some(OpenAiChatReasoningMode::DeepSeekThinking | OpenAiChatReasoningMode::DeepSeekToggle)
    );
    let cerebras_reasoning = matches!(reasoning_mode, Some(OpenAiChatReasoningMode::Cerebras));
    let openrouter_reasoning = matches!(reasoning_mode, Some(OpenAiChatReasoningMode::OpenRouter));
    let provider_uses_system_message = matches!(
        reasoning_mode,
        Some(
            OpenAiChatReasoningMode::ProviderValues {
                system_message: true,
                ..
            } | OpenAiChatReasoningMode::Cerebras
                | OpenAiChatReasoningMode::QwenEnableThinking
                | OpenAiChatReasoningMode::QwenChatTemplate { .. }
                | OpenAiChatReasoningMode::Together { .. }
        )
    );
    let provider_uses_system_message = provider_uses_system_message
        || mistral_profile
        || model
            .spec
            .preset
            .thinking_format
            .is_some_and(|format| format != crate::ThinkingFormat::OpenAi);
    let mut messages = Vec::new();
    let cache_marker = if matches!(
        model.spec.cache.cache_control_format,
        Some(crate::types::CacheControlFormat::Anthropic)
    ) {
        cache_control(&req, &model.spec.cache)
    } else {
        None
    };
    if let Some(ref sys) = req.system {
        // DeepSeek's documented Chat Completions examples use `system`; its
        // reasoning extension is not OpenAI's developer-message convention.
        let content = cache_marker.map_or_else(
            || ChatInstructionContent::Text(sys.clone()),
            |marker| {
                ChatInstructionContent::Parts(vec![ChatContentPart::Text {
                    text: sys.clone(),
                    cache_control: Some(marker),
                }])
            },
        );
        if has_reasoning
            && !deepseek_thinking
            && !matches!(reasoning_mode, Some(OpenAiChatReasoningMode::SystemMessage))
            && !provider_uses_system_message
        {
            messages.push(ChatCompletionsMessage::Developer { content });
        } else {
            messages.push(ChatCompletionsMessage::System { content });
        }
    }

    // 3. Map messages
    for msg in &req.messages {
        match msg {
            Message::User(ref user) => {
                let mut parts = Vec::new();
                for part in &user.content {
                    match part {
                        UserPart::Text(ref text) => {
                            parts.push(ChatContentPart::Text {
                                text: text.clone(),
                                cache_control: None,
                            });
                        }
                        UserPart::Media(Media::Image(ref image)) => {
                            if !model
                                .spec
                                .capabilities
                                .input_modalities
                                .contains(crate::types::Modality::Image)
                            {
                                continue;
                            }

                            let url = match &image.source {
                                ImageSource::Url(url) => WireImageUrl::Url(url.to_string()),
                                ImageSource::Inline(bytes) => {
                                    // No documented default MIME; guessing a wire
                                    // field is forbidden (design §75). Validation
                                    // has already diagnosed the drop, so skip the
                                    // part when the media type is absent.
                                    let Some(media_type) = image.media_type.as_ref() else {
                                        continue;
                                    };
                                    WireImageUrl::Inline {
                                        media_type: media_type.to_string(),
                                        data: bytes.clone(),
                                    }
                                }
                                ImageSource::ProviderRef(_) => continue,
                            };

                            let detail = image.detail.map(|d| match d {
                                crate::types::ImageDetail::Auto => "auto".to_string(),
                                crate::types::ImageDetail::Low => "low".to_string(),
                                crate::types::ImageDetail::High => "high".to_string(),
                            });

                            parts.push(ChatContentPart::ImageUrl {
                                image_url: ChatImageUrl { url, detail },
                            });
                        }
                        UserPart::Media(Media::Audio(ref audio)) => {
                            if !model
                                .spec
                                .capabilities
                                .input_modalities
                                .contains(crate::types::Modality::Audio)
                            {
                                continue;
                            }

                            let format_str = match audio.format {
                                AudioFormat::Wav => "wav".to_string(),
                                AudioFormat::Mp3 => "mp3".to_string(),
                                _ => continue,
                            };

                            let data = match &audio.payload {
                                AudioPayload::Inline(bytes) => Base64Bytes::from(bytes),
                                AudioPayload::InlineWithProviderRef { data, .. } => {
                                    Base64Bytes::from(data)
                                }
                                AudioPayload::ProviderRef(_) => continue,
                            };

                            parts.push(ChatContentPart::InputAudio {
                                input_audio: ChatInputAudio {
                                    data,
                                    format: format_str,
                                },
                            });
                        }
                        UserPart::ToolResult(ref tr) => {
                            // Preserve canonical order: flush any buffered user
                            // content before this `role:"tool"` message rather
                            // than emitting all tool results first (design §11).
                            if !parts.is_empty() {
                                messages.push(ChatCompletionsMessage::User {
                                    content: compact_text_content(std::mem::take(&mut parts)),
                                });
                            }
                            let mut text_parts = Vec::new();
                            for tr_part in &tr.content {
                                if let ToolResultPart::Text(ref text) = tr_part {
                                    text_parts.push(text.clone());
                                }
                            }
                            messages.push(ChatCompletionsMessage::Tool {
                                tool_call_id: if mistral_profile {
                                    mistral_tool_call_id(&tr.tool_call_id.0)
                                } else {
                                    crate::protocol::normalize_tool_call_id(&tr.tool_call_id.0)
                                },
                                content: text_parts.join("\n"),
                            });
                        }
                    }
                }
                if !parts.is_empty() {
                    messages.push(ChatCompletionsMessage::User {
                        content: compact_text_content(parts),
                    });
                }
            }
            Message::Assistant(ref assistant) => {
                let mut text_parts = Vec::new();
                let mut mistral_content_parts = Vec::new();
                let mut reasoning_parts = Vec::new();
                let mut tool_calls = Vec::new();
                let mut audio_ref = None;

                for part in &assistant.content {
                    match part {
                        AssistantPart::Text(ref text) => {
                            text_parts.push(text.clone());
                            if mistral_profile {
                                mistral_content_parts.push(ChatContentPart::Text {
                                    text: text.clone(),
                                    cache_control: None,
                                });
                            }
                        }
                        AssistantPart::Reasoning(reasoning)
                            if mistral_profile
                                && assistant.protocol == Protocol::OpenAiChat
                                && assistant.model == model.spec.id =>
                        {
                            if let Some(text) = &reasoning.text {
                                mistral_content_parts.push(ChatContentPart::Thinking {
                                    thinking: vec![ChatThinkingPart::Text { text: text.clone() }],
                                });
                            }
                        }
                        // DeepSeek requires the previous turn's full
                        // `reasoning_content` when that turn called a tool.
                        // Sending it for ordinary same-model turns is allowed
                        // (the API ignores it), and retaining the model check
                        // prevents cross-model Chat reasoning from leaking in.
                        AssistantPart::Reasoning(reasoning)
                            if (deepseek_thinking
                                || cerebras_reasoning
                                || model.spec.preset.thinking_format
                                    == Some(crate::ThinkingFormat::StringThinking))
                                && assistant.protocol == Protocol::OpenAiChat
                                && assistant.model == model.spec.id =>
                        {
                            if let Some(text) = &reasoning.text {
                                reasoning_parts.push(text.clone());
                            }
                        }
                        // Most OpenAI-compatible local servers expose reasoning
                        // as a response-only field. Replaying an assistant turn
                        // containing only that field serializes as `content:
                        // null`, which llama.cpp rejects before it can see the
                        // following tool result. Preserve it as ordinary
                        // assistant text instead; DeepSeek is handled above
                        // because it requires the dedicated field.
                        AssistantPart::Reasoning(reasoning) => {
                            if let Some(text) = &reasoning.text {
                                text_parts.push(text.clone());
                            }
                        }
                        AssistantPart::ToolCall(ref tc) => {
                            let property = crate::protocol::grammar::input_property(
                                &req.tools,
                                &tc.name,
                                crate::protocol::grammar_tools_for(model),
                            )?;
                            let custom = property
                                .as_deref()
                                .map(|property| {
                                    crate::protocol::grammar::replay_input(
                                        &tc.arguments_json,
                                        property,
                                    )
                                    .map(|input| {
                                        ChatCustomCall {
                                            name: tc.name.clone(),
                                            input,
                                        }
                                    })
                                })
                                .transpose()?;
                            tool_calls.push(ChatToolCall {
                                id: if mistral_profile {
                                    mistral_tool_call_id(&tc.id.0)
                                } else {
                                    crate::protocol::normalize_tool_call_id(&tc.id.0)
                                },
                                r#type: if custom.is_some() {
                                    "custom"
                                } else {
                                    "function"
                                }
                                .to_owned(),
                                function: custom.is_none().then(|| ChatFunctionCall {
                                    name: tc.name.clone(),
                                    arguments: tc.arguments_json.clone(),
                                }),
                                custom,
                            });
                        }
                        AssistantPart::Media(Media::Audio(ref audio)) => {
                            // Design §7: only replay an assistant audio id whose
                            // reference is still usable (same protocol, not
                            // expired). An expired/wrong-protocol ref is dropped
                            // rather than serialized.
                            let reference = match &audio.payload {
                                AudioPayload::InlineWithProviderRef { reference, .. } => {
                                    Some(reference)
                                }
                                AudioPayload::ProviderRef(reference) => Some(reference),
                                AudioPayload::Inline(_) => None,
                            };
                            if let Some(reference) = reference {
                                if crate::validate::provider_ref_is_usable(
                                    reference,
                                    Protocol::OpenAiChat,
                                ) {
                                    audio_ref = Some(ChatAssistantAudioRef {
                                        id: reference.id.clone(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let content_str = if mistral_profile {
                    (!mistral_content_parts.is_empty())
                        .then_some(ChatInstructionContent::Parts(mistral_content_parts))
                } else if text_parts.is_empty() {
                    None
                } else {
                    Some(ChatInstructionContent::Text(text_parts.join("\n")))
                };
                let reasoning_content = (!mistral_profile && !reasoning_parts.is_empty())
                    .then(|| reasoning_parts.join("\n"));
                let tool_calls_opt = if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                };

                messages.push(ChatCompletionsMessage::Assistant {
                    content: content_str,
                    reasoning: cerebras_reasoning
                        .then(|| reasoning_content.clone())
                        .flatten(),
                    reasoning_content: if cerebras_reasoning {
                        None
                    } else {
                        reasoning_content
                    },
                    tool_calls: tool_calls_opt,
                    audio: audio_ref,
                });
            }
        }
    }

    if let Some(marker) = cache_marker {
        add_cache_control_to_last_conversation_message(&mut messages, marker);
    }

    // 4. Map tools and tool_choice
    // Registry announcements are local metadata, not a provider load operation.
    // Until a native deferred-load codec exists, always send every schema.
    let active_tools: Vec<&crate::types::ToolDef> = req.tools.iter().collect();
    let tools_opt = if active_tools.is_empty() || !model.spec.capabilities.tools {
        None
    } else {
        let mut built = Vec::with_capacity(active_tools.len());
        for (index, tool) in active_tools.iter().enumerate() {
            // Grammar-constrained tools are caller-opted OpenAI `custom` tools;
            // every other tool is a strict-resolved function tool.
            if let Some(grammar) = crate::constrained_sampling::resolve_grammar(
                tool,
                crate::protocol::grammar_tools_for(model),
            )? {
                built.push(ChatTool::Custom(ChatCustomTool {
                    r#type: "custom",
                    custom: ChatCustomDef {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        format: ChatCustomFormat {
                            r#type: "grammar",
                            grammar: ChatGrammar {
                                syntax: grammar.format.to_owned(),
                                definition: grammar.definition,
                            },
                        },
                    },
                }));
                continue;
            }
            let (parameters, strict) = crate::constrained_sampling::function_tool_parameters(
                tool,
                crate::protocol::strict_mode_for(model),
            )?;
            built.push(ChatTool::Function(ChatFunctionTool {
                r#type: "function",
                function: ChatFunctionDef {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters,
                    strict,
                },
                cache_control: (index + 1 == active_tools.len()
                    && model.spec.cache.supports_cache_control_on_tools)
                    .then_some(cache_marker)
                    .flatten(),
            }));
        }
        Some(built)
    };

    let tool_choice_opt = if !model.spec.capabilities.tools || req.tools.is_empty() {
        None
    } else {
        match &req.tool_choice {
            ToolChoice::Auto => Some(serde_json::Value::String("auto".to_string())),
            ToolChoice::Required => Some(serde_json::Value::String("required".to_string())),
            ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
            ToolChoice::Named(name) => Some(
                if crate::protocol::grammar::input_property(
                    &req.tools,
                    name,
                    crate::protocol::grammar_tools_for(model),
                )?
                .is_some()
                {
                    serde_json::json!({"type": "custom", "custom": {"name": name}})
                } else {
                    serde_json::json!({"type": "function", "function": {"name": name}})
                },
            ),
        }
    };

    // Typed profiles are independent: a model's family does not establish its
    // serving endpoint's control format. Exact supported choices were validated.
    let enabled = req.reasoning != ReasoningConfig::Off;
    let always_on =
        reasoning_capability.is_some_and(|c| c.control == crate::types::ReasoningControl::AlwaysOn);
    let wire = reasoning_capability
        .and_then(|c| c.wire_value(&req.reasoning))
        .filter(|value| !value.eq_ignore_ascii_case("default"));
    let emits_effort = !always_on
        && !matches!(
            reasoning_mode,
            Some(
                OpenAiChatReasoningMode::OpenRouter
                    | OpenAiChatReasoningMode::DeepSeekToggle
                    | OpenAiChatReasoningMode::QwenEnableThinking
                    | OpenAiChatReasoningMode::QwenChatTemplate { .. }
                    | OpenAiChatReasoningMode::Together { effort: false }
            )
        );
    let reasoning_effort = if emits_effort && (!deepseek_thinking || enabled) {
        wire.clone()
    } else {
        None
    };
    let reasoning = if openrouter_reasoning && !always_on && enabled {
        // OpenRouter Off leaves the endpoint default in force, including for
        // auxiliary summary requests. Never manufacture an explicit disable.
        if reasoning_capability.is_some_and(|c| c.control == crate::types::ReasoningControl::Toggle)
        {
            Some(ChatReasoningConfig {
                effort: None,
                enabled: Some(true),
            })
        } else {
            wire.map(|effort| ChatReasoningConfig {
                effort: Some(effort),
                enabled: None,
            })
        }
    } else if matches!(
        reasoning_mode,
        Some(OpenAiChatReasoningMode::Together { .. })
    ) && !always_on
    {
        Some(ChatReasoningConfig {
            effort: None,
            enabled: Some(enabled),
        })
    } else {
        None
    };
    let thinking = (deepseek_thinking && !always_on).then_some(ChatThinkingConfig {
        r#type: if enabled { "enabled" } else { "disabled" },
    });
    let enable_thinking = matches!(
        reasoning_mode,
        Some(OpenAiChatReasoningMode::QwenEnableThinking)
    )
    .then_some(enabled);
    let chat_template_kwargs = match reasoning_mode {
        Some(OpenAiChatReasoningMode::QwenChatTemplate { preserve_thinking }) => {
            Some(ChatTemplateThinking {
                enable_thinking: enabled,
                preserve_thinking: *preserve_thinking,
            })
        }
        _ => None,
    };

    // 6. Response format
    let response_format_opt = if model.spec.capabilities.structured_output {
        match &req.output_format {
            OutputFormat::Text => None,
            OutputFormat::JsonObject => Some(ChatResponseFormat::JsonObject),
            OutputFormat::JsonSchema(ref s) => Some(ChatResponseFormat::JsonSchema {
                json_schema: ChatJsonSchema {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    schema: s.schema.clone(),
                    strict: s.strict,
                },
            }),
        }
    } else {
        None
    };

    // 7. Modalities & Audio Output
    let mut modalities_opt = None;
    let mut audio_opt = None;
    let mut streaming = true;

    if let OutputModalities::TextAndAudio(ref opts) = req.output_modalities {
        if model
            .spec
            .capabilities
            .output_modalities
            .contains(crate::types::Modality::Audio)
        {
            modalities_opt = Some(vec!["text".to_string(), "audio".to_string()]);
            streaming = false;

            let voice_val = match &opts.voice {
                AudioVoice::Named(ref s) => serde_json::Value::String(s.clone()),
                AudioVoice::ProviderRef(ref id) => serde_json::json!({ "id": id }),
            };

            let format_str = match opts.format {
                AudioFormat::Wav => "wav".to_string(),
                AudioFormat::Mp3 => "mp3".to_string(),
                AudioFormat::Aac => "aac".to_string(),
                AudioFormat::Flac => "flac".to_string(),
                AudioFormat::Opus => "opus".to_string(),
                AudioFormat::Pcm16 => "pcm16".to_string(),
            };

            audio_opt = Some(ChatAudioOptions {
                voice: voice_val,
                format: format_str,
            });
        }
    }

    let stream_options = if streaming && !mistral_profile {
        Some(ChatStreamOptions {
            include_usage: true,
        })
    } else {
        None
    };

    // Model limits are local capacity metadata, not request defaults. Only
    // forward a cap explicitly chosen by the caller. DeepSeek and Mistral use
    // the compatible `max_tokens` field, while current OpenAI Chat uses
    // `max_completion_tokens`.
    let output_cap = crate::effective_output_token_cap(model, req.max_output_tokens);
    let (max_tokens, max_completion_tokens) = if deepseek_thinking || mistral_profile {
        (output_cap, None)
    } else {
        (None, output_cap)
    };

    let chat_req = ChatCompletionsRequest {
        model: model.spec.api_name.clone(),
        messages,
        tools: tools_opt,
        tool_choice: tool_choice_opt,
        max_tokens,
        max_completion_tokens,
        temperature: req.temperature,
        stop: req.stop.clone(),
        reasoning_effort,
        reasoning,
        thinking,
        enable_thinking,
        chat_template_kwargs,
        response_format: response_format_opt,
        modalities: modalities_opt,
        audio: audio_opt,
        prompt_cache_key: {
            let direct_openai = model
                .endpoint
                .base_url
                .to_string()
                .contains("api.openai.com");
            ((direct_openai && req.cache_retention != crate::types::CacheRetention::None)
                || (req.cache_retention == crate::types::CacheRetention::Long
                    && model.spec.cache.supports_long_retention))
                .then(|| prompt_cache_key(&req))
                .flatten()
        },
        prompt_cache_retention: (req.cache_retention == crate::types::CacheRetention::Long
            && model.spec.cache.supports_long_retention)
            .then_some("24h"),
        stream: streaming,
        stream_options,
    };

    let mut body = serde_json::to_value(&chat_req)
        .map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;
    crate::protocol::preset::chat(model, &req, &mut body)?;
    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;

    let url = crate::protocol::endpoint_url(&model.endpoint.base_url, "chat/completions")?;

    let mut headers = http::HeaderMap::new();
    let affinity_format = (model.spec.cache.send_session_affinity_headers
        && !crate::protocol::is_opencode_session_route(model))
    .then_some(
        model
            .spec
            .cache
            .session_affinity_format
            .unwrap_or(crate::types::SessionAffinityFormat::OpenAi),
    );
    if let (Some(format), Some(session_id)) = (affinity_format, cache_session_id(&req)) {
        let value = http::HeaderValue::from_str(session_id)
            .map_err(|_| ConfigError::InvalidHeader("session affinity".into()))?;
        match format {
            crate::types::SessionAffinityFormat::OpenAi => {
                headers.insert(http::HeaderName::from_static("session_id"), value.clone());
                headers.insert(
                    http::HeaderName::from_static("x-client-request-id"),
                    value.clone(),
                );
                headers.insert(http::HeaderName::from_static("x-session-affinity"), value);
            }
            crate::types::SessionAffinityFormat::OpenAiNoSession => {
                headers.insert(
                    http::HeaderName::from_static("x-client-request-id"),
                    value.clone(),
                );
                headers.insert(http::HeaderName::from_static("x-session-affinity"), value);
            }
            crate::types::SessionAffinityFormat::OpenRouter => {
                headers.insert(http::HeaderName::from_static("x-session-id"), value);
            }
            crate::types::SessionAffinityFormat::Mistral => {
                headers.insert(http::HeaderName::from_static("x-affinity"), value);
            }
            // Codex is a Responses route; accepting this value here keeps
            // configuration forward-compatible without emitting invalid Chat
            // headers.
            crate::types::SessionAffinityFormat::Codex => {}
        }
    }
    crate::protocol::add_opencode_session_header(model, &req, &mut headers)?;

    Ok(HttpRequestParts {
        url,
        headers,
        body: bytes::Bytes::from(body_bytes),
        streaming,
        diagnostics,
    })
}

/// Prefer the universally supported string form for plain-text user content.
/// Multipart content remains an array for image/audio requests and for
/// provider-specific cache-control annotations.
fn compact_text_content(parts: Vec<ChatContentPart>) -> ChatInstructionContent {
    if !parts.is_empty()
        && parts.iter().all(|part| {
            matches!(
                part,
                ChatContentPart::Text {
                    cache_control: None,
                    ..
                }
            )
        })
    {
        let text = parts
            .into_iter()
            .map(|part| match part {
                ChatContentPart::Text { text, .. } => text,
                _ => unreachable!("all parts were checked as text"),
            })
            .collect();
        ChatInstructionContent::Text(text)
    } else {
        ChatInstructionContent::Parts(parts)
    }
}

fn add_cache_control_to_last_conversation_message(
    messages: &mut [ChatCompletionsMessage],
    marker: CacheControl,
) {
    for message in messages.iter_mut().rev() {
        let applied = match message {
            ChatCompletionsMessage::User { content } => {
                add_cache_control_to_instruction(content, marker)
            }
            ChatCompletionsMessage::Assistant { content, .. } => content
                .as_mut()
                .and_then(|content| add_cache_control_to_instruction(content, marker)),
            ChatCompletionsMessage::Developer { .. }
            | ChatCompletionsMessage::System { .. }
            | ChatCompletionsMessage::Tool { .. } => None,
        };
        if applied.is_some() {
            return;
        }
    }
}

fn add_cache_control_to_instruction(
    content: &mut ChatInstructionContent,
    marker: CacheControl,
) -> Option<()> {
    match content {
        ChatInstructionContent::Text(text) if !text.is_empty() => {
            let text = std::mem::take(text);
            *content = ChatInstructionContent::Parts(vec![ChatContentPart::Text {
                text,
                cache_control: Some(marker),
            }]);
            Some(())
        }
        ChatInstructionContent::Parts(parts) => {
            parts.iter_mut().rev().find_map(|part| match part {
                ChatContentPart::Text { cache_control, .. } => {
                    *cache_control = Some(marker);
                    Some(())
                }
                ChatContentPart::ImageUrl { .. }
                | ChatContentPart::InputAudio { .. }
                | ChatContentPart::Thinking { .. } => None,
            })
        }
        ChatInstructionContent::Text(_) => None,
    }
}
