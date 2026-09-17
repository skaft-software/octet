//! Native `pi-messages` wire codec (Radius gateway protocol).
//!
//! Upstream reference: `packages/ai/src/api/pi-messages.ts` in the read-only Pi
//! checkout. This is *not* an OpenAI alias: the request is a single
//! `POST <base>/messages` carrying `{ model, context, options }` (Pi's own
//! `Context` document), and the response is an SSE stream of serialized
//! assistant-message events (`start`, `text_*`, `thinking_*`, `toolcall_*`)
//! terminated by exactly one `done` or `error` event carrying usage.
//!
//! Canonical mapping decisions:
//!
//! * Streaming tool arguments are a **preview**. The terminal `toolcall_end`
//!   payload is authoritative, exactly like Pi (`Object.assign(...,
//!   event.toolCall)`), so the codec replaces the accumulated fragments with
//!   the terminal call and then closes the call. A provider that changes the
//!   call's id/name mid-stream is rejected instead.
//! * Terminal `text_end`/`thinking_end` payloads must agree with the delivered
//!   deltas: a terminal payload that is not a prefix extension of the streamed
//!   text is a decode error, never a fabricated splice.
//! * Provider prose is never echoed: the in-band `error` event becomes a typed,
//!   fixed-prose failure. `rewrite` and `providerThinkingLevel` terminal
//!   metadata are recorded as bounded diagnostics.
//! * Reasoning continuation signatures are retained as opaque
//!   [`ReasoningStateKind`] values so the same route can replay them; the
//!   `protocol`/`model` identity on the state is this route's own.

use serde_json::{json, Map, Value};

use crate::catalog::Model;
use crate::error::{
    AiError, ConfigError, DecodeError, Diagnostic, ProviderError, StreamProtocolError,
    UnsupportedError,
};
use crate::protocol::sse::SseEvent;
use crate::protocol::HttpRequestParts;
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{
    CacheRetention, CompatibilityMode, Media, Message, OutputFormat, OutputModalities, ReasoningConfig,
    ReasoningMode, ReasoningState, ReasoningStateKind, Request, StopReason, ToolCallId,
    ToolChoice, ToolResultPart, Usage, UserPart,
};

/// Upper bound for a provider response identifier copied into the response.
const MAX_RESPONSE_ID_BYTES: usize = 512;
/// Upper bound for one opaque reasoning/continuation signature.
const MAX_SIGNATURE_BYTES: usize = 64 * 1024;
/// Upper bound for a provider thinking-level label.
const MAX_THINKING_LEVEL_BYTES: usize = 128;
/// Upper bound for a rewrite policy identifier copied into a diagnostic.
const MAX_REWRITE_POLICY_BYTES: usize = 256;

fn malformed(detail: &str) -> AiError {
    DecodeError::InvalidProviderField(format!("Pi-messages: {detail}")).into()
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, AiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("missing or invalid string field"))
}

fn optional_string<'a>(
    value: &'a Value,
    field: &str,
    max_bytes: usize,
) -> Result<Option<&'a str>, AiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= max_bytes => Ok(Some(text)),
        Some(_) => Err(malformed("invalid or oversized optional string field")),
    }
}

fn index(value: &Value, field: &str) -> Result<usize, AiError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value <= u64::from(u32::MAX))
        .map(|value| value as usize)
        .ok_or_else(|| malformed("missing or invalid content index"))
}

fn optional_bool(value: &Value, field: &str) -> Result<bool, AiError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(malformed("invalid boolean field")),
    }
}

/// Report an optional-but-unsupported provider field: fail closed in Strict,
/// record one bounded diagnostic in Lossy.
fn unsupported_field(
    builder: &mut ResponseBuilder,
    error: UnsupportedError,
    code: &str,
) -> Result<(), AiError> {
    if builder.compatibility == CompatibilityMode::Strict {
        return Err(error.into());
    }
    if !builder
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
    {
        builder.add_diagnostic(Diagnostic {
            code: code.to_owned(),
            message: format!("Pi-messages: {error}; omitted in Lossy mode"),
        });
    }
    Ok(())
}

// The wire `context` document -------------------------------------------------

fn usage_placeholder() -> Value {
    json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0},
    })
}

fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn image_content(media: &crate::types::ImageMedia) -> Result<Value, AiError> {
    match &media.source {
        crate::types::ImageSource::Inline(data) => {
            let media_type = media
                .media_type
                .as_ref()
                .ok_or_else(|| malformed("inline image has no declared media type"))?;
            Ok(json!({
                "type": "image",
                "data": base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &data[..],
                ),
                "mimeType": media_type.as_ref(),
            }))
        }
        // Pi's `ImageContent` carries inline bytes only. A URL or a
        // provider-hosted reference would have to be fetched by the caller
        // (a network side effect this codec must not perform) or echoed as an
        // opaque id the backend cannot resolve.
        crate::types::ImageSource::Url(_) | crate::types::ImageSource::ProviderRef(_) => {
            Err(UnsupportedError::Image.into())
        }
    }
}

fn assistant_part_value(
    part: &crate::types::AssistantPart,
    diagnostics: &mut Vec<Diagnostic>,
    lossy: bool,
) -> Result<Option<Value>, AiError> {
    let mut drop_unsupported = |error: UnsupportedError, code: &str| -> Result<(), AiError> {
        if lossy {
            if !diagnostics.iter().any(|diagnostic| diagnostic.code == code) {
                diagnostics.push(Diagnostic {
                    code: code.to_owned(),
                    message: format!("Pi-messages: {error}; omitted in Lossy mode"),
                });
            }
            Ok(())
        } else {
            Err(error.into())
        }
    };
    match part {
        crate::types::AssistantPart::Text(text) => Ok(Some(json!({"type": "text", "text": text}))),
        crate::types::AssistantPart::Reasoning(reasoning) => {
            let signature = match &reasoning.state {
                Some(ReasoningState {
                    kind: ReasoningStateKind::AnthropicSignature { signature },
                    ..
                }) => Some(signature.clone()),
                Some(ReasoningState {
                    kind: ReasoningStateKind::AnthropicRedacted { data },
                    ..
                }) => Some(data.clone()),
                Some(ReasoningState {
                    kind: ReasoningStateKind::OpenAiReasoning { .. },
                    ..
                }) => {
                    drop_unsupported(UnsupportedError::Reasoning, "dropped_reasoning_state")?;
                    None
                }
                None => None,
            };
            let mut value = json!({"type": "thinking", "thinking": reasoning.text.clone().unwrap_or_default()});
            if let Some(signature) = signature {
                value["thinkingSignature"] = json!(signature);
            }
            if matches!(
                reasoning.state,
                Some(ReasoningState {
                    kind: ReasoningStateKind::AnthropicRedacted { .. },
                    ..
                })
            ) {
                value["redacted"] = json!(true);
            }
            Ok(Some(value))
        }
        crate::types::AssistantPart::ToolCall(call) => {
            let arguments: Value = serde_json::from_str(&call.arguments_json)
                .map_err(|_| malformed("replayed tool call has non-JSON arguments"))?;
            if !arguments.is_object() {
                return Err(malformed("replayed tool call arguments must be an object"));
            }
            Ok(Some(json!({"type": "toolCall", "id": call.id.0, "name": call.name, "arguments": arguments})))
        }
        crate::types::AssistantPart::Media(_) => {
            drop_unsupported(UnsupportedError::AudioOutput, "dropped_assistant_media")?;
            Ok(None)
        }
        crate::types::AssistantPart::ProviderMetadata(_) => {
            drop_unsupported(
                UnsupportedError::Reasoning,
                "dropped_provider_metadata",
            )?;
            Ok(None)
        }
    }
}

