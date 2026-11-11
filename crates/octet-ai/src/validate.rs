//! Pure request validation and capability checks shared by every codec.

use crate::error::{AiError, Diagnostic, UnsupportedError, ValidationError};
use crate::request_history::{common_stamp_prefix, LentHistory};
use crate::transform::message_has_expiring_media;
use crate::types::{
    AssistantPart, AudioPayload, AudioVoice, Capabilities, ImageSource, Media, Message, ModelId,
    ModelLimits, OutputFormat, OutputModalities, Protocol, ReasoningConfig, ReasoningMode, Request,
    ToolCallId, ToolChoice, ToolResultPart, UserPart,
};
use crate::CompatibilityMode;
use std::borrow::Cow;
use std::collections::HashSet;
use std::time::SystemTime;

/// Everything besides the messages themselves that decides how a message is
/// paired and checked.
#[derive(Clone, PartialEq, Eq)]
struct MessageRules {
    protocol: Protocol,
    target_model: ModelId,
    mode: CompatibilityMode,
    caps: Capabilities,
}

/// Pairing facts and diagnostics for the leading messages of a history.
///
/// A full validation starts from an empty value. With a lent history the value
/// is kept between requests for the messages that did not change, so only the
/// new ones are walked. What is kept always ends where no tool call is waiting
/// for its result, and never covers an async call or a message whose verdict
/// depends on the clock.
#[derive(Default)]
pub(crate) struct ValidatedPrefix {
    rules: Option<MessageRules>,
    /// One stamp per message covered by the facts below.
    stamps: Vec<u64>,
    all_tool_calls: HashSet<ToolCallId>,
    completed_calls: HashSet<ToolCallId>,
    pairing_diagnostics: Vec<Diagnostic>,
    modality_diagnostics: Vec<Diagnostic>,
}

/// One reversible change to the kept pairing facts.
enum PairingChange {
    SawCall(ToolCallId),
    Completed(ToolCallId),
    Reopened(ToolCallId),
}

impl ValidatedPrefix {
    fn clear(&mut self) {
        self.stamps.clear();
        self.all_tool_calls.clear();
        self.completed_calls.clear();
        self.pairing_diagnostics.clear();
        self.modality_diagnostics.clear();
    }

