//! Pure request validation and capability checks shared by every codec.

use crate::error::{AiError, Diagnostic, UnsupportedError, ValidationError};
use crate::types::{
    AssistantPart, AudioFormat, AudioPayload, AudioVoice, Capabilities, ImageSource, Media,
    Message, ModelId, ModelLimits, OutputFormat, OutputModalities, Protocol, ReasoningConfig,
    ReasoningMode, Request, ToolCallId, ToolChoice, ToolResultPart, UserPart,
};
use crate::CompatibilityMode;
use std::borrow::Cow;
use std::collections::HashSet;
use std::time::SystemTime;

/// Whether a provider media reference can be sent on `protocol` right now.
///
/// A reference is usable only when it was minted for the same protocol and has
/// not expired. Validation flags an unusable reference with a diagnostic
/// (Strict errors); codecs call this to actually *drop* the part on the wire so
/// an expired/wrong-protocol file ID is never serialized (design §7).
pub(crate) fn provider_ref_is_usable(
    reference: &crate::types::ProviderMediaRef,
    protocol: Protocol,
) -> bool {
    reference.protocol == protocol
        && reference
            .expires_at
            .is_none_or(|expires_at| expires_at > SystemTime::now())
}

/// Preserve explicit core intent; supported choices and V2 authority are checked
/// by request validation rather than silently clamping this selection.
pub(crate) fn normalize_reasoning_config<'a>(
    reasoning: &'a ReasoningConfig,
    caps: &Capabilities,
) -> Cow<'a, ReasoningConfig> {
    // Explicit core requests are never silently clamped. Only the product
    // boundary may normalize persisted choices, with a visible diagnostic.
    let _ = caps;
    Cow::Borrowed(reasoning)
}

/// Preserve request intent before shared validation. Product-level normalization
/// of persisted selections is separate from the strict core request boundary.
pub(crate) fn normalize_request_reasoning<'a>(
    req: &'a Request,
    caps: &Capabilities,
) -> Cow<'a, Request> {
    let normalized = normalize_reasoning_config(&req.reasoning, caps);
    if matches!(normalized, Cow::Borrowed(_)) {
        return Cow::Borrowed(req);
    }
    let mut request = req.clone();
    request.reasoning = normalized.into_owned();
    Cow::Owned(request)
}

/// Shared exact-choice gate for generation and native compact construction.
pub(crate) fn validate_reasoning_selection(
    reasoning: &ReasoningConfig,
    caps: &Capabilities,
    protocol: Protocol,
) -> Result<(), AiError> {
    let supported = match &caps.reasoning {
        None => *reasoning == ReasoningConfig::Off,
        Some(cap) => {
            cap.supports(reasoning)
                && (*reasoning != ReasoningConfig::Effort(crate::types::ReasoningEffort::Ultra)
                    || caps.agent_delegation == Some(crate::types::AgentDelegation::V2))
        }
    };
    // Explicit controls must fail even in Lossy mode: omission can enable
    // provider-default thinking while the caller believes reasoning is Off.
    let google_level_off = protocol == Protocol::GoogleGenerativeAi
        && *reasoning == ReasoningConfig::Off
        && caps
            .reasoning
            .as_ref()
            .is_some_and(|c| c.control == crate::types::ReasoningControl::Effort);
    if !supported || google_level_off {
        return Err(AiError::Unsupported(UnsupportedError::Reasoning));
    }
    Ok(())
}