fn tool_result_name(messages: &[Message], id: &str) -> Option<String> {
    messages.iter().find_map(|message| match message {
        Message::Assistant(assistant) => assistant.content.iter().find_map(|part| match part {
            crate::types::AssistantPart::ToolCall(call) if call.id.0 == id => Some(call.name.clone()),
            _ => None,
        }),
        Message::User(_) => None,
    })
}

fn encode_context(
    req: &Request,
    provider: &str,
    lossy: bool,
) -> Result<(Vec<Value>, Vec<Diagnostic>), AiError> {
    let mut messages = Vec::new();
    let mut diagnostics = Vec::new();
    let timestamp = timestamp_ms();
    for message in &req.messages {
        match message {
            Message::User(user) => {
                let mut content: Vec<Value> = Vec::new();
                for part in &user.content {
                    match part {
                        UserPart::Text(text) => content.push(json!({"type": "text", "text": text})),
                        UserPart::Media(Media::Image(image)) => {
                            content.push(image_content(image)?);
                        }
                        UserPart::Media(Media::Audio(_)) => {
                            if lossy {
                                diagnostics.push(Diagnostic {
                                    code: "dropped_audio".to_owned(),
                                    message: "Pi-messages: audio input is unsupported; omitted in Lossy mode".to_owned(),
                                });
                            } else {
                                return Err(UnsupportedError::Audio.into());
                            }
                        }
                        UserPart::ToolResult(result) => {
                            if !content.is_empty() {
                                messages.push(json!({"role": "user", "content": content, "timestamp": timestamp}));
                                content = Vec::new();
                            }
                            let mut blocks: Vec<Value> = Vec::new();
                            for block in &result.content {
                                match block {
                                    ToolResultPart::Text(text) => {
                                        blocks.push(json!({"type": "text", "text": text}));
                                    }
                                    ToolResultPart::Media(Media::Image(image)) => {
                                        blocks.push(image_content(image)?);
                                    }
                                    ToolResultPart::Media(Media::Audio(_)) => {
                                        if lossy {
                                            diagnostics.push(Diagnostic {
                                                code: "dropped_tool_result_media".to_owned(),
                                                message: "Pi-messages: tool-result media is unsupported; omitted in Lossy mode".to_owned(),
                                            });
                                        } else {
                                            return Err(UnsupportedError::ToolResultMedia.into());
                                        }
                                    }
                                }
                            }
                            let name = tool_result_name(&req.messages, &result.tool_call_id.0)
                                .unwrap_or_default();
                            let mut value = json!({
                                "role": "toolResult",
                                "toolCallId": result.tool_call_id.0,
                                "toolName": name,
                                "content": blocks,
                                "isError": result.is_error,
                                "timestamp": timestamp,
                            });
                            if let Some(added) = &result.added_tool_names {
                                if !added.is_empty() {
                                    value["addedToolNames"] = json!(added);
                                }
                            }
                            messages.push(value);
                        }
                    }
                }
                if !content.is_empty() {
                    messages.push(json!({"role": "user", "content": content, "timestamp": timestamp}));
                }
            }
            Message::Assistant(assistant) => {
                let mut content = Vec::new();
                let mut has_tool_call = false;
                for part in &assistant.content {
                    if matches!(part, crate::types::AssistantPart::ToolCall(_)) {
                        has_tool_call = true;
                    }
                    if let Some(value) = assistant_part_value(part, &mut diagnostics, lossy)? {
                        content.push(value);
                    }
                }
                messages.push(json!({
                    "role": "assistant",
                    "content": content,
                    "api": "pi-messages",
                    "provider": provider,
                    "model": assistant.model.0,
                    "usage": usage_placeholder(),
                    "stopReason": if has_tool_call { "toolUse" } else { "stop" },
                    "timestamp": timestamp,
                }));
            }
        }
    }
    Ok((messages, diagnostics))
}

fn tool_choice_value(choice: &ToolChoice) -> Result<Value, AiError> {
    Ok(match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named(name) => json!({"type": "function", "function": {"name": name}}),
    })
}

fn pi_thinking_level(effort: crate::types::ReasoningEffort) -> &'static str {
    use crate::types::ReasoningEffort::{High, Low, Max, Medium, Minimal, Ultra, Xhigh};
    match effort {
        Minimal => "minimal",
        Low => "low",
        Medium => "medium",
        High => "high",
        Xhigh => "xhigh",
        Max => "max",
        // Pi's ThinkingLevel has no `ultra` tier; the request normalizer
        // already clamps Ultra to a model's advertised maximum for live routes.
        Ultra => "max",
    }
}

