//! Amazon Bedrock Converse/ConverseStream private wire codec.
//!
//! Bedrock streams AWS Event Stream binary frames rather than SSE. The framing
//! decoder below is deliberately incremental, CRC-checked, and bounded before a
//! payload is handed to JSON decoding.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use serde_json::{json, Map, Value};

use crate::error::{AiError, DecodeError, ProviderError};
use crate::protocol::{
    emit_event, get_canonical_index, normalize_tool_call_id, Base64Bytes, HttpRequestParts,
};
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{
    AssistantPart, ImageSource, Media, Message, Protocol, Request, StopReason, ToolCallId,
    ToolChoice, ToolResultPart, Usage, UserPart,
};
use crate::validate::{normalize_request_reasoning, validate_request};

const MAX_EVENT_STREAM_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_STREAM_BUFFER_BYTES: usize = MAX_EVENT_STREAM_FRAME_BYTES + 12;

/// Incremental AWS Event Stream frame decoder used by the HTTP client.
pub(crate) struct BedrockEventStreamDecoder {
    buffer: Vec<u8>,
}

impl BedrockEventStreamDecoder {
    /// Creates an empty frame decoder.
    pub(crate) fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
        }
    }

    /// Feeds one network chunk and returns all complete Event Stream messages.
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
    ) -> Result<Vec<BedrockEventStreamMessage>, DecodeError> {
        if self
            .buffer
            .len()
            .checked_add(chunk.len())
            .is_none_or(|size| size > MAX_EVENT_STREAM_BUFFER_BYTES)
        {
            return Err(DecodeError::BodyTooLarge);
        }
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            if self.buffer.len() < 12 {
                break;
            }
            let total_length = read_u32(&self.buffer[..4])? as usize;
            let headers_length = read_u32(&self.buffer[4..8])? as usize;
            if !(16..=MAX_EVENT_STREAM_FRAME_BYTES).contains(&total_length)
                || headers_length > total_length.saturating_sub(16)
            {
                return Err(invalid_frame());
            }
            if crc32(&self.buffer[..8]) != read_u32(&self.buffer[8..12])? {
                return Err(invalid_frame());
            }
            if self.buffer.len() < total_length {
                break;
            }
            if crc32(&self.buffer[..total_length - 4])
                != read_u32(&self.buffer[total_length - 4..total_length])?
            {
                return Err(invalid_frame());
            }
            let header_end = 12 + headers_length;
            let headers = parse_event_headers(&self.buffer[12..header_end])?;
            let payload = bytes::Bytes::copy_from_slice(&self.buffer[header_end..total_length - 4]);
            self.buffer.drain(..total_length);
            messages.push(BedrockEventStreamMessage { headers, payload });
        }
        Ok(messages)
    }

    /// Verifies that the body did not end halfway through an Event Stream frame.
    pub(crate) fn finish(self) -> Result<(), DecodeError> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(invalid_frame())
        }
    }
}

/// One decoded AWS Event Stream message.
pub(crate) struct BedrockEventStreamMessage {
    headers: BTreeMap<String, String>,
    payload: bytes::Bytes,
}

/// Stateful terminal handling for one ConverseStream response.
#[derive(Default)]
pub(crate) struct BedrockStreamState {
    message_stopped: bool,
    finished: bool,
}