    fn undo(&mut self, changes: Vec<PairingChange>) {
        for change in changes.into_iter().rev() {
            match change {
                PairingChange::SawCall(id) => {
                    self.all_tool_calls.remove(&id);
                }
                PairingChange::Completed(id) => {
                    self.completed_calls.remove(&id);
                }
                PairingChange::Reopened(id) => {
                    self.completed_calls.insert(id);
                }
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static VALIDATED_MESSAGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Messages paired by this thread since the last call.
#[cfg(test)]
pub(crate) fn take_validated_messages() -> usize {
    VALIDATED_MESSAGES.with(|count| count.replace(0))
}

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
    validate(req, caps, limits, protocol, target_model, mode, None)
}

/// [`validate_request`] for a request whose messages may be a lent history:
/// the leading messages already validated under the same rules are not walked
/// again. The result is the same either way.
pub(crate) fn validate_lent_request(
    req: &Request,
    caps: &Capabilities,
    limits: &ModelLimits,
    protocol: Protocol,
    target_model: &ModelId,
    mode: CompatibilityMode,
    lent: Option<LentHistory<'_>>,
) -> Result<Vec<Diagnostic>, AiError> {
    let resumed = lent.is_some();
    let result = validate(req, caps, limits, protocol, target_model, mode, lent);
    // Debug builds prove the resumed walk against a full one on every request,
    // so every test that sends a lent history checks the two stay equivalent.
    if cfg!(debug_assertions) && resumed {
        let outcome = |result: &Result<Vec<Diagnostic>, AiError>| match result {
            Ok(diagnostics) => {
                let mut lines: Vec<String> = diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                    .collect();
                // Several unanswered calls are reported in hash order.
                lines.sort();
                Ok(lines)
            }
            Err(error) => Err(format!("{error:?}")),
        };
        #[cfg(test)]
        let counted = VALIDATED_MESSAGES.with(|count| count.get());
        let full = validate(req, caps, limits, protocol, target_model, mode, None);
        #[cfg(test)]
        VALIDATED_MESSAGES.with(|count| count.set(counted));
        assert_eq!(
            outcome(&result),
            outcome(&full),
            "resumed validation must agree with a full validation"
        );
    }
    result
}

fn validate(
    req: &Request,
    caps: &Capabilities,
    limits: &ModelLimits,
    protocol: Protocol,
    target_model: &ModelId,
    mode: CompatibilityMode,
    lent: Option<LentHistory<'_>>,
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

    // 3 & 4. Tool results/calls pairing and modality/capability checks on messages
    validate_messages(
        req,
        caps,
        protocol,
        target_model,
        mode,
        lent,
        &mut diagnostics,
    )?;

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

/// Steps 3 and 4: tool-call pairing, then modality and capability checks.
///
/// Every message is paired before any is checked for modality, so the first
/// error and the order of the diagnostics do not depend on how many leading
/// messages were already validated for an earlier request.
fn validate_messages(
    req: &Request,
    caps: &Capabilities,
    protocol: Protocol,
    target_model: &ModelId,
    mode: CompatibilityMode,
    lent: Option<LentHistory<'_>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), AiError> {
    let mut fresh = ValidatedPrefix::default();
    let (stamps, prefix) = match lent {
        Some(LentHistory { stamps, caches }) => {
            let prefix = &mut caches.validated;
            let rules = MessageRules {
                protocol,
                target_model: target_model.clone(),
                mode,
                caps: caps.clone(),
            };
            if prefix.rules.as_ref() != Some(&rules) {
                prefix.clear();
                prefix.rules = Some(rules);
            } else if common_stamp_prefix(&prefix.stamps, stamps) != prefix.stamps.len() {
                // The pairing facts cannot be rewound to an earlier message.
                prefix.clear();
            }
            (Some(stamps), prefix)
        }
        None => (None, &mut fresh),
    };
    let start = prefix.stamps.len();
    let kept_pairing_diagnostics = prefix.pairing_diagnostics.len();
    let kept_modality_diagnostics = prefix.modality_diagnostics.len();

    let mut changes: Vec<PairingChange> = Vec::new();
    // Where the kept facts may end after this request, with the lengths that
    // go with that point.
    let mut settled = Settled {
        messages: start,
        changes: 0,
        pairing_diagnostics: kept_pairing_diagnostics,
        modality_diagnostics: kept_modality_diagnostics,
    };
    let paired = pair_tool_calls(
        req,
        caps,
        protocol,
        target_model,
        mode,
        prefix,
        start,
        &mut changes,
        &mut settled,
    );
    let checked = paired.and_then(|()| {
        for (index, msg) in req.messages.iter().enumerate().skip(start) {
            if index == settled.messages {
                settled.modality_diagnostics = prefix.modality_diagnostics.len();
            }
            validate_message_modality(
                msg,
                caps,
                protocol,
                target_model,
                mode,
                &mut prefix.modality_diagnostics,
            )?;
        }
        if settled.messages == req.messages.len() {
            settled.modality_diagnostics = prefix.modality_diagnostics.len();
        }
        Ok(())
    });

    if checked.is_ok() {
        diagnostics.extend(prefix.pairing_diagnostics.iter().cloned());
        diagnostics.extend(prefix.modality_diagnostics.iter().cloned());
    } else {
        settled = Settled {
            messages: start,
            changes: 0,
            pairing_diagnostics: kept_pairing_diagnostics,
            modality_diagnostics: kept_modality_diagnostics,
        };
    }
    if let Some(stamps) = stamps {
        // Keep the facts up to the settled point and take back the rest.
        prefix.undo(changes.split_off(settled.changes));
        prefix
            .pairing_diagnostics
            .truncate(settled.pairing_diagnostics);
        prefix
            .modality_diagnostics
            .truncate(settled.modality_diagnostics);
        prefix
            .stamps
            .extend_from_slice(&stamps[start..settled.messages]);
    }
    checked
}

/// The furthest point up to which pairing facts can be kept for a later
/// request, and how much of each running list belongs to it.
struct Settled {
    messages: usize,
    changes: usize,
    pairing_diagnostics: usize,
    modality_diagnostics: usize,
}

/// Step 3 for `req.messages[start..]`, continuing from the facts in `prefix`.
#[allow(clippy::too_many_arguments)]
fn pair_tool_calls(
    req: &Request,
    caps: &Capabilities,
    protocol: Protocol,
    target_model: &ModelId,
    mode: CompatibilityMode,
    prefix: &mut ValidatedPrefix,
    start: usize,
    changes: &mut Vec<PairingChange>,
    settled: &mut Settled,
) -> Result<(), AiError> {
    // Async-qualified Responses histories need globally unique call identities:
    // their results may arrive across turns. Other codecs may generate fallback
    // IDs per response, so a completed synchronous identity can be reused.
    let global_call_ids =
        protocol == Protocol::OpenAiResponses && caps.responses_features.async_tools;
    let results_before_next_turn = protocol == Protocol::AnthropicMessages
        || protocol == Protocol::OpenAiResponses
        || protocol == Protocol::BedrockConverse
        || protocol == Protocol::GoogleGenerativeAi;
    // A completed historical call is replay, not a request to execute its old
    // tool again. Only an async call asks whether its result exists anywhere in
    // the history, so the index is built when the first one is met; the ordered
    // walk below still rejects orphans, duplicates and reused identities.
    let mut historical_results: Option<HashSet<&ToolCallId>> = None;
    let mut pending_calls: HashSet<ToolCallId> = HashSet::new();
    let mut pending_async: HashSet<ToolCallId> = HashSet::new();
    // Kept facts must not depend on the clock or on a later message.
    let mut can_settle = true;

    for (index, msg) in req.messages.iter().enumerate().skip(start) {
        #[cfg(test)]
        VALIDATED_MESSAGES.with(|count| count.set(count.get() + 1));
        match msg {
            Message::Assistant(ref assistant) => {
                // Google, Anthropic, and OpenAI Responses require function/tool
                // responses before another assistant turn.
                if results_before_next_turn && !pending_calls.is_empty() {
                    // In Strict mode, this is a ValidationError.
                    // In Lossy mode, we emit a diagnostic and expect the codec to insert a synthetic error.
                    for call_id in pending_calls.difference(&pending_async) {
                        if mode == CompatibilityMode::Strict {
                            return Err(AiError::Validation(ValidationError::MissingToolResult(
                                call_id.clone(),
                            )));
                        } else {
                            prefix.pairing_diagnostics.push(Diagnostic {
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
                        let first_use = prefix.all_tool_calls.insert(tc.id.clone());
                        if first_use {
                            changes.push(PairingChange::SawCall(tc.id.clone()));
                        } else if global_call_ids || pending_calls.contains(&tc.id) {
                            return Err(crate::error::ConfigError::Parse(
                                "duplicate tool call ID".into(),
                            )
                            .into());
                        }
                        if !global_call_ids && prefix.completed_calls.remove(&tc.id) {
                            changes.push(PairingChange::Reopened(tc.id.clone()));
                        }
                        if tc.async_execution {
                            can_settle = false;
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
                            let historical_results = historical_results.get_or_insert_with(|| {
                                req.messages
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
                                    .collect()
                            });
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
                        if !prefix.all_tool_calls.contains(&tr.tool_call_id) {
                            return Err(AiError::Validation(ValidationError::OrphanToolResult(
                                tr.tool_call_id.clone(),
                            )));
                        }
                        if !prefix.completed_calls.insert(tr.tool_call_id.clone()) {
                            return Err(crate::error::ConfigError::Parse(
                                "duplicate tool result".into(),
                            )
                            .into());
                        }
                        changes.push(PairingChange::Completed(tr.tool_call_id.clone()));
                        pending_calls.remove(&tr.tool_call_id);
                        pending_async.remove(&tr.tool_call_id);
                    }
                }
            }
        }
        can_settle &= !message_has_expiring_media(msg);
        if can_settle && pending_calls.is_empty() {
            *settled = Settled {
                messages: index + 1,
                changes: changes.len(),
                pairing_diagnostics: prefix.pairing_diagnostics.len(),
                modality_diagnostics: settled.modality_diagnostics,
            };
        }
    }

    if results_before_next_turn && !pending_calls.is_empty() {
        for call_id in pending_calls.difference(&pending_async) {
            if mode == CompatibilityMode::Strict {
                return Err(AiError::Validation(ValidationError::MissingToolResult(
                    call_id.clone(),
                )));
            }
            prefix.pairing_diagnostics.push(Diagnostic {
                code: "missing_tool_result".to_string(),
                message: format!("Missing tool result for call ID {:?}", call_id),
            });
        }
    }
    Ok(())
}

fn validate_message_modality(
    msg: &Message,
    caps: &Capabilities,
    protocol: Protocol,
    target_model: &ModelId,
    mode: CompatibilityMode,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), AiError> {
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
                        if protocol.audio_input_formats().is_empty() {
                            if mode == CompatibilityMode::Strict {
                                return Err(AiError::Unsupported(UnsupportedError::Audio));
                            }
                            diagnostics.push(Diagnostic {
                                code: "dropped_audio".to_string(),
                                message: format!("Audio input is unsupported through {protocol:?}"),
                            });
                        }
                        if !caps
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
                                message: "Bare audio references cannot be used as Chat user input"
                                    .to_string(),
                            });
                        }

                        if protocol == Protocol::GoogleGenerativeAi
                            && !matches!(audio.payload, AudioPayload::Inline(_))
                        {
                            if mode == CompatibilityMode::Strict {
                                return Err(AiError::Unsupported(
                                    UnsupportedError::ProviderMediaRef,
                                ));
                            }
                            diagnostics.push(Diagnostic {
                                    code: "dropped_provider_media_ref".to_string(),
                                    message: "Google audio input requires inline bytes; provider references are not forwarded".to_string(),
                                });
                        }
                        if !protocol.audio_input_formats().is_empty()
                            && !protocol.audio_input_formats().contains(&audio.format)
                        {
                            if mode == CompatibilityMode::Strict {
                                return Err(AiError::Unsupported(UnsupportedError::Audio));
                            }
                            diagnostics.push(Diagnostic {
                                code: "dropped_audio_format".to_string(),
                                message: format!(
                                    "{protocol:?} does not support {:?} audio input",
                                    audio.format
                                ),
                            });
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
                    UserPart::Media(Media::Document(_)) => {
                        let supported = protocol.supports_document_input()
                            && caps.input_modalities.contains(crate::types::Modality::Pdf);
                        if !supported {
                            if mode == CompatibilityMode::Strict {
                                return Err(AiError::Unsupported(UnsupportedError::Document));
                            }
                            diagnostics.push(Diagnostic {
                                code: "dropped_document".to_string(),
                                message: format!(
                                    "PDF input is unsupported by this model through {protocol:?}"
                                ),
                            });
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
                                                && reference
                                                    .expires_at
                                                    .is_none_or(|expiry| expiry > SystemTime::now())
                                        }
                                        _ => true,
                                    },
                                    Protocol::AnthropicMessages => {
                                        matches!(
                                            &image.source,
                                            ImageSource::Inline(_) | ImageSource::Url(_)
                                        )
                                    }
                                    // Gemini's functionResponse carries text only; the
                                    // codec sends inline bytes as `inlineData` parts
                                    // beside the turn's function responses, exactly as
                                    // it sends agent-placed adjacent media.
                                    Protocol::GoogleGenerativeAi => {
                                        caps.input_modalities
                                            .contains(crate::types::Modality::Image)
                                            && image.media_type.is_some()
                                            && matches!(&image.source, ImageSource::Inline(_))
                                    }
                                    // Bedrock Converse has no documented tool-result
                                    // media mapping.
                                    Protocol::BedrockConverse => false,
                                    // No evidenced native Conversations tool-result media schema.
                                    Protocol::MistralConversations => false,
                                    // Pi's `ImageContent` block carries inline bytes only;
                                    // a URL or provider reference has no mapping.
                                    // The extension's own stream receives Pi's inline
                                    // `ImageContent`, as Pi Messages does.
                                    Protocol::ExtensionStream | Protocol::PiMessages => {
                                        matches!(&image.source, ImageSource::Inline(_))
                                    }
                                },
                                // Documents travel in user content only; no tool-result
                                // document mapping is implemented yet.
                                ToolResultPart::Media(Media::Document(_)) => false,
                                ToolResultPart::Media(Media::Audio(audio)) => {
                                    protocol == Protocol::GoogleGenerativeAi
                                        && caps
                                            .input_modalities
                                            .contains(crate::types::Modality::Audio)
                                        && protocol.audio_input_formats().contains(&audio.format)
                                        && matches!(audio.payload, AudioPayload::Inline(_))
                                }
                            };
                            if !supported {
                                if mode == CompatibilityMode::Strict {
                                    return Err(AiError::Unsupported(
                                        UnsupportedError::ToolResultMedia,
                                    ));
                                }
                                diagnostics.push(Diagnostic {
                                    code: "dropped_tool_result_media".to_string(),
                                    message:
                                        "Tool-result media has no mapping on the target protocol"
                                            .to_string(),
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
                    message: "Only one prior assistant audio reference can be replayed".to_string(),
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
                                ) | (
                                    Protocol::OpenAiChat,
                                    crate::types::ReasoningStateKind::ChatReasoningDetails { .. }
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
                                    message: "Reasoning state protocol, kind, or model mismatch"
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
                            message: "Assistant image replay has no v0.1 wire mapping".to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// The design §7 capability/validation table, exercised row-by-row in both
/// Strict and Lossy modes (plan Task 3.1 acceptance). Strict rows assert the
/// exact structured error; Lossy rows assert the exact diagnostic code.
#[cfg(test)]
mod matrix_tests;