/// Build the single `POST <base>/messages` request.
///
/// The caller-visible canonical request is never re-shaped to look like an
/// OpenAI Chat body. Fields this wire cannot express (`stop`, JSON output
/// formats, audio output, token-budget reasoning) fail closed in Strict mode
/// and are dropped with a diagnostic in Lossy mode.
pub(crate) fn build_request(model: &Model, req: &Request) -> Result<HttpRequestParts, AiError> {
    // Credentials for this route are presented as a bearer token. Like the
    // other native codecs, TLS is required except for literal loopback HTTP.
    let base = &model.endpoint.base_url;
    let loopback = match base.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if (base.scheme() != "https" && !(base.scheme() == "http" && loopback))
        || base.host().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.fragment().is_some()
    {
        return Err(ConfigError::InvalidBaseUrl(
            "pi-messages requires HTTPS (or literal loopback HTTP) without userinfo or fragment".into(),
        )
        .into());
    }
    if req.reasoning_mode != ReasoningMode::Standard {
        return Err(UnsupportedError::ReasoningMode.into());
    }
    let lossy = req.compatibility == CompatibilityMode::Lossy;
    let mut diagnostics = crate::validate::validate_request(
        req,
        &model.spec.capabilities,
        &model.spec.limits,
        model.spec.protocol,
        &model.spec.id,
        req.compatibility,
    )?;
    let drop = |diagnostics: &mut Vec<Diagnostic>, error: UnsupportedError, code: &str| -> Result<(), AiError> {
        if lossy {
            if !diagnostics.iter().any(|diagnostic| diagnostic.code == code) {
                diagnostics.push(Diagnostic {
                    code: code.to_owned(),
                    message: format!("Pi-messages: {error}; omitted in Lossy mode"),
                });
            }
            Ok(())
        } else {
            Err(error.into())
        }
    };
    if !req.stop.is_empty() {
        drop(&mut diagnostics, UnsupportedError::StopSequences, "dropped_stop_sequences")?;
    }
    if !matches!(req.output_format, OutputFormat::Text) {
        drop(
            &mut diagnostics,
            UnsupportedError::StructuredOutput,
            "dropped_structured_output",
        )?;
    }
    if req.output_modalities != OutputModalities::Text {
        drop(
            &mut diagnostics,
            UnsupportedError::AudioOutput,
            "dropped_audio_output",
        )?;
    }
    let reasoning = match &req.reasoning {
        ReasoningConfig::Off => None,
        ReasoningConfig::On => Some("high".to_owned()),
        ReasoningConfig::Effort(effort) => Some(pi_thinking_level(*effort).to_owned()),
        ReasoningConfig::Budget(_) => {
            drop(&mut diagnostics, UnsupportedError::Reasoning, "dropped_reasoning_budget")?;
            None
        }
    };
    let (messages, mut context_diagnostics) = encode_context(req, &model.endpoint.id.0, lossy)?;
    diagnostics.append(&mut context_diagnostics);
    let mut context = Map::new();
    if let Some(system) = req.system.as_deref().filter(|system| !system.is_empty()) {
        context.insert("systemPrompt".to_owned(), json!(system));
    }
    context.insert("messages".to_owned(), Value::Array(messages));
    if !req.tools.is_empty() && model.spec.capabilities.tools {
        let mut tools = Vec::with_capacity(req.tools.len());
        for tool in &req.tools {
            let mut value = Map::new();
            value.insert("name".to_owned(), json!(tool.name));
            value.insert("description".to_owned(), json!(tool.description));
            value.insert("parameters".to_owned(), tool.parameters.clone());
            if let Some(constrained) = &tool.constrained_sampling {
                value.insert("constrainedSampling".to_owned(), json!(constrained));
            }
            tools.push(Value::Object(value));
        }
        context.insert("tools".to_owned(), Value::Array(tools));
    }
    let mut options = Map::new();
    if let Some(temperature) = req.temperature {
        options.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(max_tokens) = crate::effective_output_token_cap(model, req.max_output_tokens) {
        options.insert("maxTokens".to_owned(), json!(max_tokens));
    }
    if let Some(reasoning) = reasoning {
        options.insert("reasoning".to_owned(), json!(reasoning));
    }
    match req.cache_retention {
        CacheRetention::None => {}
        CacheRetention::Short => {
            options.insert("cacheRetention".to_owned(), json!("short"));
        }
        CacheRetention::Long => {
            options.insert("cacheRetention".to_owned(), json!("long"));
        }
    }
    if let Some(session_id) = crate::protocol::cache_session_id(req) {
        options.insert("sessionId".to_owned(), json!(session_id));
    }
    if model.spec.capabilities.tools {
        options.insert("toolChoice".to_owned(), tool_choice_value(&req.tool_choice)?);
    }
    let body = json!({
        "model": model.spec.api_name,
        "context": Value::Object(context),
        "options": Value::Object(options),
    });
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("text/event-stream"),
    );
    Ok(HttpRequestParts {
        url: crate::protocol::endpoint_url(&model.endpoint.base_url, "messages")?,
        headers,
        body: serde_json::to_vec(&body)
            .map_err(|_| DecodeError::Json("cannot encode pi-messages request".into()))?
            .into(),
        streaming: true,
        diagnostics,
    })
}

// The decoded SSE stream ------------------------------------------------------

fn key(content_index: usize) -> String {
    format!("pi_messages:{content_index}")
}

fn canonical_index(builder: &mut ResponseBuilder, content_index: usize) -> usize {
    crate::protocol::get_canonical_index(builder, &key(content_index))
}

fn existing_index(builder: &ResponseBuilder, content_index: usize) -> Result<usize, AiError> {
    builder
        .provider_to_canonical_indices
        .get(&key(content_index))
        .copied()
        .ok_or_else(|| malformed("content delta arrived before its block start"))
}

fn ensure_started(builder: &mut ResponseBuilder, events: &mut Vec<StreamEvent>) -> Result<(), AiError> {
    if !builder.started {
        crate::protocol::emit_event(
            events,
            builder,
            StreamEvent::Started { response_id: None },
        )?;
    }
    Ok(())
}

fn decode_usage(value: &Value) -> Result<Usage, AiError> {
    let bucket = |field: &str| -> Result<u64, AiError> {
        value
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed("missing or invalid usage bucket"))
    };
    Ok(Usage {
        input_tokens: bucket("input")?,
        cache_read_tokens: bucket("cacheRead")?,
        cache_write_tokens: bucket("cacheWrite")?,
        cache_write_1h_tokens: value
            .get("cacheWrite1h")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: bucket("output")?,
        reasoning_tokens: value.get("reasoning").and_then(Value::as_u64).unwrap_or(0),
        total_tokens: bucket("totalTokens")?,
    })
}

