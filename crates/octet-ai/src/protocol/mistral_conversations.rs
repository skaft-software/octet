//! Native Mistral Conversations, pinned to client-python
//! 3653cd9a5169fc151a0787232aed70eeea88e52a.
//!
//! POST `/v1/conversations`, `Accept: text/event-stream`, native `inputs`.
//! The SDK operation's `#stream` fragment is never part of the HTTP target.
//! Native SSE uses conversation.response.{started,done,error},
//! message.output.delta, function.call.delta, tool.execution.{started,delta,done},
//! and agent.handoff.{started,done}. Server tools and handoffs are not local
//! function calls and must not be silently treated as such.

use serde_json::{json, Value};

use crate::catalog::Model;
use crate::error::{AiError, DecodeError, Diagnostic, UnsupportedError};
use crate::protocol::sse::SseEvent;
use crate::protocol::HttpRequestParts;
use crate::stream::{ResponseBuilder, StreamEvent};
use crate::types::{
    AssistantPart, CompatibilityMode, Media, Message, OutputFormat, OutputModalities, Protocol,
    ReasoningConfig, ReasoningMode, Request, ToolChoice, ToolResultPart, UserPart,
};

/// There is no Conversations continuation option in the canonical request. Replay
/// the supplied entries into a new, non-stored conversation; never interpret a
/// cache session ID as a provider conversation ID.
pub(crate) fn build_request(model: &Model, req: &Request) -> Result<HttpRequestParts, AiError> {
    if req.reasoning != ReasoningConfig::Off {
        return Err(UnsupportedError::Reasoning.into());
    }
    if req.reasoning_mode != ReasoningMode::Standard {
        return Err(UnsupportedError::ReasoningMode.into());
    }
    // CompletionArgs accepts only enum tool choices, not a named function.
    // Weakening a forced function to auto/required would change execution intent.
    if matches!(&req.tool_choice, ToolChoice::Named(_)) {
        return Err(UnsupportedError::ToolChoice.into());
    }
    let mut diagnostics = crate::validate::validate_request(
        req,
        &model.spec.capabilities,
        &model.spec.limits,
        Protocol::MistralConversations,
        &model.spec.id,
        req.compatibility,
    )?;
    let mut inputs = Vec::new();
    for message in &req.messages {
        match message {
            Message::User(user) => {
                for part in &user.content {
                    match part {
                        UserPart::Text(text) => inputs.push(json!({
                            "object": "entry", "type": "message.input",
                            "role": "user", "content": text,
                        })),
                        UserPart::ToolResult(result) => {
                            let mut text = String::new();
                            if result.is_error {
                                // FunctionResultEntry has a string result, not an is_error field.
                                text.push_str("Error: ");
                            }
                            for part in &result.content {
                                match part {
                                    ToolResultPart::Text(value) => text.push_str(value),
                                    ToolResultPart::Media(_) => drop_unsupported(
                                        req, &mut diagnostics, UnsupportedError::ToolResultMedia,
                                        "dropped_tool_result_media",
                                    )?,
                                }
                            }
                            inputs.push(json!({
                                "object": "entry", "type": "function.result",
                                "tool_call_id": result.tool_call_id.0, "result": text,
                            }));
                        }
                        UserPart::Media(media) => drop_media(req, &mut diagnostics, media)?,
                    }
                }
            }
            Message::Assistant(assistant) => {
                for part in &assistant.content {
                    match part {
                        AssistantPart::Text(text) => inputs.push(json!({
                            "object": "entry", "type": "message.output",
                            "role": "assistant", "content": text,
                        })),
                        AssistantPart::ToolCall(call) => inputs.push(json!({
                            "object": "entry", "type": "function.call",
                            "tool_call_id": call.id.0, "name": call.name,
                            "arguments": call.arguments_json,
                        })),
                        AssistantPart::Reasoning(_) | AssistantPart::ProviderMetadata(_) => {
                            drop_unsupported(
                                req, &mut diagnostics, UnsupportedError::Reasoning,
                                "dropped_reasoning_state",
                            )?;
                        }
                        AssistantPart::Media(media) => drop_media(req, &mut diagnostics, media)?,
                    }
                }
            }
        }
    }
    if req.output_modalities != OutputModalities::Text {
        drop_unsupported(
            req, &mut diagnostics, UnsupportedError::AudioOutput, "dropped_audio_output",
        )?;
    }
    let mut body = json!({
        "model": model.spec.api_name,
        "inputs": inputs,
        "stream": true,
        "store": false,
        // Do not delegate handoff execution through a provider default. A
        // received handoff remains unsupported, not local execution authority.
        "handoff_execution": "client",
    });
    if let Some(system) = &req.system {
        body["instructions"] = json!(system);
    }
    if !req.tools.is_empty() && model.spec.capabilities.tools {
        body["tools"] = Value::Array(req.tools.iter().map(|tool| json!({
            "type": "function",
            "function": {
                "name": tool.name, "description": tool.description,
                "parameters": tool.parameters,
            },
        })).collect());
    }
    let mut args = serde_json::Map::new();
    if let Some(temperature) = req.temperature {
        args.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = req.max_output_tokens {
        args.insert("max_tokens".into(), json!(max_tokens));
    }
    if !req.stop.is_empty() {
        args.insert("stop".into(), json!(req.stop));
    }
    if model.spec.capabilities.tools {
        let choice = match &req.tool_choice {
            ToolChoice::Auto => "auto",
            ToolChoice::None => "none",
            ToolChoice::Required => "required",
            ToolChoice::Named(_) => unreachable!("named choice rejected above"),
        };
        args.insert("tool_choice".into(), json!(choice));
    }
    if model.spec.capabilities.structured_output {
        let format = match &req.output_format {
            OutputFormat::Text => None,
            OutputFormat::JsonObject => Some(json!({"type": "json_object"})),
            OutputFormat::JsonSchema(schema) => {
                let mut value = json!({
                    "name": schema.name, "schema": schema.schema, "strict": schema.strict,
                });
                if let Some(description) = &schema.description {
                    value["description"] = json!(description);
                }
                Some(json!({"type": "json_schema", "json_schema": value}))
            }
        };
        if let Some(format) = format {
            args.insert("response_format".into(), format);
        }
    }
    if !args.is_empty() {
        body["completion_args"] = Value::Object(args);
    }
    let mut headers = http::HeaderMap::new();
    headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
    headers.insert(http::header::ACCEPT, http::HeaderValue::from_static("text/event-stream"));
    Ok(HttpRequestParts {
        url: crate::protocol::endpoint_url(&model.endpoint.base_url, "conversations")?,
        headers,
        body: serde_json::to_vec(&body).map_err(|_| DecodeError::Json(
            "cannot encode Mistral Conversations request".into(),
        ))?.into(),
        streaming: true,
        diagnostics,
    })
}

fn drop_unsupported(
    req: &Request,
    diagnostics: &mut Vec<Diagnostic>,
    error: UnsupportedError,
    code: &str,
) -> Result<(), AiError> {
    if req.compatibility == CompatibilityMode::Strict {
        return Err(error.into());
    }
    if !diagnostics.iter().any(|diagnostic| diagnostic.code == code) {
        diagnostics.push(Diagnostic {
            code: code.into(),
            message: format!("Mistral Conversations: {error}; omitted in Lossy mode"),
        });
    }
    Ok(())
}

fn drop_media(req: &Request, diagnostics: &mut Vec<Diagnostic>, media: &Media) -> Result<(), AiError> {
    let (error, code) = match media {
        Media::Image(_) => (UnsupportedError::Image, "dropped_image"),
        Media::Audio(_) => (UnsupportedError::Audio, "dropped_audio"),
    };
    drop_unsupported(req, diagnostics, error, code)
}

/// Decode only the ten event variants in the pinned ConversationEvents union.
/// EOF, `[DONE]`, Chat choices and Responses events are not native terminals.
pub(crate) fn decode_stream_event(
    _model: &Model,
    event: &SseEvent,
    builder: &mut ResponseBuilder,
) -> Result<Vec<StreamEvent>, AiError> {
    use crate::error::{ProviderError, StreamProtocolError};
    use crate::protocol::emit_event;
    use crate::types::{StopReason, ToolCallId};

    if builder.mistral_finished {
        return Err(StreamProtocolError::EventAfterFinish.into());
    }
    builder.observe_provider_stream_event()?;
    let value: Value = serde_json::from_str(&event.data)
        .map_err(|_| malformed("invalid event JSON"))?;
    let kind = string(&value, "type")?;
    if event.event.as_deref().is_some_and(|name| name != kind) {
        return Err(malformed("SSE event name and data type disagree"));
    }
    let mut events = Vec::new();
    if kind == "conversation.response.error" {
        // The native error code is an integer, not an HTTP status or retry grant.
        // Do not echo provider prose: it may contain credentials or request data.
        string(&value, "message")?;
        let code = value["code"].as_i64().ok_or_else(|| malformed("invalid error code"))?;
        builder.mistral_finished = true;
        return Err(ProviderError {
            code: Some(code.to_string()),
            kind: Some("conversation.response.error".into()),
            message: "Mistral Conversations response failed".into(),
            request_id: None,
        }.into());
    }
    if kind != "conversation.response.started" && !builder.started {
        return Err(StreamProtocolError::MissingStart.into());
    }
    match kind {
        "conversation.response.started" => {
            if builder.started {
                return Err(StreamProtocolError::DuplicateStart.into());
            }
            let id = nonempty_string(&value, "conversation_id")?;
            builder.reserve_buffered_content(id.len())?;
            emit_event(&mut events, builder, StreamEvent::Started {
                response_id: Some(id.to_owned()),
            })?;
        }
        "message.output.delta" => {
            if value.get("role").is_some_and(|role| role != "assistant") {
                return Err(malformed("invalid message role"));
            }
            let output = index(&value, "output_index")?;
            index(&value, "content_index")?;
            track_entry(builder, output, "message", nonempty_string(&value, "id")?)?;
            let content = value.get("content").ok_or_else(|| malformed("missing content"))?;
            let text = if let Some(text) = content.as_str() {
                text
            } else {
                match string(content, "type")? {
                    "text" => string(content, "text")?,
                    "thinking" | "image_url" | "document_url" | "tool_file" | "tool_reference" => {
                        omit_output(builder, "dropped_mistral_content", "non-text content is unsupported")?;
                        return Ok(events);
                    }
                    _ => return Err(malformed("unknown output content type")),
                }
            };
            // The pinned SDK concatenates text chunks within one output entry.
            // A new content index must not move part of that message after an
            // interleaved function entry in the canonical response/replay.
            let canonical = part_index(builder, &format!("mistral:text:{output}"))?;
            if !builder.text_buffers.contains_key(&canonical) {
                emit_event(&mut events, builder, StreamEvent::TextStart { index: canonical })?;
            }
            if !text.is_empty() {
                emit_event(&mut events, builder, StreamEvent::TextDelta {
                    index: canonical, delta: text.to_owned(),
                })?;
            }
        }
        "function.call.delta" => {
            let output = index(&value, "output_index")?;
            track_entry(builder, output, "function", nonempty_string(&value, "id")?)?;
            if value.get("confirmation_status").is_some_and(|status| !status.is_null()) {
                // No canonical provider-confirmation authority exists. Lossy may
                // not turn a pending/rejected provider call into local execution.
                return Err(malformed("function confirmation status is unsupported"));
            }
            let name = string(&value, "name")?;
            let id = string(&value, "tool_call_id")?;
            let arguments = string(&value, "arguments")?;
            let canonical = part_index(builder, &format!("mistral:function:{output}"))?;
            if let Some(call) = builder.tool_call_builders.get(&canonical) {
                if (!id.is_empty() && id != call.id.0) || (!name.is_empty() && name != call.name) {
                    return Err(malformed("function identity changed within an entry"));
                }
            } else {
                if name.is_empty() || id.is_empty() {
                    return Err(malformed("missing initial function identity"));
                }
                if builder.tool_call_builders.values().any(|call| call.id.0 == id) {
                    return Err(malformed("duplicate function call ID"));
                }
                emit_event(&mut events, builder, StreamEvent::ToolCallStart {
                    index: canonical, id: ToolCallId(id.to_owned()), name: name.to_owned(),
                })?;
            }
            if !arguments.is_empty() {
                emit_event(&mut events, builder, StreamEvent::ToolCallArgsDelta {
                    index: canonical, delta: arguments.to_owned(),
                })?;
            }
        }
        "conversation.response.done" => {
            let usage = value.get("usage").filter(|value| value.is_object())
                .ok_or_else(|| malformed("missing terminal usage"))?;
            let usage = decode_usage(builder, usage)?;
            // Validate every call before exposing any ToolCallEnd. A native done
            // has no length/truncation reason and cannot authorize guessed args.
            for call in builder.tool_call_builders.values() {
                if !serde_json::from_str::<Value>(&call.arguments_json)
                    .is_ok_and(|value| value.is_object())
                {
                    return Err(malformed("invalid completed function arguments"));
                }
            }
            let reason = if builder.tool_call_builders.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            };
            builder.set_stop_reason(reason);
            let mut indices: Vec<_> = builder.observed_indices.iter().copied().collect();
            indices.sort_unstable();
            for index in indices {
                let end = if builder.text_buffers.contains_key(&index) {
                    StreamEvent::TextEnd { index }
                } else {
                    StreamEvent::ToolCallEnd { index, argument_error: None }
                };
                emit_event(&mut events, builder, end)?;
            }
            emit_event(&mut events, builder, StreamEvent::Usage(usage))?;
            let response = builder.finish_mut()?;
            builder.mistral_finished = true;
            events.push(StreamEvent::Finished(response));
        }
        "tool.execution.started" | "tool.execution.delta" | "tool.execution.done" => {
            // Track even omitted entries: their output slots may never become
            // local functions. These are server-side connectors, not ToolCalls.
            let output = index(&value, "output_index")?;
            track_entry(builder, output, "server_tool", nonempty_string(&value, "id")?)?;
            omit_output(builder, "dropped_mistral_server_tool", "server tool execution is unsupported")?;
        }
        "agent.handoff.started" | "agent.handoff.done" => {
            let output = index(&value, "output_index")?;
            track_entry(builder, output, "handoff", nonempty_string(&value, "id")?)?;
            omit_output(builder, "dropped_mistral_handoff", "server agent handoff is unsupported")?;
        }
        // Handled before the start gate, including an error before a started event.
        "conversation.response.error" => unreachable!("native error handled above"),
        _ => return Err(malformed("unknown Conversations event type")),
    }
    Ok(events)
}

