//! Streaming half of the OpenAI Chat Completions codec.
//!
//! Owns the frame DTOs (`ChatChunk`, `ChatStreamChunk` and the delta family),
//! the single-frame decoder `decode_chat_chunk`, and the per-frame state
//! machine `decode_stream_event` that turns those chunks into `StreamEvent`s.
//! It is separate from `response` because the two consume opposite inputs: a
//! completed body is parsed once and rejected wholesale, while a stream is an
//! unbounded sequence of partial documents that must keep working when a field
//! is absent, truncated, or repeated, and whose only observable contract is
//! the event ordering the builder sees.
//!
//! The `ChatStreamChunk` `Deserialize` impl lives here rather than in
//! `response` because it is a property of the frame, not of the stream loop:
//! it is what lets a frame be decoded exactly once while still noticing an
//! error envelope, and the fixture suite asserts that decode count directly.

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::error::{AiError, DecodeError, ProviderError};
use crate::protocol::emit_event;
use crate::protocol::sse::SseEvent;
use crate::stream::{ResponseBuilder, StreamEvent, MAX_TOOL_ARGUMENT_BYTES};
use crate::types::{StopReason, ToolCallId};

use super::compat::{
    consume_qwen_xml_content, emit_text_without_locked_marker, flush_qwen_xml_pending,
};
use super::decode_stream_json;
use super::response::{
    content_fragments, defaulted_stop_reason_diagnostic, map_stop_reason, map_usage,
    ChatContentFragment, ChatResponseContent, ChatUsage,
};

#[derive(Deserialize)]
pub(super) struct ChatChunk {
    // Some OpenAI-compatible providers omit `id` and/or `choices` on trailing
    // usage-only chunks; neither absence is fatal to the stream.
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    pub(super) choices: Vec<ChatChunkChoice>,
    #[serde(default)]
    pub(super) usage: Option<ChatUsage>,
    #[serde(default)]
    timings: crate::inference::wire::RawMetrics,
    #[serde(default)]
    time_info: crate::inference::wire::RawMetrics,
    #[serde(default)]
    x_groq: crate::inference::wire::RawMetrics,
}

/// Decode normal frames directly into their typed DTO, noticing error
/// envelopes without a full-DOM prepass. An untagged enum would buffer every
/// chunk, and its defaulted chunk variant would swallow error-only objects.
struct ChatStreamChunk {
    chunk: ChatChunk,
    has_error: bool,
}

impl<'de> Deserialize<'de> for ChatStreamChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Id,
            Choices,
            Usage,
            Timings,
            TimeInfo,
            XGroq,
            Error,
            #[serde(other)]
            Other,
        }

        struct ChunkVisitor;
        impl<'de> Visitor<'de> for ChunkVisitor {
            type Value = ChatStreamChunk;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct ChatChunk")
            }

            fn visit_seq<A>(self, sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                // Preserve the derived DTO's sequence/default behavior too.
                Ok(ChatStreamChunk {
                    chunk: ChatChunk::deserialize(de::value::SeqAccessDeserializer::new(sequence))?,
                    has_error: false,
                })
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut id = None;
                let mut choices = None;
                let mut usage = None;
                let mut timings = None;
                let mut time_info = None;
                let mut x_groq = None;
                let mut has_error = false;
                while let Some(field) = map.next_key::<Field>()? {
                    match field {
                        Field::Id => {
                            if id.is_some() {
                                return Err(de::Error::duplicate_field("id"));
                            }
                            id = Some(map.next_value()?);
                            continue;
                        }
                        Field::Choices => {
                            if choices.is_some() {
                                return Err(de::Error::duplicate_field("choices"));
                            }
                            choices = Some(map.next_value()?);
                            continue;
                        }
                        Field::Usage => {
                            if usage.is_some() {
                                return Err(de::Error::duplicate_field("usage"));
                            }
                            usage = Some(map.next_value()?);
                            continue;
                        }
                        Field::Timings | Field::TimeInfo | Field::XGroq => {
                            let target = match field {
                                Field::Timings => &mut timings,
                                Field::TimeInfo => &mut time_info,
                                _ => &mut x_groq,
                            };
                            let mut value: crate::inference::wire::RawMetrics = map.next_value()?;
                            if target.is_some() {
                                value.mark_duplicate();
                            }
                            *target = Some(value);
                            continue;
                        }
                        // Error fields were ignored by the original DTO:
                        // their types and duplicates must remain permissive.
                        Field::Error => has_error = true,
                        Field::Other => {}
                    }
                    map.next_value::<IgnoredAny>()?;
                }
                Ok(ChatStreamChunk {
                    chunk: ChatChunk {
                        id: id.unwrap_or_default(),
                        choices: choices.unwrap_or_default(),
                        usage: usage.unwrap_or_default(),
                        timings: timings.unwrap_or_default(),
                        time_info: time_info.unwrap_or_default(),
                        x_groq: x_groq.unwrap_or_default(),
                    },
                    has_error,
                })
            }
        }
        deserializer.deserialize_struct("ChatChunk", &["id", "choices", "usage"], ChunkVisitor)
    }
}