/// Validates a request against the model's capabilities and protocol constraints.
///
/// In `Strict` mode, returns the first error encountered.
/// In `Lossy` mode, returns a list of diagnostics for any dropped or downgraded features.
pub(crate) fn validate_request(
    req: &Request,
    caps: &Capabilities,
    limits: &ModelLimits,
    protocol: Protocol,
    target_model: &ModelId,
    mode: CompatibilityMode,
) -> Result<Vec<Diagnostic>, AiError> {
    let mut diagnostics = Vec::new();

    // No codec emits the native load-point schemas/references yet. Accepting
    // this flag previously hid announced Chat tools without loading them.
    if caps.deferred_tool_loading {
        return Err(crate::error::ConfigError::InvalidModel(target_model.clone()).into());
    }

    if req.responses.is_some() && protocol != Protocol::OpenAiResponses {
        return Err(AiError::Unsupported(UnsupportedError::ResponsesOptions));
    }

    // 1. Temperature check
    if let Some(temp) = req.temperature {
        if !temp.is_finite() || !(0.0..=2.0).contains(&temp) {
            return Err(AiError::Validation(ValidationError::InvalidTemperature));
        }
    }

    // 2. Max output tokens check
    if let Some(requested) = req.max_output_tokens {
        if requested == 0 || requested > limits.max_output_tokens {
            return Err(AiError::Validation(
                ValidationError::InvalidMaxOutputTokens {
                    requested,
                    model_max: limits.max_output_tokens,
                },
            ));
        }
    }

    for tool in &req.tools {
        if tool.async_execution
            && (protocol != Protocol::OpenAiResponses || !caps.responses_features.async_tools)
        {
            return Err(crate::error::ConfigError::Parse(
                "async tools are not qualified for this route".into(),
            )
            .into());
        }
        if tool.name.is_empty() || !tool.parameters.is_object() {
            return Err(AiError::Validation(ValidationError::InvalidToolSchema(
                tool.name.clone(),
            )));
        }
    }
    if let ToolChoice::Named(name) = &req.tool_choice {
        if !req.tools.iter().any(|tool| &tool.name == name) {
            return Err(AiError::Validation(ValidationError::InvalidToolSchema(
                format!("named tool `{name}` is not defined"),
            )));
        }
    }
    if protocol == Protocol::OpenAiResponses && !req.stop.is_empty() {
        if mode == CompatibilityMode::Strict {
            return Err(AiError::Unsupported(UnsupportedError::StopSequences));
        }
        diagnostics.push(Diagnostic {
            code: "dropped_stop_sequences".to_string(),
            message: "OpenAI Responses does not accept stop sequences".to_string(),
        });
    }

    // 3. Tool results and calls pairing
    // Async-qualified Responses histories need globally unique call identities:
    // their results may arrive across turns. Other codecs may generate fallback
    // IDs per response, so a completed synchronous identity can be reused.
    let global_call_ids =
        protocol == Protocol::OpenAiResponses && caps.responses_features.async_tools;
    // A completed historical call is replay, not a request to execute its old
    // tool again. Index potential results here; the ordered walk below still
    // rejects orphans, duplicates and reused identities before accepting them.
    let historical_results: HashSet<&ToolCallId> = req
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            UserPart::ToolResult(result) => Some(&result.tool_call_id),
            _ => None,
        })
        .collect();
    let mut all_tool_calls: HashSet<ToolCallId> = HashSet::new();
    let mut pending_calls: HashSet<ToolCallId> = HashSet::new();
    let mut pending_async: HashSet<ToolCallId> = HashSet::new();
    let mut completed_calls: HashSet<ToolCallId> = HashSet::new();

    for msg in &req.messages {
        match msg {
            Message::Assistant(ref assistant) => {
                // Google, Anthropic, and OpenAI Responses require function/tool
                // responses before another assistant turn.
                if (protocol == Protocol::AnthropicMessages
                    || protocol == Protocol::OpenAiResponses
                    || protocol == Protocol::BedrockConverse
                    || protocol == Protocol::GoogleGenerativeAi)
                    && !pending_calls.is_empty()
                {
                    // In Strict mode, this is a ValidationError.
                    // In Lossy mode, we emit a diagnostic and expect the codec to insert a synthetic error.
                    for call_id in pending_calls.difference(&pending_async) {
                        if mode == CompatibilityMode::Strict {
                            return Err(AiError::Validation(ValidationError::MissingToolResult(
                                call_id.clone(),
                            )));
                        } else {
                            diagnostics.push(Diagnostic {
                                code: "missing_tool_result".to_string(),
                                message: format!("Missing tool result for call ID {:?}", call_id),
                            });
                        }
                    }
                    pending_calls.retain(|id| pending_async.contains(id));
                }

                // Collect tool calls from this assistant message
                for part in &assistant.content {
                    if let AssistantPart::ToolCall(ref tc) = part {
                        // Invalid caller input is a validation error, not a wire decode error.
                        tc.arguments_value().map_err(|_| {
                            AiError::Validation(ValidationError::ToolArgumentsNotObject(
                                tc.id.clone(),
                            ))
                        })?;
                        if !all_tool_calls.insert(tc.id.clone())
                            && (global_call_ids || pending_calls.contains(&tc.id))
                        {
                            return Err(crate::error::ConfigError::Parse(
                                "duplicate tool call ID".into(),
                            )
                            .into());
                        }
                        if !global_call_ids {
                            completed_calls.remove(&tc.id);
                        }
                        if tc.async_execution {
                            if protocol != Protocol::OpenAiResponses
                                || !caps.responses_features.async_tools
                                || assistant.protocol != protocol
                                || &assistant.model != target_model
                            {
                                return Err(crate::error::ConfigError::Parse(
                                    "async call history is not qualified for this model route"
                                        .into(),
                                )
                                .into());
                            }
                            if historical_results.contains(&tc.id) {
                                // Its paired result makes this historical data. A
                                // current definition may be removed or changed;
                                // it is not the past call's execution authority.
                                pending_async.insert(tc.id.clone());
                            } else {
                                if !req
                                    .tools
                                    .iter()
                                    .any(|tool| tool.name == tc.name && tool.async_execution)
                                {
                                    return Err(crate::error::ConfigError::Parse(
                                        "pending async call was not advertised for this tool"
                                            .into(),
                                    )
                                    .into());
                                }
                                let arguments = tc.arguments_value()?;
                                if tc.argument_error.is_none()
                                    && matches!(
                                        crate::json_repair::validate_tool_arguments(
                                            &tc.name, &arguments, &req.tools
                                        )?,
                                        crate::types::ToolArgumentValidation::Valid
                                    )
                                {
                                    pending_async.insert(tc.id.clone());
                                }
                            }
                        }
                        pending_calls.insert(tc.id.clone());
                    }
                }
            }
            Message::User(ref user) => {
                for part in &user.content {
                    if let UserPart::ToolResult(ref tr) = part {
                        if !all_tool_calls.contains(&tr.tool_call_id) {
                            return Err(AiError::Validation(ValidationError::OrphanToolResult(
                                tr.tool_call_id.clone(),
                            )));
                        }
                        if !completed_calls.insert(tr.tool_call_id.clone()) {
                            return Err(crate::error::ConfigError::Parse(
                                "duplicate tool result".into(),
                            )
                            .into());
                        }
                        pending_calls.remove(&tr.tool_call_id);
                        pending_async.remove(&tr.tool_call_id);
                    }
                }
            }
        }
    }

    if (protocol == Protocol::AnthropicMessages
        || protocol == Protocol::OpenAiResponses
        || protocol == Protocol::BedrockConverse
        || protocol == Protocol::GoogleGenerativeAi)
        && !pending_calls.is_empty()
    {
        for call_id in pending_calls.difference(&pending_async) {
            if mode == CompatibilityMode::Strict {
                return Err(AiError::Validation(ValidationError::MissingToolResult(
                    call_id.clone(),
                )));
            }
            diagnostics.push(Diagnostic {
                code: "missing_tool_result".to_string(),
                message: format!("Missing tool result for call ID {:?}", call_id),
            });
        }
    }

    // 4. Modality/capability checks on messages
    for msg in &req.messages {
        match msg {
            Message::User(ref user) => {
                for part in &user.content {
                    match part {
                        UserPart::Media(Media::Image(ref image)) => {
                            if !caps
                                .input_modalities
                                .contains(crate::types::Modality::Image)
                            {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(UnsupportedError::Image));
                                } else {
                                    diagnostics.push(Diagnostic {
                                        code: "dropped_image".to_string(),
                                        message: "Model does not support image input".to_string(),
                                    });
                                }
                            }

                            // Inline images must carry an explicit media type: the
                            // wire mapping (`data:<mime>;base64,…` / Anthropic
                            // `source.media_type`) has no documented default, and
                            // guessing a wire field is forbidden (design §75).
                            // Anthropic further restricts it to a documented set.
                            if let ImageSource::Inline(_) = &image.source {
                                let media_issue = match &image.media_type {
                                    None => Some(
                                        "Inline images require an explicit media type".to_string(),
                                    ),
                                    Some(mime)
                                        if (protocol == Protocol::AnthropicMessages
                                            || protocol == Protocol::BedrockConverse)
                                            && !matches!(
                                                mime.as_ref(),
                                                "image/jpeg"
                                                    | "image/png"
                                                    | "image/gif"
                                                    | "image/webp"
                                            ) =>
                                    {
                                        Some("Anthropic and Bedrock inline images require JPEG, PNG, GIF, or WebP media type".to_string())
                                    }
                                    Some(_) => None,
                                };
                                if let Some(message) = media_issue {
                                    if mode == CompatibilityMode::Strict {
                                        return Err(AiError::Unsupported(UnsupportedError::Image));
                                    }
                                    diagnostics.push(Diagnostic {
                                        code: "dropped_image_media_type".to_string(),
                                        message,
                                    });
                                }
                            }

                            // Bedrock and Gemini accept inline image bytes, not arbitrary remote URLs.
                            // In particular, Google `fileData` references have a distinct trust
                            // contract and must not expand the provider's network authority.
                            if matches!(
                                protocol,
                                Protocol::BedrockConverse | Protocol::GoogleGenerativeAi
                            ) && matches!(&image.source, ImageSource::Url(_))
                            {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(UnsupportedError::Image));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_image_url".to_string(),
                                    message: match protocol {
                                        Protocol::BedrockConverse => {
                                            "Bedrock Converse requires inline image bytes".to_string()
                                        }
                                        Protocol::GoogleGenerativeAi => "Google Generative AI supports inline image input; arbitrary image URLs are not forwarded".to_string(),
                                        _ => unreachable!("only inline-image protocols reach this branch"),
                                    },
                                });
                            }

                            // Provider-hosted image IDs are only documented for Responses.
                            if matches!(&image.source, ImageSource::ProviderRef(_))
                                && protocol != Protocol::OpenAiResponses
                            {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(
                                        UnsupportedError::ProviderMediaRef,
                                    ));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_provider_media_ref".to_string(),
                                    message: "Image provider references are only supported by OpenAI Responses".to_string(),
                                });
                            }

                            // Check provider ref protocol and expiration
                            if let ImageSource::ProviderRef(ref r) = image.source {
                                if r.protocol != protocol {
                                    if mode == CompatibilityMode::Strict {
                                        return Err(AiError::Unsupported(
                                            UnsupportedError::ProviderMediaRef,
                                        ));
                                    } else {
                                        diagnostics.push(Diagnostic {
                                            code: "dropped_mismatched_media_ref".to_string(),
                                            message: "Provider media reference protocol mismatch"
                                                .to_string(),
                                        });
                                    }
                                }
                                if let Some(expires_at) = r.expires_at {
                                    if expires_at <= SystemTime::now() {
                                        if mode == CompatibilityMode::Strict {
                                            return Err(AiError::Unsupported(
                                                UnsupportedError::ProviderMediaRef,
                                            ));
                                        } else {
                                            diagnostics.push(Diagnostic {
                                                code: "dropped_expired_media_ref".to_string(),
                                                message: "Provider media reference is expired"
                                                    .to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        UserPart::Media(Media::Audio(ref audio)) => {
                            if protocol != Protocol::OpenAiChat {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(UnsupportedError::Audio));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_audio".to_string(),
                                    message: "Audio input is Chat-only in v0.1".to_string(),
                                });
                            }
                            if protocol == Protocol::OpenAiChat
                                && !caps
                                    .input_modalities
                                    .contains(crate::types::Modality::Audio)
                            {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(UnsupportedError::Audio));
                                } else {
                                    diagnostics.push(Diagnostic {
                                        code: "dropped_audio".to_string(),
                                        message: "Model does not support audio input".to_string(),
                                    });
                                }
                            }

                            if protocol == Protocol::OpenAiChat
                                && matches!(audio.payload, AudioPayload::ProviderRef(_))
                            {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(
                                        UnsupportedError::ProviderMediaRef,
                                    ));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_provider_media_ref".to_string(),
                                    message:
                                        "Bare audio references cannot be used as Chat user input"
                                            .to_string(),
                                });
                            }

                            // Chat format gate (only Wav or Mp3 for inline input)
                            if protocol == Protocol::OpenAiChat {
                                match audio.format {
                                    AudioFormat::Wav | AudioFormat::Mp3 => {}
                                    _ => {
                                        if mode == CompatibilityMode::Strict {
                                            return Err(AiError::Unsupported(
                                                UnsupportedError::Audio,
                                            ));
                                        } else {
                                            diagnostics.push(Diagnostic {
                                                code: "dropped_audio_format".to_string(),
                                                message: format!("OpenAI Chat only supports Wav/Mp3 audio input. Got {:?}", audio.format),
                                            });
                                        }
                                    }
                                }
                            }

                            // Check provider ref protocol and expiration
                            let ref_opt = match &audio.payload {
                                AudioPayload::ProviderRef(r) => Some(r),
                                AudioPayload::InlineWithProviderRef { reference, .. } => {
                                    Some(reference)
                                }
                                AudioPayload::Inline(_) => None,
                            };
                            if let Some(r) = ref_opt.filter(|_| protocol == Protocol::OpenAiChat) {
                                if r.protocol != protocol {
                                    if mode == CompatibilityMode::Strict {
                                        return Err(AiError::Unsupported(
                                            UnsupportedError::ProviderMediaRef,
                                        ));
                                    } else {
                                        diagnostics.push(Diagnostic {
                                            code: "dropped_mismatched_media_ref".to_string(),
                                            message: "Provider media reference protocol mismatch"
                                                .to_string(),
                                        });
                                    }
                                }
                                if let Some(expires_at) = r.expires_at {
                                    if expires_at <= SystemTime::now() {
                                        if mode == CompatibilityMode::Strict {
                                            return Err(AiError::Unsupported(
                                                UnsupportedError::ProviderMediaRef,
                                            ));
                                        } else {
                                            diagnostics.push(Diagnostic {
                                                code: "dropped_expired_media_ref".to_string(),
                                                message: "Provider media reference is expired"
                                                    .to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        UserPart::ToolResult(ref tr) => {
                            for part in &tr.content {
                                let supported = match part {
                                    ToolResultPart::Text(_) => true,
                                    ToolResultPart::Media(Media::Image(image)) => match protocol {
                                        Protocol::OpenAiChat => false,
                                        Protocol::OpenAiResponses => match &image.source {
                                            ImageSource::ProviderRef(reference) => {
                                                reference.protocol == protocol
                                                    && reference.expires_at.is_none_or(|expiry| {
                                                        expiry > SystemTime::now()
                                                    })
                                            }
                                            _ => true,
                                        },
                                        Protocol::AnthropicMessages => {
                                            matches!(
                                                &image.source,
                                                ImageSource::Inline(_) | ImageSource::Url(_)
                                            )
                                        }
                                        // Bedrock Converse and Gemini functionResponse have no
                                        // documented tool-result media mapping.
                                        Protocol::BedrockConverse
                                        | Protocol::GoogleGenerativeAi => false,
                                        // No evidenced native Conversations tool-result media schema.
                                        Protocol::MistralConversations => false,
                                        // Pi's `ImageContent` block carries inline bytes only;
                                        // a URL or provider reference has no mapping.
                                        Protocol::PiMessages => {
                                            matches!(&image.source, ImageSource::Inline(_))
                                        }
                                    },
                                    ToolResultPart::Media(Media::Audio(_)) => false,
                                };
                                if !supported {
                                    if mode == CompatibilityMode::Strict {
                                        return Err(AiError::Unsupported(
                                            UnsupportedError::ToolResultMedia,
                                        ));
                                    }
                                    diagnostics.push(Diagnostic {
                                        code: "dropped_tool_result_media".to_string(),
                                        message: "Tool-result media has no mapping on the target protocol".to_string(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant(ref assistant) => {
                let assistant_media_count = assistant
                    .content
                    .iter()
                    .filter(|part| matches!(part, AssistantPart::Media(_)))
                    .count();
                if assistant_media_count > 1 {
                    if mode == CompatibilityMode::Strict {
                        return Err(AiError::Unsupported(UnsupportedError::ProviderMediaRef));
                    }
                    diagnostics.push(Diagnostic {
                        code: "dropped_assistant_media".to_string(),
                        message: "Only one prior assistant audio reference can be replayed"
                            .to_string(),
                    });
                }
                for part in &assistant.content {
                    match part {
                        AssistantPart::Reasoning(rp) => {
                            if protocol != Protocol::OpenAiChat
                                && protocol != Protocol::GoogleGenerativeAi
                                && rp.state.is_none()
                            {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(
                                        UnsupportedError::ReasoningStateMismatch {
                                            have: assistant.protocol,
                                            want: protocol,
                                        },
                                    ));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_reasoning_state".to_string(),
                                    message: "Target protocol requires replayable reasoning state"
                                        .to_string(),
                                });
                            }
                            if let Some(state) = &rp.state {
                                // The pi-messages codec retains the provider's
                                // opaque continuation payload as the canonical
                                // signature carriers and stamps the state with
                                // its own protocol/model, so this route must be
                                // able to replay those kinds (see
                                // `protocol::pi_messages::decode_stream_event`
                                // and `assistant_part_value`).
                                let kind_matches = matches!(
                                    (protocol, &state.kind),
                                    (
                                        Protocol::OpenAiResponses,
                                        crate::types::ReasoningStateKind::OpenAiReasoning { .. }
                                    ) | (
                                        Protocol::AnthropicMessages,
                                        crate::types::ReasoningStateKind::AnthropicSignature { .. }
                                    ) | (
                                        Protocol::AnthropicMessages | Protocol::BedrockConverse,
                                        crate::types::ReasoningStateKind::AnthropicRedacted { .. }
                                    ) | (
                                        Protocol::BedrockConverse,
                                        crate::types::ReasoningStateKind::AnthropicSignature { .. }
                                    ) | (
                                        Protocol::PiMessages,
                                        crate::types::ReasoningStateKind::AnthropicSignature { .. }
                                            | crate::types::ReasoningStateKind::AnthropicRedacted { .. }
                                    )
                                );
                                let empty_bedrock_signature = protocol == Protocol::BedrockConverse
                                    && matches!(&state.kind, crate::types::ReasoningStateKind::AnthropicSignature { signature } if signature.is_empty());
                                if empty_bedrock_signature
                                    || state.protocol != protocol
                                    || &state.model != target_model
                                    || !kind_matches
                                {
                                    if mode == CompatibilityMode::Strict {
                                        return Err(AiError::Unsupported(
                                            UnsupportedError::ReasoningStateMismatch {
                                                have: state.protocol,
                                                want: protocol,
                                            },
                                        ));
                                    }
                                    diagnostics.push(Diagnostic {
                                        code: "dropped_reasoning_state".to_string(),
                                        message:
                                            "Reasoning state protocol, kind, or model mismatch"
                                                .to_string(),
                                    });
                                }
                            }
                        }
                        AssistantPart::Media(Media::Audio(audio)) => {
                            let reference = match &audio.payload {
                                AudioPayload::ProviderRef(reference)
                                | AudioPayload::InlineWithProviderRef { reference, .. } => {
                                    Some(reference)
                                }
                                AudioPayload::Inline(_) => None,
                            };
                            let valid = protocol == Protocol::OpenAiChat
                                && reference.is_some_and(|reference| {
                                    reference.protocol == protocol
                                        && reference
                                            .expires_at
                                            .is_none_or(|expiry| expiry > SystemTime::now())
                                });
                            if !valid {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(
                                        UnsupportedError::ProviderMediaRef,
                                    ));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_assistant_media".to_string(),
                                    message: "Assistant audio is replayable only as a valid Chat provider reference".to_string(),
                                });
                            }
                        }
                        AssistantPart::Media(Media::Image(_)) => {
                            if mode == CompatibilityMode::Strict {
                                return Err(AiError::Unsupported(UnsupportedError::Image));
                            }
                            diagnostics.push(Diagnostic {
                                code: "dropped_assistant_media".to_string(),
                                message: "Assistant image replay has no v0.1 wire mapping"
                                    .to_string(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // 5. Tools capability check
    if !req.tools.is_empty() && !caps.tools {
        if mode == CompatibilityMode::Strict {
            return Err(AiError::Unsupported(UnsupportedError::Tools));
        } else {
            diagnostics.push(Diagnostic {
                code: "dropped_tools".to_string(),
                message: "Model does not support tool calling".to_string(),
            });
        }
    }

    // 6. Tool choice capability check
    if req.tool_choice != ToolChoice::Auto && req.tool_choice != ToolChoice::None && !caps.tools {
        if mode == CompatibilityMode::Strict {
            return Err(AiError::Unsupported(UnsupportedError::ToolChoice));
        } else {
            diagnostics.push(Diagnostic {
                code: "dropped_tool_choice".to_string(),
                message: "Model does not support tool calling choice".to_string(),
            });
        }
    }

    // 7. Legacy execution mode and reasoning control checks. Protocol codecs
    // cannot represent Pro mode; product layers must migrate it to Ultra after
    // checking the selected model's effort and delegation metadata.
    if req.reasoning_mode == ReasoningMode::Pro {
        if mode == CompatibilityMode::Strict {
            return Err(AiError::Unsupported(UnsupportedError::ReasoningMode));
        }
        diagnostics.push(Diagnostic {
            code: "ignored_reasoning_mode".to_string(),
            message: "Legacy Pro reasoning mode must be migrated to Ultra by the caller"
                .to_string(),
        });
    }

    validate_reasoning_selection(&req.reasoning, caps, protocol)?;
    let budget = match (&req.reasoning, &caps.reasoning) {
        (ReasoningConfig::Budget(budget), _) => Some(*budget),
        (ReasoningConfig::Effort(effort), Some(cap)) => cap.budget(*effort),
        _ => None,
    };
    if let Some(budget) = budget {
        if matches!(
            protocol,
            Protocol::AnthropicMessages | Protocol::BedrockConverse
        ) {
            if req.temperature.is_some_and(|value| value != 1.0) {
                return Err(AiError::Validation(ValidationError::InvalidTemperature));
            }
            if matches!(req.tool_choice, ToolChoice::Required | ToolChoice::Named(_)) {
                return Err(AiError::Unsupported(UnsupportedError::ToolChoice));
            }
        }
        let output = req.max_output_tokens.unwrap_or(limits.max_output_tokens);
        // At least one answer token remains; never consume the entire output
        // allowance with hidden thinking.
        if budget < 1024 || budget >= output {
            return Err(AiError::Validation(
                ValidationError::ReasoningBudgetOutOfRange,
            ));
        }
    }

    // 8. Structured output format check
    match &req.output_format {
        OutputFormat::JsonObject => {
            if !caps.structured_output
                || matches!(
                    protocol,
                    Protocol::AnthropicMessages | Protocol::BedrockConverse
                )
            {
                if mode == CompatibilityMode::Strict {
                    return Err(AiError::Unsupported(UnsupportedError::StructuredOutput));
                }
                diagnostics.push(Diagnostic {
                    code: "downgraded_output_format".to_string(),
                    message: "JSON object output is unsupported by the target model or protocol"
                        .to_string(),
                });
            }
        }
        OutputFormat::JsonSchema(ref s) => {
            if !caps.structured_output || protocol == Protocol::BedrockConverse {
                if mode == CompatibilityMode::Strict {
                    return Err(AiError::Unsupported(UnsupportedError::StructuredOutput));
                } else {
                    diagnostics.push(Diagnostic {
                        code: "downgraded_output_format".to_string(),
                        message: "Model does not support structured output formats".to_string(),
                    });
                }
            }
            // Schema must be a JSON object
            if !s.schema.is_object() {
                return Err(AiError::Validation(ValidationError::InvalidOutputSchema(
                    "JSON Schema must be a JSON object".to_string(),
                )));
            }
            // Name validation: 1-64 ASCII letters, digits, _, or -
            let is_valid_name = !s.name.is_empty()
                && s.name.len() <= 64
                && s.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !is_valid_name {
                return Err(AiError::Validation(
                    ValidationError::InvalidOutputFormatName(s.name.clone()),
                ));
            }
        }
        OutputFormat::Text => {}
    }

    // 9. Audio output capability and options checks
    if let OutputModalities::TextAndAudio(options) = &req.output_modalities {
        let voice = match &options.voice {
            AudioVoice::Named(voice) | AudioVoice::ProviderRef(voice) => voice,
        };
        if voice.trim().is_empty() {
            return Err(AiError::Validation(ValidationError::InvalidAudioVoice));
        }
        let supported = protocol == Protocol::OpenAiChat
            && caps
                .output_modalities
                .contains(crate::types::Modality::Audio);
        if !supported {
            if mode == CompatibilityMode::Strict {
                return Err(AiError::Unsupported(UnsupportedError::AudioOutput));
            }
            diagnostics.push(Diagnostic {
                code: "downgraded_audio_output".to_string(),
                message: "Audio output requires an audio-capable OpenAI Chat model".to_string(),
            });
        }
    }

    Ok(diagnostics)
}

#[cfg(test)]
mod tests;

/// The design §7 capability/validation table, exercised row-by-row in both
/// Strict and Lossy modes (plan Task 3.1 acceptance). Strict rows assert the
/// exact structured error; Lossy rows assert the exact diagnostic code.
#[cfg(test)]
mod matrix_tests;