/// Builds an Amazon Bedrock ConverseStream HTTP request.
pub(crate) fn build_request(
    model: &crate::catalog::Model,
    request: &Request,
) -> Result<HttpRequestParts, AiError> {
    let request = normalize_request_reasoning(request, &model.spec.capabilities);
    let diagnostics = validate_request(
        &request,
        &model.spec.capabilities,
        &model.spec.limits,
        Protocol::BedrockConverse,
        &model.spec.id,
        request.compatibility,
    )?;

    let mut messages = Vec::new();
    let mut pending_tool_uses = BTreeSet::new();
    let mut synthetic_tool_results = BTreeSet::new();
    for message in &request.messages {
        match message {
            Message::User(user) => {
                let mut content = Vec::new();
                for part in &user.content {
                    match part {
                        UserPart::Text(text) => content.push(json!({"text": text})),
                        UserPart::Media(Media::Image(image)) => {
                            if !model
                                .spec
                                .capabilities
                                .input_modalities
                                .contains(crate::types::Modality::Image)
                            {
                                continue;
                            }
                            let (ImageSource::Inline(bytes), Some(media_type)) =
                                (&image.source, &image.media_type)
                            else {
                                continue;
                            };
                            let Some(format) = bedrock_image_format(media_type.as_ref()) else {
                                continue;
                            };
                            content.push(json!({
                                "image": {
                                    "format": format,
                                    "source": {"bytes": Base64Bytes::from(bytes)},
                                }
                            }));
                        }
                        UserPart::Media(Media::Audio(_)) => {}
                        UserPart::ToolResult(result) => {
                            let tool_use_id = normalize_tool_call_id(&result.tool_call_id.0);
                            if synthetic_tool_results.contains(&tool_use_id) {
                                continue;
                            }
                            pending_tool_uses.remove(&tool_use_id);
                            let result_text = result
                                .content
                                .iter()
                                .filter_map(|part| match part {
                                    ToolResultPart::Text(text) => Some(json!({"text": text})),
                                    ToolResultPart::Media(_) => None,
                                })
                                .collect::<Vec<_>>();
                            let result_content = if result_text.is_empty() {
                                vec![json!({"text": ""})]
                            } else {
                                result_text
                            };
                            content.push(json!({
                                "toolResult": {
                                    "toolUseId": tool_use_id,
                                    "content": result_content,
                                    "status": if result.is_error { "error" } else { "success" },
                                }
                            }));
                        }
                    }
                }
                push_message(&mut messages, "user", content);
            }
            Message::Assistant(assistant) => {
                if request.compatibility == crate::CompatibilityMode::Lossy {
                    push_synthetic_tool_results(
                        &mut messages,
                        &mut pending_tool_uses,
                        &mut synthetic_tool_results,
                    );
                }
                let mut content = Vec::new();
                for part in &assistant.content {
                    match part {
                        AssistantPart::Text(text) => content.push(json!({"text": text})),
                        AssistantPart::ToolCall(call) => {
                            let tool_use_id = normalize_tool_call_id(&call.id.0);
                            let input = serde_json::from_str::<Value>(&call.arguments_json)
                                .map_err(|error| {
                                    AiError::Decode(DecodeError::Json(error.to_string()))
                                })?;
                            pending_tool_uses.insert(tool_use_id.clone());
                            content.push(json!({
                                "toolUse": {
                                    "toolUseId": tool_use_id,
                                    "name": call.name,
                                    "input": input,
                                }
                            }));
                        }
                        AssistantPart::Reasoning(reasoning) => {
                            if let Some(state) = reasoning.state.as_ref().filter(|state| {
                                state.protocol == Protocol::BedrockConverse
                                    && state.model == model.spec.id
                            }) {
                                let value = match &state.kind {
                                    crate::types::ReasoningStateKind::AnthropicSignature {
                                        signature,
                                    } if !signature.is_empty() => Some(
                                        json!({"reasoningText": {"text": reasoning.text.as_deref().unwrap_or(""), "signature": signature}}),
                                    ),
                                    crate::types::ReasoningStateKind::AnthropicRedacted {
                                        data,
                                    } => {
                                        base64::engine::general_purpose::STANDARD
                                            .decode(data)
                                            .map_err(|_| {
                                                invalid_provider_field(
                                                    "invalid redacted reasoning state",
                                                )
                                            })?;
                                        Some(json!({"redactedContent": data}))
                                    }
                                    _ => None,
                                };
                                if let Some(value) = value {
                                    content.push(json!({"reasoningContent": value}));
                                }
                            }
                        }
                        AssistantPart::Media(_) | AssistantPart::ProviderMetadata(_) => {}
                    }
                }
                push_message(&mut messages, "assistant", content);
            }
        }
    }
    if request.compatibility == crate::CompatibilityMode::Lossy {
        push_synthetic_tool_results(
            &mut messages,
            &mut pending_tool_uses,
            &mut synthetic_tool_results,
        );
    }

    let mut body = Map::new();
    body.insert(
        "messages".to_owned(),
        serde_json::to_value(messages)
            .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?,
    );
    if let Some(system) = &request.system {
        body.insert("system".to_owned(), json!([{"text": system}]));
    }
    let mut inference = Map::new();
    inference.insert(
        "maxTokens".to_owned(),
        json!(request
            .max_output_tokens
            .unwrap_or(model.spec.limits.max_output_tokens)),
    );
    if let Some(temperature) = request.temperature {
        inference.insert("temperature".to_owned(), json!(temperature));
    }
    if !request.stop.is_empty() {
        inference.insert("stopSequences".to_owned(), json!(request.stop));
    }
    body.insert("inferenceConfig".to_owned(), Value::Object(inference));
    if let Some(cap) = &model.spec.capabilities.reasoning {
        use crate::types::ReasoningConfig;
        let budget = match &request.reasoning {
            ReasoningConfig::Budget(budget) => Some(*budget),
            ReasoningConfig::Effort(effort) => cap.budget(*effort),
            _ => None,
        };
        let thinking = budget.map_or_else(
            || json!({"type": "disabled"}),
            |budget| json!({"type": "enabled", "budget_tokens": budget}),
        );
        body.insert(
            "additionalModelRequestFields".to_owned(),
            json!({"thinking": thinking}),
        );
    }

    if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "toolSpec": {
                        "name": tool.name,
                        "description": tool.description,
                        "inputSchema": {"json": tool.parameters},
                    }
                })
            })
            .collect::<Vec<_>>();
        let tool_choice = match &request.tool_choice {
            ToolChoice::Auto => json!({"auto": {}}),
            ToolChoice::Required => json!({"any": {}}),
            ToolChoice::Named(name) => json!({"tool": {"name": name}}),
            ToolChoice::None => unreachable!("ToolChoice::None omits Bedrock toolConfig"),
        };
        body.insert(
            "toolConfig".to_owned(),
            json!({"tools": tools, "toolChoice": tool_choice}),
        );
    }

    let mut url = model.endpoint.base_url.clone();
    {
        let mut segments = url.path_segments_mut().map_err(|_| {
            AiError::Decode(DecodeError::InvalidProviderField(
                "invalid Bedrock endpoint URL".to_owned(),
            ))
        })?;
        segments.pop_if_empty();
        segments.push("model");
        segments.push(&model.spec.api_name);
        segments.push("converse-stream");
    }
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/vnd.amazon.eventstream"),
    );
    headers.insert(
        http::HeaderName::from_static("x-amzn-bedrock-accept"),
        http::HeaderValue::from_static("application/json"),
    );
    Ok(HttpRequestParts {
        url,
        headers,
        body: bytes::Bytes::from(
            serde_json::to_vec(&Value::Object(body))
                .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))?,
        ),
        streaming: true,
        diagnostics,
    })
}