pub(super) fn decode_chat_chunk(data: &str) -> Result<ChatChunk, AiError> {
    let frame: ChatStreamChunk = decode_stream_json(data).map_err(|error| {
        // A valid provider error wins even over malformed chunk fields, in
        // either key order. Only a failed decode or an error-bearing frame
        // needs this permissive fallback, never ordinary text/tool/usage data.
        provider_error_from_stream_event(data)
            .map(AiError::Provider)
            .unwrap_or_else(|| AiError::Decode(DecodeError::Json(error.to_string())))
    })?;
    if frame.has_error {
        if let Some(error) = provider_error_from_stream_event(data) {
            return Err(AiError::Provider(error));
        }
    }
    Ok(frame.chunk)
}

#[derive(Deserialize)]
pub(super) struct ChatChunkChoice {
    delta: ChatChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatChunkDelta {
    #[serde(default)]
    content: Option<ChatResponseContent>,
    #[serde(default)]
    reasoning_content: Option<String>,
    // OpenRouter and several OpenAI-compatible gateways normalize reasoning to
    // a flat `reasoning` string instead of `reasoning_content`; without this
    // alias their reasoning is silently dropped.
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatChunkToolCall>>,
}

#[derive(Deserialize)]
struct ChatChunkToolCall {
    // A single tool call streamed without an explicit index is index 0.
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChatChunkFunction>,
    #[serde(default)]
    custom: Option<ChatChunkCustom>,
}

#[derive(Deserialize)]
struct ChatChunkCustom {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<String>,
}

#[derive(Deserialize)]
struct ChatChunkFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

fn emit_reasoning_delta(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    reasoning: String,
) -> Result<(), AiError> {
    let idx = segment_index(builder, "reasoning");
    if !builder.reasoning_text_buffers.contains_key(&idx) {
        emit_event(events, builder, StreamEvent::ReasoningStart { index: idx })?;
    }
    emit_event(
        events,
        builder,
        StreamEvent::ReasoningDelta {
            index: idx,
            delta: reasoning,
        },
    )
}

fn provider_error_from_stream_event(data: &str) -> Option<ProviderError> {
    let value: serde_json::Value = decode_stream_json(data).ok()?;
    let error = value.get("error").and_then(|value| value.as_object())?;
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("message").and_then(serde_json::Value::as_str))?
        .to_string();
    let code = error
        .get("code")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            error
                .get("code")
                .and_then(|value| value.as_u64().map(|value| value.to_string()))
        })
        .or_else(|| {
            value
                .get("code")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        });
    let kind = error
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    // An explicit outer null/non-string still masks the nested request_id.
    let request_id = value
        .get("request_id")
        .or_else(|| error.get("request_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(ProviderError {
        code,
        kind,
        message,
        request_id,
    })
}