fn record_terminal_metadata(
    builder: &mut ResponseBuilder,
    value: &Value,
) -> Result<(), AiError> {
    if let Some(level) = optional_string(value, "providerThinkingLevel", MAX_THINKING_LEVEL_BYTES)? {
        if !level.is_empty() {
            builder.add_diagnostic(Diagnostic {
                code: "pi_messages_provider_thinking_level".to_owned(),
                message: format!("Pi-messages: provider thinking level {level}"),
            });
        }
    }
    if let Some(rewrite) = value.get("rewrite").filter(|value| !value.is_null()) {
        let policy = optional_string(rewrite, "policyId", MAX_REWRITE_POLICY_BYTES)?
            .unwrap_or_default();
        let version = rewrite.get("policyVersion").and_then(Value::as_i64).unwrap_or(0);
        let changed = optional_bool(rewrite, "changed")?;
        let token_change = rewrite.get("tokenCountChange").and_then(Value::as_i64).unwrap_or(0);
        let message_change = rewrite.get("messageCountChange").and_then(Value::as_i64).unwrap_or(0);
        let system_changed = optional_bool(rewrite, "systemPromptChanged")?;
        builder.add_diagnostic(Diagnostic {
            code: "pi_messages_rewrite".to_owned(),
            message: format!(
                "Pi-messages: server rewrite policy {policy} v{version} changed={changed} tokens={token_change} messages={message_change} system={system_changed}"
            ),
        });
    }
    Ok(())
}

fn set_response_id(builder: &mut ResponseBuilder, value: &Value) -> Result<(), AiError> {
    if let Some(id) = optional_string(value, "responseId", MAX_RESPONSE_ID_BYTES)? {
        if !id.is_empty() && !id.chars().any(char::is_control) {
            builder.response_id = Some(id.to_owned());
        }
    }
    Ok(())
}

fn close_text(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    index: usize,
    content: &str,
) -> Result<(), AiError> {
    let assembled = builder.text_buffers.get(&index).cloned().unwrap_or_default();
    if builder.ended_indices.contains(&index) {
        return if assembled == content {
            Ok(())
        } else {
            Err(malformed("text changed after it was closed"))
        };
    }
    let suffix = content
        .strip_prefix(assembled.as_str())
        .ok_or_else(|| malformed("terminal text disagrees with the streamed deltas"))?;
    if !suffix.is_empty() {
        crate::protocol::emit_event(
            events,
            builder,
            StreamEvent::TextDelta {
                index,
                delta: suffix.to_owned(),
            },
        )?;
    }
    crate::protocol::emit_event(events, builder, StreamEvent::TextEnd { index })
}

fn close_reasoning(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
    index: usize,
    content: &str,
) -> Result<(), AiError> {
    let assembled = builder
        .reasoning_text_buffers
        .get(&index)
        .cloned()
        .unwrap_or_default();
    if builder.ended_indices.contains(&index) {
        return if assembled == content {
            Ok(())
        } else {
            Err(malformed("thinking changed after it was closed"))
        };
    }
    let suffix = content
        .strip_prefix(assembled.as_str())
        .ok_or_else(|| malformed("terminal thinking disagrees with the streamed deltas"))?;
    if !suffix.is_empty() {
        crate::protocol::emit_event(
            events,
            builder,
            StreamEvent::ReasoningDelta {
                index,
                delta: suffix.to_owned(),
            },
        )?;
    }
    crate::protocol::emit_event(events, builder, StreamEvent::ReasoningEnd { index })
}

fn close_open_parts(
    events: &mut Vec<StreamEvent>,
    builder: &mut ResponseBuilder,
) -> Result<(), AiError> {
    let mut indices: Vec<usize> = builder.observed_indices.iter().copied().collect();
    indices.sort_unstable();
    for index in indices {
        if builder.ended_indices.contains(&index) {
            continue;
        }
        if builder.text_buffers.contains_key(&index) {
            crate::protocol::emit_event(events, builder, StreamEvent::TextEnd { index })?;
        } else if builder.reasoning_text_buffers.contains_key(&index) {
            crate::protocol::emit_event(events, builder, StreamEvent::ReasoningEnd { index })?;
        } else if builder.tool_call_builders.contains_key(&index) {
            // A native terminal can only close a call whose arguments the
            // provider already completed; otherwise the provider truncated the
            // turn and the call must not be exposed as executable.
            let completed = builder
                .tool_call_builders
                .get(&index)
                .is_some_and(|call| {
                    serde_json::from_str::<Value>(&call.arguments_json)
                        .is_ok_and(|value| value.is_object())
                });
            if !completed {
                return Err(malformed("terminal arrived before tool arguments completed"));
            }
            crate::protocol::emit_event(
                events,
                builder,
                StreamEvent::ToolCallEnd {
                    index,
                    argument_error: None,
                },
            )?;
        }
    }
    Ok(())
}