/// Decodes one Event Stream message into canonical events.
pub(crate) fn decode_stream_event(
    _model: &crate::catalog::Model,
    message: &BedrockEventStreamMessage,
    builder: &mut ResponseBuilder,
    state: &mut BedrockStreamState,
) -> Result<Vec<StreamEvent>, AiError> {
    builder.observe_provider_stream_event()?;
    let message_type = message.headers.get(":message-type").map(String::as_str);
    if message_type == Some("exception") {
        return Err(bedrock_exception(message));
    }
    let Some(event_type) = message.headers.get(":event-type").map(String::as_str) else {
        return Ok(Vec::new());
    };
    let payload = parse_payload(&message.payload)?;
    let mut events = Vec::new();
    match event_type {
        "messageStart" => emit_event(
            &mut events,
            builder,
            StreamEvent::Started { response_id: None },
        )?,
        "contentBlockStart" => {
            let index = content_block_index(&payload)?;
            let canonical = get_canonical_index(builder, &format!("block_{index}"));
            if let Some(tool_use) = payload.get("start").and_then(|start| start.get("toolUse")) {
                let id = required_string(tool_use, "toolUseId")?;
                let name = required_string(tool_use, "name")?;
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallStart {
                        index: canonical,
                        id: ToolCallId(id),
                        name,
                    },
                )?;
            }
        }
        "contentBlockDelta" => {
            let index = content_block_index(&payload)?;
            let canonical = get_canonical_index(builder, &format!("block_{index}"));
            let delta = payload
                .get("delta")
                .ok_or_else(|| invalid_provider_field("contentBlockDelta.delta"))?;
            if let Some(reasoning) = delta.get("reasoningContent") {
                let object = reasoning
                    .as_object()
                    .ok_or_else(|| invalid_provider_field("reasoningContent union"))?;
                if object.len() != 1
                    || !object
                        .keys()
                        .all(|k| matches!(k.as_str(), "text" | "signature" | "redactedContent"))
                    || delta.as_object().is_none_or(|d| d.len() != 1)
                {
                    return Err(invalid_provider_field("reasoningContent union"));
                }
                if builder.text_buffers.contains_key(&canonical)
                    || builder.tool_call_builders.contains_key(&canonical)
                    || builder.ended_indices.contains(&canonical)
                {
                    return Err(invalid_provider_field("reasoningContent block kind"));
                }
                let redacted_key = format!("bedrock_redacted_{index}");
                let signature_key = format!("bedrock_signature_{index}");
                let already_reasoning = builder.reasoning_text_buffers.contains_key(&canonical);
                if !already_reasoning {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ReasoningStart { index: canonical },
                    )?;
                }
                if let Some(chunk) = object.get("redactedContent") {
                    if (already_reasoning && !builder.temp_buffers.contains_key(&redacted_key))
                        || builder.temp_buffers.contains_key(&signature_key)
                    {
                        return Err(invalid_provider_field("mixed redacted/signed reasoning"));
                    }
                    let chunk = chunk
                        .as_str()
                        .ok_or_else(|| invalid_provider_field("redactedContent bytes"))?;
                    base64::engine::general_purpose::STANDARD
                        .decode(chunk)
                        .map_err(|_| invalid_provider_field("redactedContent base64"))?;
                    // Each AWS blob delta is separately base64 encoded. Keep
                    // boundaries until a single bounded decode/concatenate at stop.
                    builder.append_temp_buffer(redacted_key.clone(), chunk)?;
                    builder.append_temp_buffer(redacted_key, "\n")?;
                } else {
                    if builder.temp_buffers.contains_key(&redacted_key) {
                        return Err(invalid_provider_field("mixed redacted/signed reasoning"));
                    }
                    if let Some(text) = object.get("text") {
                        let text = text
                            .as_str()
                            .ok_or_else(|| invalid_provider_field("reasoningContent text"))?;
                        emit_event(
                            &mut events,
                            builder,
                            StreamEvent::ReasoningDelta {
                                index: canonical,
                                delta: text.to_owned(),
                            },
                        )?;
                    } else {
                        let signature = object
                            .get("signature")
                            .and_then(Value::as_str)
                            .ok_or_else(|| invalid_provider_field("reasoningContent signature"))?;
                        builder.append_temp_buffer(signature_key, signature)?;
                    }
                }
            } else if let Some(text) = delta.get("text").and_then(Value::as_str) {
                if !builder.text_buffers.contains_key(&canonical) {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::TextStart { index: canonical },
                    )?;
                }
                if !text.is_empty() {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::TextDelta {
                            index: canonical,
                            delta: text.to_owned(),
                        },
                    )?;
                }
            } else if let Some(input) = delta
                .get("toolUse")
                .and_then(|tool_use| tool_use.get("input"))
                .and_then(Value::as_str)
            {
                if !builder.tool_call_builders.contains_key(&canonical) {
                    return Err(invalid_provider_field(
                        "toolUse delta without toolUse start",
                    ));
                }
                if !input.is_empty() {
                    emit_event(
                        &mut events,
                        builder,
                        StreamEvent::ToolCallArgsDelta {
                            index: canonical,
                            delta: input.to_owned(),
                        },
                    )?;
                }
            }
        }
        "contentBlockStop" => {
            let index = content_block_index(&payload)?;
            let canonical = get_canonical_index(builder, &format!("block_{index}"));
            if builder.text_buffers.contains_key(&canonical)
                && !builder.ended_indices.contains(&canonical)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextEnd { index: canonical },
                )?;
            } else if builder.reasoning_text_buffers.contains_key(&canonical)
                && !builder.ended_indices.contains(&canonical)
            {
                let redacted_key = format!("bedrock_redacted_{index}");
                let kind = if let Some(chunks) = builder.take_temp_buffer(&redacted_key) {
                    let mut bytes = Vec::new();
                    for chunk in chunks.lines() {
                        base64::engine::general_purpose::STANDARD
                            .decode_vec(chunk, &mut bytes)
                            .map_err(|_| invalid_provider_field("redactedContent base64"))?;
                    }
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                    builder.replace_temp_buffer(redacted_key.clone(), data)?;
                    crate::types::ReasoningStateKind::AnthropicRedacted {
                        data: builder
                            .take_temp_buffer(&redacted_key)
                            .expect("inserted redacted buffer"),
                    }
                } else {
                    let signature = builder
                        .take_temp_buffer(&format!("bedrock_signature_{index}"))
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| invalid_provider_field("missing reasoning signature"))?;
                    crate::types::ReasoningStateKind::AnthropicSignature { signature }
                };
                builder.set_reasoning_state(
                    canonical,
                    crate::types::ReasoningState {
                        protocol: Protocol::BedrockConverse,
                        model: builder.model.clone(),
                        kind,
                    },
                )?;
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningEnd { index: canonical },
                )?;
            } else if builder.tool_call_builders.contains_key(&canonical)
                && !builder.ended_indices.contains(&canonical)
            {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallEnd {
                        index: canonical,
                        argument_error: None,
                    },
                )?;
            }
        }
        "messageStop" => {
            let stop = payload
                .get("stopReason")
                .and_then(Value::as_str)
                .map(map_stop_reason)
                .unwrap_or(StopReason::EndTurn);
            builder.set_stop_reason(stop);
            state.message_stopped = true;
        }
        "metadata" => {
            if let Some(usage) = payload.get("usage") {
                let usage = map_usage(usage)?;
                emit_event(&mut events, builder, StreamEvent::Usage(usage))?;
            }
            if state.message_stopped {
                finish_stream(builder, state, &mut events)?;
            }
        }
        _ => {}
    }
    Ok(events)
}

