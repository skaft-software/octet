//! Completed-response half of the OpenAI Chat Completions codec.
//!
//! Owns the `ChatCompletionsResponse` DTO tree, the non-streaming response
//! decoder, and the two wire-value maps that turn provider text into shared
//! types — `map_stop_reason` and `map_usage`. It is separate from `request`
//! because these are the only places a response can be rejected after the
//! provider has already accepted the call, and separate from `stream` because
//! a completed body is one self-contained JSON document while a stream is an
//! unbounded sequence of partial ones. The frame DTOs therefore live in
//! `stream`, with the one exception of `ChatUsage`, which both halves read.

use base64::prelude::*;
use serde::Deserialize;

use crate::error::{AiError, DecodeError};
use crate::stream::ResponseBuilder;
use crate::types::{
    AssistantMessage, AssistantPart, AudioFormat, AudioMedia, AudioPayload, Media, Protocol,
    ProviderMediaRef, ReasoningPart, Response, StopReason, ToolArgumentValidation, ToolCall,
    ToolCallArgumentError, ToolCallId, ToolDef, Usage,
};

use super::compat::{
    consume_qwen_xml_content, content_may_contain_tool_call, flush_qwen_xml_pending,
    TOOL_OUTPUT_LOCKED,
};
use super::request::ChatCustomCall;

// --- Completed-response DTOs ---

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    id: String,
    choices: Vec<ChatChoice>,
    usage: ChatUsage,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    // `role` is always "assistant" here and is not needed after decode; the wire
    // field is ignored rather than stored.
    #[serde(default)]
    content: Option<ChatResponseContent>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatResponseMessageToolCall>>,
    #[serde(default)]
    audio: Option<ChatAudioResponse>,
}

/// OpenAI-compatible endpoints normally return a string. Mistral's Chat
/// Completions endpoint can instead return structured text and thinking chunks.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ChatResponseContent {
    Text(String),
    Parts(Vec<ChatResponseContentPart>),
}

#[derive(Deserialize)]
pub(super) struct ChatResponseContentPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Vec<ChatThinkingResponsePart>,
}