/// Decode one `data:` payload of the pi-messages SSE stream.
pub(crate) fn decode_stream_event(
    model: &Model,
    event: &SseEvent,
    builder: &mut ResponseBuilder,
) -> Result<Vec<StreamEvent>, AiError> {
    use crate::protocol::emit_event;

    // Upstream's parser skips frames without a `data:` line and the `[DONE]`
    // sentinel; neither is a provider event.
    let data = event.data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(Vec::new());
    }
    builder.observe_provider_stream_event()?;
    let value: Value = serde_json::from_str(data).map_err(|_| malformed("invalid event JSON"))?;
    let kind = string(&value, "type")?.to_owned();
    let mut events = Vec::new();
    match kind.as_str() {
        "start" => {
            if builder.started {
                return Err(StreamProtocolError::DuplicateStart.into());
            }
            emit_event(&mut events, builder, StreamEvent::Started { response_id: None })?;
        }
        "text_start" => {
            ensure_started(builder, &mut events)?;
            let canonical = canonical_index(builder, index(&value, "contentIndex")?);
            if builder.text_buffers.contains_key(&canonical)
                || builder.reasoning_text_buffers.contains_key(&canonical)
                || builder.tool_call_builders.contains_key(&canonical)
            {
                return Err(malformed("duplicate content index"));
            }
            emit_event(&mut events, builder, StreamEvent::TextStart { index: canonical })?;
        }
        "text_delta" => {
            ensure_started(builder, &mut events)?;
            let canonical = existing_index(builder, index(&value, "contentIndex")?)?;
            let delta = string(&value, "delta")?;
            if !builder.text_buffers.contains_key(&canonical)
                || builder.ended_indices.contains(&canonical)
            {
                return Err(malformed("text delta outside an open text block"));
            }
            if !delta.is_empty() {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::TextDelta {
                        index: canonical,
                        delta: delta.to_owned(),
                    },
                )?;
            }
        }
        "text_end" => {
            ensure_started(builder, &mut events)?;
            let canonical = existing_index(builder, index(&value, "contentIndex")?)?;
            if !builder.text_buffers.contains_key(&canonical) {
                return Err(malformed("text end outside a text block"));
            }
            if optional_string(&value, "contentSignature", MAX_SIGNATURE_BYTES)?
                .is_some_and(|signature| !signature.is_empty())
            {
                unsupported_field(
                    builder,
                    UnsupportedError::Reasoning,
                    "dropped_text_signature",
                )?;
            }
            close_text(&mut events, builder, canonical, string(&value, "content")?)?;
        }
        "thinking_start" => {
            ensure_started(builder, &mut events)?;
            let canonical = canonical_index(builder, index(&value, "contentIndex")?);
            if builder.text_buffers.contains_key(&canonical)
                || builder.reasoning_text_buffers.contains_key(&canonical)
                || builder.tool_call_builders.contains_key(&canonical)
            {
                return Err(malformed("duplicate content index"));
            }
            emit_event(
                &mut events,
                builder,
                StreamEvent::ReasoningStart { index: canonical },
            )?;
        }
        "thinking_delta" => {
            ensure_started(builder, &mut events)?;
            let canonical = existing_index(builder, index(&value, "contentIndex")?)?;
            let delta = string(&value, "delta")?;
            if !builder.reasoning_text_buffers.contains_key(&canonical)
                || builder.ended_indices.contains(&canonical)
            {
                return Err(malformed("thinking delta outside an open thinking block"));
            }
            if !delta.is_empty() {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ReasoningDelta {
                        index: canonical,
                        delta: delta.to_owned(),
                    },
                )?;
            }
        }
        "thinking_end" => {
            ensure_started(builder, &mut events)?;
            let canonical = existing_index(builder, index(&value, "contentIndex")?)?;
            if !builder.reasoning_text_buffers.contains_key(&canonical) {
                return Err(malformed("thinking end outside a thinking block"));
            }
            let signature = optional_string(&value, "contentSignature", MAX_SIGNATURE_BYTES)?
                .filter(|signature| !signature.is_empty());
            let redacted = optional_bool(&value, "redacted")?;
            close_reasoning(&mut events, builder, canonical, string(&value, "content")?)?;
            if let Some(signature) = signature {
                // The opaque continuation payload is retained verbatim so the
                // same route can replay it; the stated protocol/model identity
                // is this route's own, never another provider's.
                let kind = if redacted {
                    ReasoningStateKind::AnthropicRedacted {
                        data: signature.to_owned(),
                    }
                } else {
                    ReasoningStateKind::AnthropicSignature {
                        signature: signature.to_owned(),
                    }
                };
                builder.set_reasoning_state(
                    canonical,
                    ReasoningState {
                        protocol: model.spec.protocol,
                        model: model.spec.id.clone(),
                        kind,
                    },
                )?;
            } else if redacted {
                unsupported_field(builder, UnsupportedError::Reasoning, "dropped_redacted_thinking")?;
            }
        }
        "toolcall_start" => {
            ensure_started(builder, &mut events)?;
            let canonical = canonical_index(builder, index(&value, "contentIndex")?);
            if builder.text_buffers.contains_key(&canonical)
                || builder.reasoning_text_buffers.contains_key(&canonical)
                || builder.tool_call_builders.contains_key(&canonical)
            {
                return Err(malformed("duplicate content index"));
            }
            let id = string(&value, "id")?;
            let name = string(&value, "toolName")?;
            if id.is_empty()
                || id.len() > 512
                || id.chars().any(char::is_control)
                || name.trim().is_empty()
                || name.len() > 512
                || name.chars().any(char::is_control)
            {
                return Err(malformed("invalid tool call identity"));
            }
            emit_event(
                &mut events,
                builder,
                StreamEvent::ToolCallStart {
                    index: canonical,
                    id: ToolCallId(id.to_owned()),
                    name: name.to_owned(),
                },
            )?;
        }
        "toolcall_delta" => {
            ensure_started(builder, &mut events)?;
            let canonical = existing_index(builder, index(&value, "contentIndex")?)?;
            let delta = string(&value, "delta")?;
            if !builder.tool_call_builders.contains_key(&canonical)
                || builder.ended_indices.contains(&canonical)
            {
                return Err(malformed("tool argument delta outside an open call"));
            }
            if !delta.is_empty() {
                emit_event(
                    &mut events,
                    builder,
                    StreamEvent::ToolCallArgsDelta {
                        index: canonical,
                        delta: delta.to_owned(),
                    },
                )?;
            }
        }
        "toolcall_end" => {
            ensure_started(builder, &mut events)?;
            let canonical = existing_index(builder, index(&value, "contentIndex")?)?;
            if !builder.tool_call_builders.contains_key(&canonical)
                || builder.ended_indices.contains(&canonical)
            {
                return Err(malformed("tool call end outside an open call"));
            }
            let call = value
                .get("toolCall")
                .filter(|call| call.is_object())
                .ok_or_else(|| malformed("missing terminal tool call"))?;
            let id = string(call, "id")?;
            let name = string(call, "name")?;
            let arguments = call
                .get("arguments")
                .filter(|arguments| arguments.is_object())
                .ok_or_else(|| malformed("terminal tool call needs object arguments"))?;
            if optional_string(call, "thoughtSignature", MAX_SIGNATURE_BYTES)?
                .is_some_and(|signature| !signature.is_empty())
            {
                unsupported_field(builder, UnsupportedError::Reasoning, "dropped_tool_signature")?;
            }
            let started = &builder.tool_call_builders[&canonical];
            if started.id.0 != id || started.name != name {
                return Err(malformed("tool call identity changed before completion"));
            }
            let authoritative = serde_json::to_string(arguments)
                .map_err(|_| malformed("terminal tool arguments are not serializable"))?;
            // Pi replaces the streamed fragments with the terminal call. The
            // deltas were a preview (arguments are only canonical at closure),
            // and the replacement is the provider's own terminal value: no
            // fabricated or spliced arguments are introduced.
            builder
                .tool_call_builders
                .get_mut(&canonical)
                .expect("checked above")
                .arguments_json = authoritative;
            emit_event(
                &mut events,
                builder,
                StreamEvent::ToolCallEnd {
                    index: canonical,
                    argument_error: None,
                },
            )?;
        }
        "done" => {
            ensure_started(builder, &mut events)?;
            let reason = match string(&value, "reason")? {
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "toolUse" => StopReason::ToolUse,
                _ => return Err(malformed("invalid terminal reason")),
            };
            let usage = decode_usage(
                value
                    .get("usage")
                    .filter(|usage| usage.is_object())
                    .ok_or_else(|| malformed("missing terminal usage"))?,
            )?;
            set_response_id(builder, &value)?;
            record_terminal_metadata(builder, &value)?;
            close_open_parts(&mut events, builder)?;
            if reason == StopReason::ToolUse && builder.tool_call_builders.is_empty() {
                return Err(malformed("tool-use terminal without a tool call"));
            }
            builder.set_stop_reason(reason);
            emit_event(&mut events, builder, StreamEvent::Usage(usage))?;
            let response = builder.finish_mut()?;
            events.push(StreamEvent::Finished(response));
        }
        "error" => {
            ensure_started(builder, &mut events)?;
            let aborted = match string(&value, "reason")? {
                "aborted" => true,
                "error" => false,
                _ => return Err(malformed("invalid error reason")),
            };
            if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
                decode_usage(usage)?;
            }
            set_response_id(builder, &value)?;
            record_terminal_metadata(builder, &value)?;
            if aborted {
                return Err(AiError::Canceled);
            }
            // Provider prose (`errorMessage`) is deliberately not echoed.
            return Err(ProviderError {
                code: None,
                kind: Some("pi_messages_error".to_owned()),
                message: "Pi-messages response failed".to_owned(),
                request_id: None,
            }
            .into());
        }
        _ => return Err(malformed("unknown event type")),
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::protocol::harness::{drive, model as harness_model};
    use crate::types::{
        AssistantMessage, AssistantPart, ModelId, Protocol, ReasoningPart, ToolCall, ToolDef,
        UserMessage, UserPart,
    };

    fn fixture_model() -> crate::catalog::Model {
        harness_model(Protocol::OpenAiChat, None)
    }

    fn sse(value: serde_json::Value) -> String {
        format!("data: {value}\n\n")
    }

    fn request() -> Request {
        Request {
            system: Some("system-private".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: vec![UserPart::Text("hello".to_owned())],
            })],
            tools: tools(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(64),
            temperature: Some(0.5),
            stop: Vec::new(),
            reasoning: ReasoningConfig::Effort(crate::types::ReasoningEffort::High),
            reasoning_mode: ReasoningMode::Standard,
            responses: None,
            output_format: OutputFormat::Text,
            output_modalities: OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: CacheRetention::Short,
            session_id: Some("session-1".to_owned()),
        }
    }

    fn tools() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "lookup".to_owned(),
            description: "Look up a city.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
            constrained_sampling: None,
        }]
    }

    #[test]
    fn request_is_one_messages_post_with_a_native_context_document() {
        let model = fixture_model();
        let parts = build_request(&model, &request()).unwrap();
        assert!(parts.streaming);
        assert_eq!(parts.url.as_str(), "https://api.example.test/v1/messages");
        assert_eq!(parts.headers["accept"], "text/event-stream");
        assert_eq!(parts.headers["content-type"], "application/json");
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        assert_eq!(body["model"], "fixture-api-name");
        assert_eq!(body["context"]["systemPrompt"], "system-private");
        assert_eq!(body["context"]["messages"][0]["role"], "user");
        assert_eq!(
            body["context"]["messages"][0]["content"],
            json!([{"type": "text", "text": "hello"}])
        );
        assert!(body["context"]["messages"][0]["timestamp"].is_u64());
        assert_eq!(body["context"]["tools"][0]["name"], "lookup");
        assert_eq!(
            body["context"]["tools"][0]["parameters"]["properties"]["city"]["type"],
            "string"
        );
        assert_eq!(
            body["options"],
            json!({
                "temperature": 0.5,
                "maxTokens": 64,
                "reasoning": "high",
                "cacheRetention": "short",
                "sessionId": "session-1",
                "toolChoice": "auto",
            })
        );
        assert!(parts.diagnostics.is_empty());
    }

    #[test]
    fn strict_mode_rejects_wire_inexpressible_controls_and_lossy_drops_them() {
        let model = fixture_model();
        let mut strict = request();
        strict.stop = vec!["STOP".to_owned()];
        strict.output_format = OutputFormat::JsonObject;
        assert!(build_request(&model, &strict).is_err());

        let mut lossy = strict.clone();
        lossy.compatibility = CompatibilityMode::Lossy;
        let parts = build_request(&model, &lossy).unwrap();
        let codes: Vec<&str> = parts
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"dropped_stop_sequences"));
        assert!(codes.contains(&"dropped_structured_output"));

        let mut budget_model = fixture_model();
        Arc::make_mut(&mut budget_model.spec)
            .capabilities
            .reasoning
            .as_mut()
            .unwrap()
            .control = crate::types::ReasoningControl::TokenBudget;
        let mut budget = request();
        // The budget must be a *valid* size for the request (>= 1024 and below
        // the output allowance) so the codec's "no native budget field"
        // rejection is what this exercises, not the generic range validation.
        budget.max_output_tokens = Some(8192);
        budget.reasoning = ReasoningConfig::Budget(4096);
        assert!(build_request(&budget_model, &budget).is_err());
        budget.compatibility = CompatibilityMode::Lossy;
        let parts = build_request(&budget_model, &budget).unwrap();
        assert!(parts
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "dropped_reasoning_budget"));
        assert!(!String::from_utf8(parts.body.to_vec())
            .unwrap()
            .contains("\"reasoning\""));
    }

    #[test]
    fn base_url_must_be_tls_and_credential_free() {
        let mut model = fixture_model();
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse("http://api.example.test/v1/").unwrap();
        assert!(build_request(&model, &request()).is_err());
        let mut loopback = fixture_model();
        Arc::make_mut(&mut loopback.endpoint).base_url =
            url::Url::parse("http://127.0.0.1:8080/v1/").unwrap();
        assert!(build_request(&loopback, &request()).is_ok());
        let mut userinfo = fixture_model();
        Arc::make_mut(&mut userinfo.endpoint).base_url =
            url::Url::parse("https://user:secret@api.example.test/v1/").unwrap();
        assert!(build_request(&userinfo, &request()).is_err());
    }

    #[test]
    fn replayed_context_keeps_reasoning_signatures_and_tool_identity() {
        // The codec under test is the pi-messages route: a route-native
        // reasoning state (the codec stores the provider's opaque continuation
        // payload with its own protocol/model) must survive validation.
        let mut model = fixture_model();
        Arc::make_mut(&mut model.spec).protocol = Protocol::PiMessages;
        let mut req = request();
        req.reasoning = ReasoningConfig::Off;
        req.messages.push(Message::Assistant(AssistantMessage {
            content: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    text: Some("prior thinking".to_owned()),
                    state: Some(ReasoningState {
                        protocol: Protocol::PiMessages,
                        model: ModelId("fixture-model".to_owned()),
                        kind: ReasoningStateKind::AnthropicSignature {
                            signature: "opaque-signature".to_owned(),
                        },
                    }),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId("call_1".to_owned()),
                    name: "lookup".to_owned(),
                    arguments_json: r#"{"city":"Paris"}"#.to_owned(),
                    argument_error: None,
                }),
            ],
            model: ModelId("fixture-model".to_owned()),
            protocol: Protocol::PiMessages,
        }));
        req.messages.push(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(crate::types::ToolResult {
                tool_call_id: ToolCallId("call_1".to_owned()),
                content: vec![ToolResultPart::Text("found".to_owned())],
                is_error: false,
                added_tool_names: Some(vec!["extra".to_owned()]),
            })],
        }));
        let parts = build_request(&model, &req).unwrap();
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        let messages = body["context"]["messages"].as_array().unwrap();
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(
            messages[1]["content"][0],
            json!({"type": "thinking", "thinking": "prior thinking", "thinkingSignature": "opaque-signature"})
        );
        assert_eq!(
            messages[1]["content"][1],
            json!({"type": "toolCall", "id": "call_1", "name": "lookup", "arguments": {"city": "Paris"}})
        );
        assert_eq!(messages[1]["stopReason"], "toolUse");
        assert_eq!(messages[2]["role"], "toolResult");
        assert_eq!(messages[2]["toolCallId"], "call_1");
        assert_eq!(messages[2]["toolName"], "lookup");
        assert_eq!(messages[2]["content"][0], json!({"type": "text", "text": "found"}));
        assert_eq!(messages[2]["addedToolNames"], json!(["extra"]));
    }

    #[tokio::test]
    async fn text_stream_settles_with_terminal_usage_and_response_id() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "text_start", "contentIndex": 0}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "lo"}))
            + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}))
            + &sse(json!({
                "type": "done", "reason": "stop", "responseId": "resp_1",
                "providerThinkingLevel": "high",
                "usage": {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15}
            }));
        let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap();
        assert!(matches!(&events[0], StreamEvent::Started { response_id } if response_id.is_none()));
        assert!(matches!(&events[2], StreamEvent::TextDelta { delta, .. } if delta == "Hel"));
        let response = match events.last().unwrap() {
            StreamEvent::Finished(response) => response,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_eq!(response.response_id.as_deref(), Some("resp_1"));
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 5);
        assert_eq!(response.usage.total_tokens, 15);
        assert!(matches!(&response.message.content[..], [AssistantPart::Text(text)] if text == "Hello"));
        assert!(response
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "pi_messages_provider_thinking_level"));
    }

    #[tokio::test]
    async fn terminal_text_completes_missing_suffix_but_never_splices() {
        let model = fixture_model();
        let ok = sse(json!({"type": "start"}))
            + &sse(json!({"type": "text_start", "contentIndex": 0}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
            + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "Hello"}))
            + &sse(json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}));
        let events = drive(&model, decode_stream_event, ok.as_bytes(), 0)
            .await
            .unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { delta, .. } if delta == "lo")));

        let bad = sse(json!({"type": "start"}))
            + &sse(json!({"type": "text_start", "contentIndex": 0}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "Hel"}))
            + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "World"}))
            + &sse(json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}));
        assert!(drive(&model, decode_stream_event, bad.as_bytes(), 0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn thinking_end_retains_opaque_reasoning_state() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "thinking_start", "contentIndex": 0}))
            + &sse(json!({"type": "thinking_delta", "contentIndex": 0, "delta": "think"}))
            + &sse(json!({
                "type": "thinking_end", "contentIndex": 0, "content": "thinking",
                "contentSignature": "sig-1"
            }))
            + &sse(json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}));
        let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap();
        let response = match events.last().unwrap() {
            StreamEvent::Finished(response) => response,
            other => panic!("expected Finished, got {other:?}"),
        };
        let AssistantPart::Reasoning(reasoning) = &response.message.content[0] else {
            panic!("expected reasoning part");
        };
        assert_eq!(reasoning.text.as_deref(), Some("thinking"));
        assert!(matches!(
            reasoning.state.as_ref(),
            Some(ReasoningState {
                kind: ReasoningStateKind::AnthropicSignature { signature },
                ..
            }) if signature == "sig-1"
        ));
    }

    #[tokio::test]
    async fn terminal_tool_call_replaces_the_streamed_preview() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "lookup"}))
            + &sse(json!({"type": "toolcall_delta", "contentIndex": 0, "delta": "{\"city\":"}))
            + &sse(json!({"type": "toolcall_delta", "contentIndex": 0, "delta": "\"Pa"}))
            + &sse(json!({
                "type": "toolcall_end", "contentIndex": 0,
                "toolCall": {"type": "toolCall", "id": "call_1", "name": "lookup", "arguments": {"city": "Paris"}}
            }))
            + &sse(json!({"type": "done", "reason": "toolUse", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}));
        let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap();
        let response = match events.last().unwrap() {
            StreamEvent::Finished(response) => response,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert!(matches!(&response.message.content[..], [AssistantPart::ToolCall(call)]
            if call.id.0 == "call_1" && call.arguments_value().unwrap() == json!({"city": "Paris"})));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn tool_identity_change_is_rejected() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "lookup"}))
            + &sse(json!({
                "type": "toolcall_end", "contentIndex": 0,
                "toolCall": {"type": "toolCall", "id": "call_2", "name": "lookup", "arguments": {}}
            }))
            + &sse(json!({"type": "done", "reason": "toolUse", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}));
        assert!(drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn in_band_error_is_typed_and_never_echoes_provider_prose() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "text_start", "contentIndex": 0}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "partial"}))
            + &sse(json!({
                "type": "error", "reason": "error",
                "errorMessage": "provider-private request-private",
                "usage": {"input": 3, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 4}
            }));
        let error = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap_err();
        assert!(matches!(&error, AiError::Provider(provider)
            if provider.kind.as_deref() == Some("pi_messages_error")));
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("provider-private"));
            assert!(!rendered.contains("request-private"));
        }
    }

    #[tokio::test]
    async fn aborted_error_event_is_a_canceled_failure() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({
                "type": "error", "reason": "aborted",
                "usage": {"input": 3, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 4}
            }));
        let error = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap_err();
        assert!(matches!(error, AiError::Canceled));
    }

    #[tokio::test]
    async fn a_done_without_start_still_guards_a_started_stream() {
        let model = fixture_model();
        let wire = sse(json!({
            "type": "done", "reason": "stop",
            "usage": {"input": 1, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1}
        }));
        let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap();
        assert!(matches!(&events[0], StreamEvent::Started { .. }));
    }

    #[tokio::test]
    async fn rewrite_and_malformed_events_are_bounded() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "text_start", "contentIndex": 0}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "x"}))
            + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "x"}))
            + &sse(json!({
                "type": "done", "reason": "stop",
                "rewrite": {"policyId": "gateway-policy", "policyVersion": 3, "changed": true,
                    "tokenCountChange": -2, "messageCountChange": 1, "systemPromptChanged": false},
                "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}
            }));
        let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap();
        let response = match events.last().unwrap() {
            StreamEvent::Finished(response) => response,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert!(response
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "pi_messages_rewrite"
                && diagnostic.message.contains("gateway-policy v3")));

        for bad in [
            sse(json!({"type": "start"})) + &sse(json!({"type": "unknown_event"})),
            sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "x"})),
            sse(json!({"type": "start"})) + &sse(json!({"type": "done", "reason": "weird", "usage": {}})),
            sse(json!({"type": "start"})) + &sse(json!({"type": "done", "reason": "toolUse", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}})),
            sse(json!({"type": "start"})) + &sse(json!({"type": "start"})),
        ] {
            assert!(
                drive(&model, decode_stream_event, bad.as_bytes(), 0)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn signature_free_text_blocks_settle_without_reasoning_state() {
        let model = fixture_model();
        let wire = sse(json!({"type": "start"}))
            + &sse(json!({"type": "text_start", "contentIndex": 0}))
            + &sse(json!({"type": "text_delta", "contentIndex": 0, "delta": "done"}))
            + &sse(json!({"type": "text_end", "contentIndex": 0, "content": "done"}))
            + &sse(json!({"type": "done", "reason": "stop", "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2}}));
        let events = drive(&model, decode_stream_event, wire.as_bytes(), 0)
            .await
            .unwrap();
        assert!(matches!(
            events.last().unwrap(),
            StreamEvent::Finished(_)
        ));
    }
    const FIXTURE_TEXT_TOOL: &str =
        include_str!("../../tests/fixtures/pi_messages/text_tool_done.sse");
    const FIXTURE_ERROR: &str =
        include_str!("../../tests/fixtures/pi_messages/error_terminal.sse");
    const FIXTURE_THINKING: &str =
        include_str!("../../tests/fixtures/pi_messages/thinking_signature_done.sse");

    #[tokio::test]
    async fn fixture_text_and_tool_settle_once_and_are_chunking_invariant() {
        let model = fixture_model();
        let events = drive(&model, decode_stream_event, FIXTURE_TEXT_TOOL.as_bytes(), 0)
            .await
            .unwrap();
        let response = match events.last().unwrap() {
            StreamEvent::Finished(response) => response,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_eq!(response.response_id.as_deref(), Some("resp_fixture"));
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.cache_read_tokens, 3);
        assert_eq!(response.usage.total_tokens, 20);
        assert!(matches!(&response.message.content[..], [
            AssistantPart::Text(text), AssistantPart::ToolCall(call)
        ] if text == "Reading " && call.id.0 == "call_1"
            && call.arguments_value().unwrap() == json!({"city": "Paris"})));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
                .count(),
            1
        );
        // The trailing `[DONE]` frame is not a provider event and the missing
        // explicit text end is completed exactly once at the terminal.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::TextEnd { .. }))
                .count(),
            1
        );
        let chunked = drive(&model, decode_stream_event, FIXTURE_TEXT_TOOL.as_bytes(), 1)
            .await
            .unwrap();
        assert_eq!(chunked.len(), events.len());
    }

    #[tokio::test]
    async fn fixture_error_terminal_is_typed_and_prose_free() {
        let error = drive(&fixture_model(), decode_stream_event, FIXTURE_ERROR.as_bytes(), 0)
            .await
            .unwrap_err();
        assert!(matches!(&error, AiError::Provider(provider)
            if provider.kind.as_deref() == Some("pi_messages_error")));
        assert!(!format!("{error:?} {error}").contains("provider-private"));
    }

    #[tokio::test]
    async fn fixture_thinking_signature_and_terminal_metadata_survive() {
        let events = drive(&fixture_model(), decode_stream_event, FIXTURE_THINKING.as_bytes(), 0)
            .await
            .unwrap();
        let response = match events.last().unwrap() {
            StreamEvent::Finished(response) => response,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(response.usage.cache_write_tokens, 1);
        assert_eq!(response.usage.total_tokens, 13);
        match &response.message.content[0] {
            AssistantPart::Reasoning(reasoning) => {
                assert_eq!(reasoning.text.as_deref(), Some("weighing options"));
                assert!(matches!(
                    reasoning.state.as_ref(),
                    Some(ReasoningState {
                        kind: ReasoningStateKind::AnthropicSignature { signature },
                        ..
                    }) if signature == "opaque-sig"
                ));
            }
            other => panic!("expected reasoning, got {other:?}"),
        }
        assert!(matches!(&response.message.content[1], AssistantPart::Text(text) if text == "answer"));
        let codes: Vec<&str> = response
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"pi_messages_provider_thinking_level"));
        assert!(codes.contains(&"pi_messages_rewrite"));
    }

}