/// Emits the terminal event when an otherwise valid Bedrock stream reaches EOF.
pub(crate) fn finish_stream(
    builder: &mut ResponseBuilder,
    state: &mut BedrockStreamState,
    events: &mut Vec<StreamEvent>,
) -> Result<(), AiError> {
    if state.message_stopped && !state.finished {
        let response = builder.finish_mut()?;
        emit_event(events, builder, StreamEvent::Finished(response))?;
        state.finished = true;
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct BedrockMessage {
    role: &'static str,
    content: Vec<Value>,
}

fn push_message(messages: &mut Vec<BedrockMessage>, role: &'static str, content: Vec<Value>) {
    if content.is_empty() {
        return;
    }
    if let Some(previous) = messages.last_mut().filter(|message| message.role == role) {
        previous.content.extend(content);
    } else {
        messages.push(BedrockMessage { role, content });
    }
}

fn push_synthetic_tool_results(
    messages: &mut Vec<BedrockMessage>,
    pending: &mut BTreeSet<String>,
    synthetic: &mut BTreeSet<String>,
) {
    let content = std::mem::take(pending)
        .into_iter()
        .map(|tool_use_id| {
            synthetic.insert(tool_use_id.clone());
            json!({
                "toolResult": {
                    "toolUseId": tool_use_id,
                    "content": [{"text": "Tool execution result was not supplied by the caller."}],
                    "status": "error",
                }
            })
        })
        .collect::<Vec<_>>();
    push_message(messages, "user", content);
}

fn bedrock_image_format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/jpeg" => Some("jpeg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

fn parse_payload(payload: &[u8]) -> Result<Value, AiError> {
    serde_json::from_slice(payload)
        .map_err(|error| AiError::Decode(DecodeError::Json(error.to_string())))
}

fn bedrock_exception(message: &BedrockEventStreamMessage) -> AiError {
    let code = message
        .headers
        .get(":exception-type")
        .cloned()
        .or_else(|| message.headers.get(":event-type").cloned());
    let text = serde_json::from_slice::<Value>(&message.payload)
        .ok()
        .and_then(|payload| {
            payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Bedrock stream returned an exception".to_owned());
    AiError::Provider(ProviderError {
        code,
        kind: Some("bedrock_event_stream".to_owned()),
        message: text,
        request_id: None,
    })
}

fn content_block_index(payload: &Value) -> Result<usize, AiError> {
    let index = payload
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_provider_field("contentBlockIndex"))?;
    usize::try_from(index).map_err(|_| invalid_provider_field("contentBlockIndex"))
}

fn required_string(value: &Value, field: &str) -> Result<String, AiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_provider_field(field))
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "content_filtered" | "guardrail_intervened" => StopReason::Refusal,
        other => StopReason::Other(other.to_owned()),
    }
}