/// Decodes a streaming SSE event from OpenAI Chat Completions, emitting StreamEvents.
pub(crate) fn decode_stream_event(
    model: &crate::catalog::Model,
    sse_event: &SseEvent,
    builder: &mut ResponseBuilder,
) -> Result<Vec<StreamEvent>, AiError> {
    // Count provider frames before decoding. Compatibility candidates can be
    // intentionally withheld and would otherwise evade the canonical event
    // cap until EOF.
    builder.observe_provider_stream_event()?;

    if sse_event.data == "[DONE]" {
        let mut events = Vec::new();
        // Resolve a content-based Qwen tool call before closing the response.
        // The marker and its body are commonly split over many SSE chunks.
        flush_qwen_xml_pending(&mut events, builder)?;
        if builder.tool_output_locked_seen
            && builder.qwen_xml_call_count == 0
            && !builder.native_tool_call_seen
        {
            builder.set_stop_reason(StopReason::Other("tool_output_locked".to_string()));
        }
        // Providers that omit a finish_reason chunk entirely leave parts open
        // here; close them so the terminal response stays balanced (§8).
        close_open_parts(&mut events, builder)?;
        // Record fallback provenance without changing finalization or treating
        // absent usage as evidence of zero provider billing.
        if builder.stop_reason.is_none() {
            builder.add_diagnostic(defaulted_stop_reason_diagnostic());
        }
        if builder.usage.is_none() {
            builder.add_diagnostic(crate::Diagnostic {
                code: "chat_usage_missing".to_owned(),
                message: "Chat completion supplied no usage; billing is not known to be zero"
                    .to_owned(),
            });
        }
        let resp = builder.finish_mut()?;
        events.push(StreamEvent::Finished(resp));
        return Ok(events);
    }

    let chunk = decode_chat_chunk(&sse_event.data)?;

    let mut events = Vec::new();
    if !chunk.id.is_empty()
        && builder
            .response_id
            .as_ref()
            .is_some_and(|expected| !expected.is_empty() && expected != &chunk.id)
    {
        builder.server_timing.reject_identity();
    }
    // Only terminal-bearing or trailing usage-only frames qualify. An earlier
    // snapshot cannot be promoted to an authoritative completed server rate.
    let terminal = chunk
        .choices
        .iter()
        .any(|choice| choice.finish_reason.is_some())
        || (chunk.choices.is_empty() && (chunk.usage.is_some() || chunk.x_groq.usage.is_some()));
    super::response::observe_server_timing(
        &mut builder.server_timing,
        &chunk.timings,
        &chunk.time_info,
        &chunk.x_groq,
        chunk.usage.as_ref(),
        terminal,
    );

    if !builder.started {
        // A chunk with an empty/absent `id` still starts the stream; the
        // provider-assigned id is then simply unknown (None).
        let response_id = (!chunk.id.is_empty()).then_some(chunk.id.clone());
        emit_event(&mut events, builder, StreamEvent::Started { response_id })?;
    } else if builder.response_id.is_none() && !chunk.id.is_empty() {
        // Lifecycle feedback may have seeded a synthetic `Started` before the
        // first provider chunk. Retain the eventual provider id in the final
        // response without emitting a duplicate start event.
        builder.response_id = Some(chunk.id.clone());
    }

    for choice in chunk.choices {
        // Delta text and structured Mistral thinking content.
        if let Some(ref wire_content) = choice.delta.content {
            for fragment in content_fragments(wire_content) {
                match fragment {
                    ChatContentFragment::Text(text) if !text.is_empty() => {
                        if model.spec.capabilities.tools {
                            // vLLM can return native XML/JSON tool syntax as ordinary
                            // content. Defer recognized calls until turn completion so
                            // a later structured `tool_calls` delta can supersede them.
                            consume_qwen_xml_content(&mut events, builder, &text)?;
                        } else {
                            emit_text_without_locked_marker(&mut events, builder, &text)?;
                        }
                    }
                    ChatContentFragment::Reasoning(text) if !text.is_empty() => {
                        emit_reasoning_delta(&mut events, builder, text)?;
                    }
                    ChatContentFragment::Text(_) | ChatContentFragment::Reasoning(_) => {}
                }
            }
        }

        // Delta reasoning content. OpenRouter and similar gateways send
        // `reasoning` where DeepSeek/Moonshot send `reasoning_content`;
        // prefer the documented field when a chunk carries both.
        let reasoning_text = choice
            .delta
            .reasoning_content
            .as_ref()
            .filter(|text| !text.is_empty())
            .or_else(|| {
                choice
                    .delta
                    .reasoning
                    .as_ref()
                    .filter(|text| !text.is_empty())
            });
        if let Some(reasoning) = reasoning_text {
            emit_reasoning_delta(&mut events, builder, reasoning.clone())?;
        }

        // Delta tool calls
        if let Some(ref tcs) = choice.delta.tool_calls {
            if !tcs.is_empty() {
                builder.native_tool_call_seen = true;
            }
            for tc in tcs {
                let key = format!("tool_{}", tc.index);
                let idx = segment_index(builder, &key);

                let id_key = format!("tool_id_{}", tc.index);
                let name_key = format!("tool_name_{}", tc.index);
                let args_key = format!("tool_args_{}", tc.index);
                let custom_key = format!("tool_custom_{}", tc.index);
                if tc.function.is_some() && tc.custom.is_some() {
                    return Err(DecodeError::InvalidProviderField(
                        "tool call has both function and custom data".to_owned(),
                    )
                    .into());
                }
                if tc.custom.is_some() {
                    if builder.tool_call_builders.contains_key(&idx)
                        && !crate::protocol::grammar::is_open(builder, idx)
                    {
                        return Err(DecodeError::InvalidProviderField(
                            "function call changed to a custom call".to_owned(),
                        )
                        .into());
                    }
                    builder.replace_temp_buffer(custom_key.clone(), String::new())?;
                }
                if tc.function.is_some()
                    && (builder.temp_buffers.contains_key(&custom_key)
                        || crate::protocol::grammar::is_open(builder, idx))
                {
                    return Err(DecodeError::InvalidProviderField(
                        "custom call changed to a function call".to_owned(),
                    )
                    .into());
                }
                let name = tc
                    .function
                    .as_ref()
                    .and_then(|function| function.name.as_ref())
                    .or_else(|| tc.custom.as_ref().and_then(|custom| custom.name.as_ref()));
                let args = tc
                    .function
                    .as_ref()
                    .and_then(|function| function.arguments.as_ref())
                    .or_else(|| tc.custom.as_ref().and_then(|custom| custom.input.as_ref()));
                if !builder.tool_call_builders.contains_key(&idx) {
                    if let Some(id) = &tc.id {
                        builder.replace_temp_buffer(id_key.clone(), id.clone())?;
                    }
                    if let Some(name) = name {
                        builder.replace_temp_buffer(name_key.clone(), name.clone())?;
                    }
                }
                if let Some(args) = args {
                    builder.append_temp_buffer_bounded(
                        args_key.clone(),
                        args,
                        MAX_TOOL_ARGUMENT_BYTES,
                    )?;
                }
                if !builder.tool_call_builders.contains_key(&idx)
                    && builder.temp_buffers.contains_key(&id_key)
                    && builder.temp_buffers.contains_key(&name_key)
                {
                    let id = builder
                        .take_temp_buffer(&id_key)
                        .expect("presence checked above");
                    let name = builder
                        .take_temp_buffer(&name_key)
                        .expect("presence checked above");
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallStart {
                            async_execution: false,
                            index: idx,
                            id: ToolCallId(id),
                            name,
                        },
                    )?;
                    if builder.take_temp_buffer(&custom_key).is_some() {
                        crate::protocol::grammar::start(builder, idx)?;
                    }
                }
                if builder.tool_call_builders.contains_key(&idx) {
                    builder.take_temp_buffer(&custom_key);
                    if let Some(args) = builder.take_temp_buffer(&args_key) {
                        if crate::protocol::grammar::is_open(builder, idx) {
                            crate::protocol::grammar::delta(&mut events, builder, idx, &args)?;
                        } else if !args.is_empty() {
                            emit_event(
                                &mut events,
                                builder,
                                StreamEvent::ToolCallArgsDelta {
                                    index: idx,
                                    delta: args,
                                },
                            )?;
                        }
                    }
                }
            }
        }

        // Finish reason. Several providers (Moonshot, OpenRouter passthrough)
        // repeat `finish_reason` in the trailing usage-bearing chunk; part
        // closing is idempotent so the duplicate close emits nothing (§8).
        if let Some(ref finish_reason) = choice.finish_reason {
            let stop_reason = if builder.qwen_xml_call_count > 0 && finish_reason == "stop" {
                // A Qwen XML fallback call is semantically tool use even though
                // the unconfigured vLLM endpoint labels the content turn stop.
                StopReason::ToolUse
            } else {
                map_stop_reason(finish_reason)
            };
            builder.set_stop_reason(stop_reason);
            flush_qwen_xml_pending(&mut events, builder)?;
            if builder.tool_output_locked_seen
                && builder.qwen_xml_call_count == 0
                && !builder.native_tool_call_seen
            {
                builder.set_stop_reason(StopReason::Other("tool_output_locked".to_string()));
            }
            close_open_parts(&mut events, builder)?;
        }
    }

    if let Some(ref usage) = chunk.usage {
        let u = map_usage(usage)?;
        emit_event(&mut events, builder, StreamEvent::Usage(u))?;
    }

    Ok(events)
}