#[derive(Deserialize)]
struct ChatThinkingResponsePart {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

pub(super) enum ChatContentFragment {
    Text(String),
    Reasoning(String),
}

pub(super) fn content_fragments(content: &ChatResponseContent) -> Vec<ChatContentFragment> {
    match content {
        ChatResponseContent::Text(text) => vec![ChatContentFragment::Text(text.clone())],
        ChatResponseContent::Parts(parts) => parts
            .iter()
            .flat_map(|part| match part.kind.as_str() {
                "text" => part
                    .text
                    .as_ref()
                    .map(|text| vec![ChatContentFragment::Text(text.clone())])
                    .unwrap_or_default(),
                "thinking" => part
                    .thinking
                    .iter()
                    .filter(|thinking| thinking.kind.as_deref().unwrap_or("text") == "text")
                    .filter_map(|thinking| {
                        thinking
                            .text
                            .as_ref()
                            .map(|text| ChatContentFragment::Reasoning(text.clone()))
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect(),
    }
}

#[derive(Deserialize)]
struct ChatResponseMessageToolCall {
    id: String,
    #[serde(default)]
    function: Option<ChatResponseMessageFunction>,
    #[serde(default)]
    custom: Option<ChatCustomCall>,
}

#[derive(Deserialize)]
struct ChatResponseMessageFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatAudioResponse {
    id: String,
    data: String,
    transcript: String,
    expires_at: u64,
}

#[derive(Deserialize)]
pub(super) struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<ChatPromptTokensDetails>,
    // OpenRouter and some OpenAI-compatible gateways expose this legacy
    // top-level spelling instead of `prompt_tokens_details.cached_tokens`.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens_details: Option<ChatCompletionTokensDetails>,
}

#[derive(Deserialize, Default)]
struct ChatPromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
    // Unlike direct OpenAI Chat, compatible gateways can report writes in the
    // same bucket. Keep it disjoint from full-rate prompt input.
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
struct ChatCompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

/// Decodes the non-streaming JSON response body from OpenAI Chat Completions.
#[cfg(test)]
pub(crate) fn decode_response(
    model: &crate::catalog::Model,
    body: &[u8],
    requested_audio_format: Option<AudioFormat>,
) -> Result<Response, AiError> {
    decode_response_inner(model, body, requested_audio_format, None)
}

/// Decodes a completed response while validating calls against the exact
/// tool-definition snapshot sent with the request.
pub(crate) fn decode_response_with_tools(
    model: &crate::catalog::Model,
    body: &[u8],
    requested_audio_format: Option<AudioFormat>,
    tools: &[ToolDef],
) -> Result<Response, AiError> {
    decode_response_inner(model, body, requested_audio_format, Some(tools))
}

fn decode_response_inner(
    model: &crate::catalog::Model,
    body: &[u8],
    requested_audio_format: Option<AudioFormat>,
    tool_definitions: Option<&[ToolDef]>,
) -> Result<Response, AiError> {
    if let Some(tools) = tool_definitions {
        crate::json_repair::validate_tool_definitions(tools).map_err(AiError::Decode)?;
    }
    let resp: ChatCompletionsResponse = serde_json::from_slice(body)
        .map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;

    let choice = resp
        .choices
        .first()
        .ok_or_else(|| AiError::Decode(DecodeError::Json("Empty choices array".to_string())))?;

    let mut content = Vec::new();

    // 1. Map content text. A non-streaming OpenAI-compatible response can
    // have the same Qwen XML fallback as the streaming path when vLLM's
    // parser was not configured.
    let has_native_tool_calls = choice
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());
    let mut recovered_content_tool = false;
    let mut recovered_locked_placeholder = false;
    for fragment in choice
        .message
        .content
        .as_ref()
        .map(content_fragments)
        .unwrap_or_default()
    {
        match fragment {
            ChatContentFragment::Text(text) if !text.is_empty() => {
                if model.spec.capabilities.tools
                    && !has_native_tool_calls
                    && content_may_contain_tool_call(&text)
                {
                    let mut fallback =
                        ResponseBuilder::new(model.spec.id.clone(), Protocol::OpenAiChat, None);
                    // The completed body is already fully available, so enabling
                    // ambiguous compatibility recovery cannot hide streamed JSON.
                    fallback.set_buffer_ambiguous_compatibility_content(true);
                    if let Some(tools) = tool_definitions {
                        fallback.set_tool_definitions(tools)?;
                        fallback.strict_tool_sampling = crate::protocol::strict_mode_for(model);
                    }
                    let mut ignored_events = Vec::new();
                    consume_qwen_xml_content(&mut ignored_events, &mut fallback, &text)?;
                    flush_qwen_xml_pending(&mut ignored_events, &mut fallback)?;
                    recovered_locked_placeholder |= fallback.tool_output_locked_seen;
                    let fallback_response = fallback.finish()?;
                    recovered_content_tool |= fallback_response
                        .message
                        .content
                        .iter()
                        .any(|part| matches!(part, AssistantPart::ToolCall(_)));
                    content.extend(fallback_response.message.content);
                } else {
                    let sanitized = text.replace(TOOL_OUTPUT_LOCKED, "");
                    recovered_locked_placeholder |= sanitized.len() != text.len();
                    if !sanitized.is_empty() {
                        content.push(AssistantPart::Text(sanitized));
                    }
                }
            }
            ChatContentFragment::Reasoning(text) if !text.is_empty() => {
                content.push(AssistantPart::Reasoning(ReasoningPart {
                    text: Some(text),
                    state: None,
                }));
            }
            ChatContentFragment::Text(_) | ChatContentFragment::Reasoning(_) => {}
        }
    }

    // 2. Map reasoning. Some OpenAI-compatible gateways use the flat
    // `reasoning` spelling in completed responses too.
    let reasoning = choice
        .message
        .reasoning_content
        .as_ref()
        .filter(|text| !text.is_empty())
        .or_else(|| {
            choice
                .message
                .reasoning
                .as_ref()
                .filter(|text| !text.is_empty())
        });
    if let Some(reasoning) = reasoning {
        content.push(AssistantPart::Reasoning(ReasoningPart {
            text: Some(reasoning.clone()),
            state: None,
        }));
    }

    // 3. Map tool calls
    if let Some(ref tcs) = choice.message.tool_calls {
        for tc in tcs {
            let (name, mut arguments_json) = match (&tc.function, &tc.custom) {
                (Some(function), None) => (
                    &function.name,
                    crate::json_repair::normalize_json_object(&function.arguments)
                        .map_err(AiError::Decode)?,
                ),
                (None, Some(custom)) => {
                    let property = crate::protocol::grammar::input_property(
                        tool_definitions.unwrap_or_default(),
                        &custom.name,
                        crate::protocol::grammar_tools_for(model),
                    )?
                    .unwrap_or_else(|| "input".to_owned());
                    (
                        &custom.name,
                        serde_json::json!({property: custom.input}).to_string(),
                    )
                }
                _ => {
                    return Err(DecodeError::InvalidProviderField(
                        "tool call must contain exactly one of function or custom".to_owned(),
                    )
                    .into())
                }
            };
            let argument_error = if let Some(tools) = tool_definitions {
                let mut arguments = serde_json::from_str(&arguments_json)
                    .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?;
                crate::constrained_sampling::normalize_tool_arguments(
                    name,
                    &mut arguments,
                    tools,
                    crate::protocol::strict_mode_for(model),
                )?;
                arguments_json = arguments.to_string();
                match crate::json_repair::validate_tool_arguments(name, &arguments, tools)
                    .map_err(AiError::Decode)?
                {
                    ToolArgumentValidation::SchemaMismatch => {
                        Some(ToolCallArgumentError::SchemaMismatch)
                    }
                    ToolArgumentValidation::Valid | ToolArgumentValidation::UnknownTool => None,
                }
            } else {
                None
            };

            content.push(AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId(tc.id.clone()),
                name: name.clone(),
                arguments_json,
                argument_error,
            }));
        }
    }

    // 4. Map audio response
    if let Some(ref audio) = choice.message.audio {
        let decoded_data = BASE64_STANDARD
            .decode(&audio.data)
            .map_err(|_| AiError::Decode(DecodeError::InvalidBase64))?;

        let format = requested_audio_format.ok_or_else(|| {
            AiError::Decode(DecodeError::InvalidProviderField(
                "audio returned without requested output format".to_string(),
            ))
        })?;
        let expires_at_sys = std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(audio.expires_at))
            .ok_or_else(|| {
                AiError::Decode(DecodeError::InvalidProviderField(
                    "audio expires_at is out of range".to_string(),
                ))
            })?;

        content.push(AssistantPart::Media(Media::Audio(AudioMedia {
            payload: AudioPayload::InlineWithProviderRef {
                data: bytes::Bytes::from(decoded_data),
                reference: ProviderMediaRef {
                    protocol: Protocol::OpenAiChat,
                    id: audio.id.clone(),
                    expires_at: Some(expires_at_sys),
                },
            },
            format,
            transcript: Some(audio.transcript.clone()),
        })));
    }