fn map_usage(value: &Value) -> Result<Usage, AiError> {
    let input_tokens = value
        .get("inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = value
        .get("outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let minimum_total = input_tokens
        .checked_add(output_tokens)
        .ok_or_else(|| AiError::Decode(DecodeError::UsageUnderflow))?;
    let total_tokens = value
        .get("totalTokens")
        .and_then(Value::as_u64)
        .unwrap_or(minimum_total);
    if total_tokens < minimum_total {
        return Err(AiError::Decode(DecodeError::UsageUnderflow));
    }
    Ok(Usage {
        input_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        output_tokens,
        reasoning_tokens: 0,
        total_tokens,
    })
}

fn invalid_provider_field(field: impl Into<String>) -> AiError {
    AiError::Decode(DecodeError::InvalidProviderField(field.into()))
}

fn invalid_frame() -> DecodeError {
    DecodeError::InvalidProviderField("invalid AWS Event Stream frame".to_owned())
}

fn read_u32(bytes: &[u8]) -> Result<u32, DecodeError> {
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| invalid_frame())?;
    Ok(u32::from_be_bytes(bytes))
}

fn parse_event_headers(bytes: &[u8]) -> Result<BTreeMap<String, String>, DecodeError> {
    let mut offset = 0usize;
    let mut headers = BTreeMap::new();
    while offset < bytes.len() {
        let name_length = usize::from(*bytes.get(offset).ok_or_else(invalid_frame)?);
        offset = offset.checked_add(1).ok_or_else(invalid_frame)?;
        let name_end = offset.checked_add(name_length).ok_or_else(invalid_frame)?;
        let name = std::str::from_utf8(bytes.get(offset..name_end).ok_or_else(invalid_frame)?)
            .map_err(|_| invalid_frame())?;
        offset = name_end;
        let value_type = *bytes.get(offset).ok_or_else(invalid_frame)?;
        offset = offset.checked_add(1).ok_or_else(invalid_frame)?;
        let value = match value_type {
            // AWS Event Stream string header.
            7 => {
                let length = read_u16(bytes.get(offset..offset + 2).ok_or_else(invalid_frame)?)?;
                offset = offset.checked_add(2).ok_or_else(invalid_frame)?;
                let end = offset
                    .checked_add(usize::from(length))
                    .ok_or_else(invalid_frame)?;
                let value = std::str::from_utf8(bytes.get(offset..end).ok_or_else(invalid_frame)?)
                    .map_err(|_| invalid_frame())?
                    .to_owned();
                offset = end;
                Some(value)
            }
            // true/false, byte, int16, int32, int64, timestamp, UUID.
            0 | 1 => None,
            2 => {
                offset = offset.checked_add(1).ok_or_else(invalid_frame)?;
                None
            }
            3 => {
                offset = offset.checked_add(2).ok_or_else(invalid_frame)?;
                None
            }
            4 => {
                offset = offset.checked_add(4).ok_or_else(invalid_frame)?;
                None
            }
            5 | 8 => {
                offset = offset.checked_add(8).ok_or_else(invalid_frame)?;
                None
            }
            6 => {
                let length = read_u16(bytes.get(offset..offset + 2).ok_or_else(invalid_frame)?)?;
                offset = offset
                    .checked_add(2 + usize::from(length))
                    .ok_or_else(invalid_frame)?;
                None
            }
            9 => {
                offset = offset.checked_add(16).ok_or_else(invalid_frame)?;
                None
            }
            _ => return Err(invalid_frame()),
        };
        if offset > bytes.len() {
            return Err(invalid_frame());
        }
        if let Some(value) = value {
            headers.insert(name.to_owned(), value);
        }
    }
    Ok(headers)
}

fn read_u16(bytes: &[u8]) -> Result<u16, DecodeError> {
    let bytes: [u8; 2] = bytes.try_into().map_err(|_| invalid_frame())?;
    Ok(u16::from_be_bytes(bytes))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::harness;
    use crate::types::Request;
    use crate::CompatibilityMode;

    fn frame(headers: &[(&str, &str)], payload: &Value) -> Vec<u8> {
        let mut header_bytes = Vec::new();
        for (name, value) in headers {
            header_bytes.push(name.len() as u8);
            header_bytes.extend_from_slice(name.as_bytes());
            header_bytes.push(7);
            header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
            header_bytes.extend_from_slice(value.as_bytes());
        }
        let payload = serde_json::to_vec(payload).unwrap();
        let total = 16 + header_bytes.len() + payload.len();
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(&(total as u32).to_be_bytes());
        bytes.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
        bytes.extend_from_slice(&header_bytes);
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
        bytes
    }

    #[test]
    fn frame_decoder_handles_fragmented_converse_stream() {
        let bytes = [
            frame(
                &[(":message-type", "event"), (":event-type", "messageStart")],
                &json!({"role": "assistant"}),
            ),
            frame(
                &[
                    (":message-type", "event"),
                    (":event-type", "contentBlockDelta"),
                ],
                &json!({"contentBlockIndex": 0, "delta": {"text": "hello"}}),
            ),
            frame(
                &[
                    (":message-type", "event"),
                    (":event-type", "contentBlockStop"),
                ],
                &json!({"contentBlockIndex": 0}),
            ),
            frame(
                &[(":message-type", "event"), (":event-type", "messageStop")],
                &json!({"stopReason": "end_turn"}),
            ),
            frame(
                &[(":message-type", "event"), (":event-type", "metadata")],
                &json!({"usage": {"inputTokens": 3, "outputTokens": 2, "totalTokens": 5}}),
            ),
        ]
        .concat();
        let mut decoder = BedrockEventStreamDecoder::new();
        let mut messages = Vec::new();
        for chunk in bytes.chunks(7) {
            messages.extend(decoder.push(chunk).unwrap());
        }
        decoder.finish().unwrap();
        let model = harness::model(Protocol::BedrockConverse, None);
        let mut builder =
            ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
        let mut state = BedrockStreamState::default();
        let mut events = Vec::new();
        for message in messages {
            events.extend(decode_stream_event(&model, &message, &mut builder, &mut state).unwrap());
        }
        finish_stream(&mut builder, &mut state, &mut events).unwrap();
        assert!(matches!(events.first(), Some(StreamEvent::Started { .. })));
        assert!(events.iter().any(
            |event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "hello")
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::Usage(Usage {
                input_tokens: 3,
                output_tokens: 2,
                total_tokens: 5,
                ..
            })
        )));
        assert!(matches!(events.last(), Some(StreamEvent::Finished(_))));
    }

    #[test]
    fn frame_decoder_rejects_bad_crc() {
        let mut bytes = frame(
            &[(":message-type", "event"), (":event-type", "messageStart")],
            &json!({"role": "assistant"}),
        );
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(BedrockEventStreamDecoder::new().push(&bytes).is_err());
    }

    #[test]
    fn request_builder_uses_converse_shape() {
        let model = harness::model(Protocol::BedrockConverse, None);
        let request = Request {
            system: Some("be concise".to_owned()),
            messages: vec![Message::User(crate::types::UserMessage {
                content: vec![UserPart::Text("hello".to_owned())],
            })],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(64),
            temperature: None,
            stop: Vec::new(),
            reasoning: crate::types::ReasoningConfig::Off,
            reasoning_mode: crate::types::ReasoningMode::Standard,
            responses: None,
            output_format: crate::types::OutputFormat::Text,
            output_modalities: crate::types::OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: crate::types::CacheRetention::None,
            session_id: None,
        };
        let parts = build_request(&model, &request).unwrap();
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 64);
        assert!(parts
            .url
            .path()
            .ends_with("/model/fixture-api-name/converse-stream"));
    }

    fn thinking_model() -> crate::catalog::Model {
        let mut model = harness::model(Protocol::BedrockConverse, None);
        let spec = std::sync::Arc::make_mut(&mut model.spec);
        spec.capabilities.structured_output = false;
        spec.capabilities.reasoning = Some(crate::types::ReasoningCapability {
            options: Some(crate::types::ReasoningOptions {
                values: vec!["none".into(), "low".into(), "high".into()],
                default: Some("low".into()),
            }),
            control: crate::types::ReasoningControl::TokenBudget,
            exposes_text: true,
            preserves_state: true,
            effort_budgets: Some(crate::types::ReasoningEffortBudgets {
                minimal: 1024,
                low: 1536,
                medium: 2048,
                high: 3072,
                xhigh: 4096,
                max: 6144,
            }),
            openai_chat_mode: crate::types::OpenAiChatReasoningMode::Standard,
            min_effort: crate::types::ReasoningEffort::Low,
            max_effort: crate::types::ReasoningEffort::High,
        });
        model
    }

    fn thinking_request() -> Request {
        Request {
            system: None,
            messages: vec![Message::User(crate::types::UserMessage {
                content: vec![UserPart::Text("hello".into())],
            })],
            tools: vec![crate::types::ToolDef {
                name: "echo".into(),
                description: "fixture".into(),
                parameters: json!({"type": "object"}),
            }],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(4096),
            temperature: None,
            stop: Vec::new(),
            reasoning: crate::types::ReasoningConfig::Effort(crate::types::ReasoningEffort::Low),
            reasoning_mode: crate::types::ReasoningMode::Standard,
            responses: None,
            output_format: crate::types::OutputFormat::Text,
            output_modalities: crate::types::OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: crate::types::CacheRetention::None,
            session_id: None,
        }
    }

    fn reasoning_tool_stream(deltas: Vec<Value>) -> Vec<(&'static str, Value)> {
        let mut events = vec![("messageStart", json!({"role": "assistant"}))];
        events.extend(deltas.into_iter().map(|delta| {
            (
                "contentBlockDelta",
                json!({"contentBlockIndex": 0, "delta": {"reasoningContent": delta}}),
            )
        }));
        events.extend([
            ("contentBlockStop", json!({"contentBlockIndex": 0})),
            ("contentBlockStart", json!({"contentBlockIndex": 1, "start": {"toolUse": {"toolUseId": "call-1", "name": "echo"}}})),
            ("contentBlockDelta", json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{}"}}})),
            ("contentBlockStop", json!({"contentBlockIndex": 1})),
            ("messageStop", json!({"stopReason": "tool_use"})),
            ("metadata", json!({"usage": {"inputTokens": 3, "outputTokens": 2, "totalTokens": 5}})),
        ]);
        events
    }

    fn drive_thinking(
        events: &[(&str, Value)],
        chunk: usize,
    ) -> Result<crate::types::Response, AiError> {
        let model = thinking_model();
        let bytes = events
            .iter()
            .flat_map(|(kind, payload)| {
                frame(
                    &[(":message-type", "event"), (":event-type", kind)],
                    payload,
                )
            })
            .collect::<Vec<_>>();
        let mut decoder = BedrockEventStreamDecoder::new();
        let mut builder =
            ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
        let mut state = BedrockStreamState::default();
        let mut output = Vec::new();
        for bytes in bytes.chunks(chunk) {
            for message in decoder.push(bytes).map_err(AiError::Decode)? {
                output.extend(decode_stream_event(
                    &model,
                    &message,
                    &mut builder,
                    &mut state,
                )?);
            }
        }
        decoder.finish().map_err(AiError::Decode)?;
        finish_stream(&mut builder, &mut state, &mut output)?;
        output
            .into_iter()
            .find_map(|event| match event {
                StreamEvent::Finished(response) => Some(response),
                _ => None,
            })
            .ok_or_else(|| invalid_provider_field("fixture missing terminal event"))
    }

    fn continuation(response: crate::types::Response) -> Request {
        let mut request = thinking_request();
        request.messages.push(Message::Assistant(response.message));
        request
            .messages
            .push(Message::User(crate::types::UserMessage {
                content: vec![UserPart::ToolResult(crate::types::ToolResult {
                    tool_call_id: ToolCallId("call-1".into()),
                    content: vec![ToolResultPart::Text("actual supplied result".into())],
                    is_error: true,
                    added_tool_names: None,
                })],
            }));
        request
    }

    #[test]
    fn thinking_budgets_off_holes_and_answer_room_match_the_converse_wire() {
        let model = thinking_model();
        let mut request = thinking_request();
        let body: Value =
            serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"],
            json!({"type": "enabled", "budget_tokens": 1536})
        );
        assert!(body.get("thinking").is_none());
        assert!(body.get("anthropic_version").is_none());
        request.reasoning = crate::types::ReasoningConfig::Off;
        let body: Value =
            serde_json::from_slice(&build_request(&model, &request).unwrap().body).unwrap();
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"],
            json!({"type": "disabled"})
        );
        for budget in [0, 1023, 4096, 4097] {
            request.reasoning = crate::types::ReasoningConfig::Budget(budget);
            assert!(build_request(&model, &request).is_err());
        }
        request.reasoning = crate::types::ReasoningConfig::Budget(1024);
        assert!(build_request(&model, &request).is_ok());
        request.reasoning =
            crate::types::ReasoningConfig::Effort(crate::types::ReasoningEffort::Medium);
        assert!(build_request(&model, &request).is_err());
        let mut nonthinking = model.clone();
        std::sync::Arc::make_mut(&mut nonthinking.spec)
            .capabilities
            .reasoning = None;
        assert!(build_request(&nonthinking, &thinking_request()).is_err());
    }

    #[test]
    fn thinking_rejects_sampling_and_forced_tool_combinations() {
        let model = thinking_model();
        let mut request = thinking_request();
        request.temperature = Some(0.7);
        assert!(build_request(&model, &request).is_err());
        request.temperature = Some(1.0);
        assert!(build_request(&model, &request).is_ok());
        for choice in [ToolChoice::Required, ToolChoice::Named("echo".into())] {
            request.tool_choice = choice;
            assert!(build_request(&model, &request).is_err());
        }
    }

    #[test]
    fn signed_thinking_fragments_and_supplied_tool_result_replay_without_changes() {
        let events = reasoning_tool_stream(vec![
            json!({"text": "think"}),
            json!({"text": "ing"}),
            json!({"signature": "SIGN-A"}),
            json!({"signature": "-B"}),
        ]);
        for chunk in [1, 2, 7, 31, usize::MAX] {
            let response = drive_thinking(&events, chunk).unwrap();
            assert_eq!(response.stop_reason, StopReason::ToolUse);
            assert!(!format!("{response:?}").contains("SIGN-A"));
            let request = continuation(response);
            let body: Value =
                serde_json::from_slice(&build_request(&thinking_model(), &request).unwrap().body)
                    .unwrap();
            assert_eq!(
                body["messages"][1]["content"][0],
                json!({"reasoningContent": {"reasoningText": {"text": "thinking", "signature": "SIGN-A-B"}}})
            );
            assert_eq!(
                body["messages"][1]["content"][1]["toolUse"]["toolUseId"],
                "call-1"
            );
            assert_eq!(
                body["messages"][2]["content"][0],
                json!({"toolResult": {"toolUseId": "call-1", "content": [{"text": "actual supplied result"}], "status": "error"}})
            );
        }
    }

    #[test]
    fn redacted_chunks_are_decoded_separately_then_reencoded_as_one_blob() {
        let events = reasoning_tool_stream(vec![
            json!({"redactedContent": "AQ=="}),
            json!({"redactedContent": "AgM="}),
        ]);
        for chunk in [1, 7, 31] {
            let request = continuation(drive_thinking(&events, chunk).unwrap());
            let body: Value =
                serde_json::from_slice(&build_request(&thinking_model(), &request).unwrap().body)
                    .unwrap();
            assert_eq!(
                body["messages"][1]["content"][0],
                json!({"reasoningContent": {"redactedContent": "AQID"}})
            );
        }
    }

    #[test]
    fn malformed_or_unsigned_reasoning_blocks_fail_before_a_finished_response() {
        for deltas in [
            vec![json!({"text": "missing signature"})],
            vec![json!({"redactedContent": "not base64!"})],
            vec![json!({"text": "mixed union", "signature": "signature"})],
            vec![
                json!({"text": "mixed blocks"}),
                json!({"redactedContent": "AQ=="}),
            ],
            vec![
                json!({"redactedContent": "AQ=="}),
                json!({"signature": "mixed"}),
            ],
            vec![json!({"signature": 7})],
        ] {
            assert!(drive_thinking(&reasoning_tool_stream(deltas), 7).is_err());
        }
    }

    #[test]
    fn incompatible_reasoning_is_rejected_or_dropped_without_plaintext_downgrade() {
        let events = reasoning_tool_stream(vec![
            json!({"text": "private reasoning"}),
            json!({"signature": "signature"}),
        ]);
        let mut request = continuation(drive_thinking(&events, 7).unwrap());
        let Message::Assistant(assistant) = &mut request.messages[1] else {
            panic!()
        };
        let AssistantPart::Reasoning(reasoning) = &mut assistant.content[0] else {
            panic!()
        };
        reasoning.state.as_mut().unwrap().model = crate::types::ModelId("different-model".into());
        assert!(build_request(&thinking_model(), &request).is_err());
        request.compatibility = CompatibilityMode::Lossy;
        let parts = build_request(&thinking_model(), &request).unwrap();
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        assert!(!body.to_string().contains("private reasoning"));
        assert!(!body.to_string().contains("reasoningContent"));
        assert!(parts
            .diagnostics
            .iter()
            .any(|d| d.code == "dropped_reasoning_state"));
    }

    #[test]
    fn signature_buffer_uses_the_existing_aggregate_response_limit() {
        let model = thinking_model();
        let mut builder =
            ResponseBuilder::new(model.spec.id.clone(), Protocol::BedrockConverse, None);
        builder
            .reserve_buffered_content(crate::stream::MAX_RESPONSE_CONTENT_BYTES)
            .unwrap();
        let message = BedrockEventStreamMessage {
            headers: [(":message-type".into(), "event".into()), (":event-type".into(), "contentBlockDelta".into())].into(),
            payload: serde_json::to_vec(&json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"signature": "one more byte"}}})).unwrap().into(),
        };
        assert!(matches!(
            decode_stream_event(
                &model,
                &message,
                &mut builder,
                &mut BedrockStreamState::default()
            ),
            Err(AiError::Decode(DecodeError::ResponseTooLarge))
        ));
    }
}