pub(super) fn get_canonical_index(builder: &mut ResponseBuilder, key: &str) -> usize {
    if let Some(&idx) = builder.provider_to_canonical_indices.get(key) {
        idx
    } else {
        let idx = builder.next_canonical_index;
        builder.next_canonical_index += 1;
        builder
            .provider_to_canonical_indices
            .insert(key.to_string(), idx);
        idx
    }
}

/// Canonical index for `key`'s current segment, reopening a fresh segment when
/// the provider kept streaming a part kind after closing it (deltas arriving
/// after a finish chunk). Reopening allocates from the monotonic counter, so
/// the fresh index can never collide with an existing part.
pub(super) fn segment_index(builder: &mut ResponseBuilder, key: &str) -> usize {
    let idx = get_canonical_index(builder, key);
    if builder.ended_indices.contains(&idx) {
        builder.provider_to_canonical_indices.remove(key);
        get_canonical_index(builder, key)
    } else {
        idx
    }
}

/// Emits the `*End` event for every part that is still open, exactly once, in
/// canonical index order. Shared by the `finish_reason` path and the terminal
/// `[DONE]` path so both tolerate providers that duplicate or omit the finish
/// chunk.
fn close_open_parts(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
) -> Result<(), AiError> {
    // A tool-call key whose id/name never arrived cannot form a ToolCall.
    let incomplete_tool = builder
        .provider_to_canonical_indices
        .iter()
        .filter(|(key, _)| key.starts_with("tool_"))
        .any(|(_, index)| !builder.tool_call_builders.contains_key(index));
    if incomplete_tool {
        return Err(AiError::Decode(DecodeError::InvalidProviderField(
            "tool call ended before id and name were received".to_string(),
        )));
    }

    let mut open: Vec<usize> = builder
        .text_buffers
        .keys()
        .chain(builder.reasoning_text_buffers.keys())
        .chain(builder.tool_call_builders.keys())
        .filter(|idx| !builder.ended_indices.contains(idx))
        .cloned()
        .collect();
    open.sort_unstable();
    open.dedup();
    for idx in open {
        if builder.text_buffers.contains_key(&idx) {
            emit_event(events, builder, StreamEvent::TextEnd { index: idx })?;
        } else if builder.reasoning_text_buffers.contains_key(&idx) {
            emit_event(events, builder, StreamEvent::ReasoningEnd { index: idx })?;
        } else if crate::protocol::grammar::is_open(builder, idx) {
            crate::protocol::grammar::finish(events, builder, idx, None)?;
        } else if builder.tool_call_builders.contains_key(&idx) {
            emit_event(
                events,
                builder,
                StreamEvent::ToolCallEnd {
                    index: idx,
                    argument_error: None,
                },
            )?;
        }
    }
    Ok(())
}