fn malformed(message: &str) -> AiError {
    DecodeError::Json(format!("Mistral Conversations: {message}")).into()
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, AiError> {
    value.get(field).and_then(Value::as_str)
        .ok_or_else(|| malformed("missing or invalid string field"))
}

fn nonempty_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, AiError> {
    let text = string(value, field)?;
    if text.is_empty() {
        return Err(malformed("empty entry identifier"));
    }
    Ok(text)
}

fn index(value: &Value, field: &str) -> Result<u64, AiError> {
    match value.get(field) {
        None => Ok(0), // Pinned SDK default for omitted output/content indices.
        Some(value) => value.as_u64().ok_or_else(|| malformed("invalid entry index")),
    }
}

fn part_index(builder: &mut ResponseBuilder, key: &str) -> Result<usize, AiError> {
    if !builder.provider_to_canonical_indices.contains_key(key)
        && builder.provider_to_canonical_indices.len() >= crate::stream::MAX_RESPONSE_PARTS
    {
        return Err(DecodeError::TooManyResponseParts.into());
    }
    Ok(crate::protocol::get_canonical_index(builder, key))
}

fn track_entry(builder: &mut ResponseBuilder, output: u64, kind: &str, id: &str) -> Result<(), AiError> {
    let key = format!("mistral:entry:{output}");
    let identity = format!("{kind}:{id}");
    if let Some(previous) = builder.temp_buffers.get(&key) {
        if previous != &identity {
            return Err(malformed("entry identity changed at an output index"));
        }
    } else {
        // Canonical parts retain first-observation order. A new earlier native
        // entry would silently reorder effects; only known entries may interleave.
        if builder.mistral_last_output_index.is_some_and(|last| output < last) {
            return Err(malformed("out-of-order native entry is unsupported"));
        }
        if builder.temp_buffers.len() >= crate::stream::MAX_RESPONSE_PARTS {
            return Err(DecodeError::TooManyResponseParts.into());
        }
        builder.replace_temp_buffer(key, identity)?;
        builder.mistral_last_output_index = Some(output);
    }
    Ok(())
}

fn omit_output(builder: &mut ResponseBuilder, code: &str, message: &str) -> Result<(), AiError> {
    if builder.compatibility == CompatibilityMode::Strict {
        return Err(malformed(message));
    }
    if !builder.diagnostics.iter().any(|diagnostic| diagnostic.code == code) {
        builder.add_diagnostic(Diagnostic {
            code: code.to_owned(), message: format!("Mistral Conversations: {message}; omitted in Lossy mode"),
        });
    }
    Ok(())
}

fn decode_usage(builder: &mut ResponseBuilder, value: &Value) -> Result<crate::types::Usage, AiError> {
    // Cache and reasoning counters are not in ConversationUsageInfo. Never infer
    // them from Chat usage fields, nor relabel connector tokens as model output.
    // Missing model counters default to zero; only connector fields are nullable
    // in ConversationUsageInfo. An explicit invalid counter is not missing usage.
    let count = |field, nullable| match value.get(field) {
        None => Ok(0),
        Some(Value::Null) if nullable => Ok(0),
        Some(value) => value.as_u64().ok_or_else(|| malformed("invalid usage counter")),
    };
    let connector_tokens = count("connector_tokens", true)?;
    let connectors = match value.get("connectors") {
        None | Some(Value::Null) => false,
        Some(Value::Object(connectors)) => {
            for count in connectors.values() {
                if count.as_u64().is_none() {
                    return Err(malformed("invalid connector usage"));
                }
            }
            !connectors.is_empty()
        }
        Some(_) => return Err(malformed("invalid connector usage")),
    };
    if connector_tokens != 0 || connectors {
        omit_output(builder, "dropped_mistral_connector_usage", "connector usage breakdown is unsupported")?;
    }
    Ok(crate::types::Usage {
        input_tokens: count("prompt_tokens", false)?,
        output_tokens: count("completion_tokens", false)?,
        total_tokens: count("total_tokens", false)?,
        ..Default::default()
    })
}