    let message = AssistantMessage {
        content,
        model: model.spec.id.clone(),
        protocol: Protocol::OpenAiChat,
    };
    let stop_reason = choice
        .finish_reason
        .as_deref()
        .map(|reason| {
            if recovered_content_tool && reason == "stop" {
                StopReason::ToolUse
            } else if recovered_locked_placeholder && reason == "stop" {
                StopReason::Other("tool_output_locked".to_string())
            } else {
                map_stop_reason(reason)
            }
        })
        .unwrap_or_else(|| {
            if recovered_content_tool {
                StopReason::ToolUse
            } else if recovered_locked_placeholder {
                StopReason::Other("tool_output_locked".to_string())
            } else {
                StopReason::EndTurn
            }
        });
    let diagnostics = if choice.finish_reason.is_none() {
        vec![defaulted_stop_reason_diagnostic()]
    } else {
        Vec::new()
    };
    let usage = map_usage(&resp.usage)?;

    let cost = model
        .spec
        .pricing
        .as_ref()
        .map(|p| crate::pricing::cost_of(p, &usage).map_err(AiError::Pricing))
        .transpose()?;

    Ok(Response {
        message,
        stop_reason,
        usage,
        cost,
        response_id: Some(resp.id),
        responses_output: None,
        diagnostics,
        deferred: None,
    })
}

pub(super) fn defaulted_stop_reason_diagnostic() -> crate::Diagnostic {
    crate::Diagnostic {
        code: "chat_defaulted_stop_reason".to_owned(),
        message: "Chat completion stop reason was defaulted".to_owned(),
    }
}

pub(super) fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

pub(super) fn map_usage(usage: &ChatUsage) -> Result<Usage, AiError> {
    // Prefer the nested OpenAI field when both aliases are present. They are
    // alternate spellings of the same bucket, not additive counters.
    let cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens)
        .or(usage.prompt_cache_hit_tokens)
        .unwrap_or(0);
    let cache_write = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cache_write_tokens)
        .unwrap_or(0);
    // Chat's documented `prompt_tokens` includes cache reads and writes.
    // Gateways occasionally report detail counters larger than that total;
    // mirror Pi's compatibility behavior by saturating the full-rate bucket
    // instead of failing an otherwise completed response.
    let input = usage
        .prompt_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    let reported_output = usage.completion_tokens;
    let reasoning = usage
        .completion_tokens_details
        .as_ref()
        .map(|d| d.reasoning_tokens)
        .unwrap_or(0);
    // OpenAI defines reasoning as a subset of completion tokens. A few
    // gateways instead report visible completion and reasoning separately; in
    // that shape, combine them so the canonical subset invariant still holds.
    let output = if reasoning > reported_output {
        reported_output
            .checked_add(reasoning)
            .ok_or(AiError::Decode(DecodeError::UsageUnderflow))?
    } else {
        reported_output
    };
    let total = input
        .checked_add(cache_read)
        .and_then(|value| value.checked_add(cache_write))
        .and_then(|value| value.checked_add(output))
        .ok_or(AiError::Decode(DecodeError::UsageUnderflow))?;
    Ok(Usage {
        input_tokens: input,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cache_write_1h_tokens: 0,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: total,
    })
}
