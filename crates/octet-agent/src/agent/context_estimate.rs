//! Request token and context estimation, Responses replay and reasoning options.

use super::*;

pub(super) fn model_visible_branch_entries(session: &Session) -> Vec<&crate::session::Entry> {
    let branch = active_branch_entries(session);
    let first_kept = branch.iter().rev().find_map(|entry| match &entry.value {
        EntryValue::Compaction { first_kept, .. } => Some(first_kept),
        _ => None,
    });
    let start = first_kept
        .and_then(|first_kept| branch.iter().position(|entry| &entry.id == first_kept))
        .unwrap_or_default();
    branch.into_iter().skip(start).collect()
}

pub(super) fn previous_message_is_user(session: &Session, entry: &crate::session::Entry) -> bool {
    let mut cursor = entry.parent.clone();
    while let Some(id) = cursor {
        let Some(previous) = session.entry(&id) else {
            return false;
        };
        match &previous.value {
            EntryValue::Message(Message::User(user)) => return !user.content.is_empty(),
            EntryValue::Message(Message::Assistant(_)) => return false,
            EntryValue::BranchSummary { .. } => return true,
            EntryValue::Compaction { .. }
            | EntryValue::ResponsesTurn { .. }
            | EntryValue::ResponsesCompaction { .. }
            | EntryValue::ResponsesReasoning { .. }
            | EntryValue::ResponsesSteering { .. }
            | EntryValue::Config { .. }
            | EntryValue::PromptTemplateSelected { .. }
            | EntryValue::SkillActivated { .. }
            | EntryValue::SkillResourceRead { .. }
            | EntryValue::SkillDeactivated { .. } => cursor = previous.parent.clone(),
        }
    }
    false
}

pub(super) fn turn_starts(session: &Session) -> Vec<EntryId> {
    model_visible_branch_entries(session)
        .into_iter()
        .filter_map(|entry| {
            if !matches!(&entry.value, EntryValue::Message(Message::Assistant(_)))
                || !previous_message_is_user(session, entry)
            {
                return None;
            }
            // Every assistant whose previous durable message is a user message
            // is a potential episode boundary. Non-message compaction/config/
            // skill markers may sit between them and must not hide the turn.
            Some(entry.id.clone())
        })
        .collect()
}

#[derive(Default)]
pub(super) struct CountingWriter(pub(super) u64);

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) const ESTIMATED_IMAGE_TOKENS: u64 = 1_600;

pub(super) const ESTIMATED_AUDIO_TOKENS: u64 = 8_000;

pub(super) fn inline_media_payload_bytes(media: &Media) -> u64 {
    let raw_bytes = match media {
        Media::Image(image) => match &image.source {
            ImageSource::Inline(data) => data.len() as u64,
            ImageSource::Url(_) | ImageSource::ProviderRef(_) => 0,
        },
        Media::Audio(audio) => match &audio.payload {
            AudioPayload::Inline(data) | AudioPayload::InlineWithProviderRef { data, .. } => {
                data.len() as u64
            }
            AudioPayload::ProviderRef(_) => 0,
        },
    };
    // Inline media's serde representation is one padded base64 string. The
    // surrounding quotes and variant metadata remain in the structural byte
    // estimate; remove only payload characters before adding semantic tokens.
    raw_bytes.div_ceil(3).saturating_mul(4)
}

pub(super) fn media_tokens(media: &Media) -> u64 {
    match media {
        Media::Image(_) => ESTIMATED_IMAGE_TOKENS,
        Media::Audio(_) => ESTIMATED_AUDIO_TOKENS,
    }
}

pub(super) fn request_media_adjustment(messages: &[Message]) -> (u64, u64) {
    let mut inline_payload_bytes = 0u64;
    let mut semantic_tokens = 0u64;
    let mut observe = |media: &Media| {
        inline_payload_bytes =
            inline_payload_bytes.saturating_add(inline_media_payload_bytes(media));
        semantic_tokens = semantic_tokens.saturating_add(media_tokens(media));
    };
    for message in messages {
        match message {
            Message::User(user) => {
                for part in &user.content {
                    match part {
                        UserPart::Media(media) => observe(media),
                        UserPart::ToolResult(result) => {
                            for part in &result.content {
                                if let ToolResultPart::Media(media) = part {
                                    observe(media);
                                }
                            }
                        }
                        UserPart::Text(_) => {}
                    }
                }
            }
            Message::Assistant(assistant) => {
                for part in &assistant.content {
                    if let AssistantPart::Media(media) = part {
                        observe(media);
                    }
                }
            }
        }
    }
    (inline_payload_bytes, semantic_tokens)
}

pub(super) fn responses_replay_media_adjustment(replay: &[ResponsesReplayItem]) -> (u64, u64) {
    let mut inline_payload_bytes = 0u64;
    let mut semantic_tokens = 0u64;
    let mut observe = |media: &Media| {
        inline_payload_bytes =
            inline_payload_bytes.saturating_add(inline_media_payload_bytes(media));
        semantic_tokens = semantic_tokens.saturating_add(media_tokens(media));
    };
    for item in replay {
        let ResponsesReplayItem::User(user) = item else {
            continue;
        };
        for part in &user.content {
            match part {
                UserPart::Media(media) => observe(media),
                UserPart::ToolResult(result) => {
                    for part in &result.content {
                        if let ToolResultPart::Media(media) = part {
                            observe(media);
                        }
                    }
                }
                UserPart::Text(_) => {}
            }
        }
    }
    (inline_payload_bytes, semantic_tokens)
}

pub(super) fn tool_schema_bytes(tools: &[ToolDef]) -> usize {
    let mut bytes = CountingWriter::default();
    // ToolDef is internally constructed from serializable strings and JSON
    // values, so serialization failure would violate the provider-request
    // invariant rather than being a recoverable user boundary.
    serde_json::to_writer(&mut bytes, tools).expect("ToolDef serializes");
    usize::try_from(bytes.0).unwrap_or(usize::MAX)
}

pub(super) fn require_tool_schema_budget(
    tools: &[ToolDef],
    max_bytes: usize,
) -> Result<(), AgentError> {
    // A zero budget intentionally permits `[]`: it advertises no callable
    // schema, even though JSON's empty-array delimiters occupy two wire bytes.
    if tools.is_empty() {
        return Ok(());
    }
    let actual_bytes = tool_schema_bytes(tools);
    if actual_bytes > max_bytes {
        return Err(AgentError::ToolSchemaBudgetExceeded {
            actual_bytes,
            tool_count: tools.len(),
            max_bytes,
        });
    }
    Ok(())
}

pub(super) fn validate_compaction_summary_part(summary: &str) -> Result<(), AgentError> {
    if summary.trim().is_empty() {
        return Err(AgentError::IncompleteResponse {
            stop_reason: "compaction summary was empty or whitespace-only".to_owned(),
        });
    }
    if summary.len() > MAX_COMPACTION_HANDOFF_BYTES {
        return Err(AgentError::IncompleteResponse {
            stop_reason: format!(
                "compaction summary exceeded the {MAX_COMPACTION_HANDOFF_BYTES}-byte handoff limit"
            ),
        });
    }
    Ok(())
}

pub(super) fn append_compaction_turn_prefix(
    summary: &mut String,
    prefix_summary: &str,
) -> Result<(), AgentError> {
    validate_compaction_summary_part(prefix_summary)?;
    summary.push_str("\n\n---\n\n**Turn Context (split turn):**\n\n");
    summary.push_str(prefix_summary);
    Ok(())
}

pub(super) fn finish_validated_compaction_handoff(
    summary: String,
    details: &crate::compaction::CompactionDetails,
) -> Result<String, AgentError> {
    validate_compaction_summary_part(&summary)?;
    finish_handoff_bounded(summary, details, MAX_COMPACTION_HANDOFF_BYTES).ok_or_else(|| {
        AgentError::IncompleteResponse {
            stop_reason: format!(
                "compaction summary exceeded the {MAX_COMPACTION_HANDOFF_BYTES}-byte handoff limit"
            ),
        }
    })
}

pub(super) fn estimate_request_tokens(
    system: &str,
    messages: &[Message],
    tools: &[ToolDef],
) -> u64 {
    let mut bytes = CountingWriter::default();
    if serde_json::to_writer(&mut bytes, &(system, messages, tools)).is_err() {
        return 64;
    }
    let (inline_payload_bytes, semantic_tokens) = request_media_adjustment(messages);
    bytes
        .0
        .saturating_sub(inline_payload_bytes)
        .div_ceil(4)
        .saturating_add(semantic_tokens)
        .saturating_add(64)
}

pub(super) struct ExactResponsesReplay {
    pub(super) input: ResponsesInput,
    pub(super) replay: Arc<Vec<ResponsesReplayItem>>,
    pub(super) instructions: Option<String>,
}

pub(super) fn exact_responses_replay(
    session: &Session,
    model: &Model,
    system: &str,
) -> Option<ExactResponsesReplay> {
    if model.spec.protocol != Protocol::OpenAiResponses {
        return None;
    }
    let replay = session
        .responses_replay_snapshot(&model.endpoint.id, &model.spec.id)
        .ok()
        .flatten()?;
    let instructions = matches!(replay.first(), Some(ResponsesReplayItem::Compacted(_)))
        .then(|| system.to_owned())
        .filter(|system| !system.is_empty());
    let input = octet_ai::responses::encode_responses_replay(
        model,
        (!system.is_empty()).then_some(system),
        &replay,
    )
    .ok()?;
    Some(ExactResponsesReplay {
        input,
        replay,
        instructions,
    })
}

pub(super) fn current_head_is_native_checkpoint(session: &Session, model: &Model) -> bool {
    session
        .head_ref()
        .and_then(|head| session.entry(head))
        .is_some_and(|entry| {
            matches!(
                &entry.value,
                EntryValue::ResponsesCompaction {
                    endpoint,
                    model: recorded_model,
                    ..
                } if endpoint == &model.endpoint.id && recorded_model == &model.spec.id
            )
        })
}

pub(super) fn validate_native_compact_output(
    output: &octet_ai::ResponsesOutput,
) -> Result<(), AgentError> {
    if output.has_valid_compaction() {
        Ok(())
    } else {
        Err(AiError::Decode(DecodeError::Json(
            "Responses compact output did not contain exactly one complete compaction item"
                .to_owned(),
        ))
        .into())
    }
}

pub(super) fn validate_reasoning_update(
    model: &Model,
    reasoning: &ReasoningConfig,
) -> Result<(), AgentError> {
    let update = octet_ai::ResponsesConfigurationUpdate {
        reasoning: reasoning.clone(),
    };
    octet_ai::responses::validate_responses_input(
        model,
        &ResponsesInput::new(vec![update.to_item()]),
        reasoning,
        false,
    )?;
    Ok(())
}

pub(super) fn require_ultra_observation(
    reasoning: &ReasoningConfig,
    observed: bool,
) -> Result<(), AgentError> {
    if *reasoning == ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra) && !observed {
        return Err(AgentError::Delegation(
            "Ultra requires an enabled child-session observation runtime".into(),
        ));
    }
    Ok(())
}

pub(super) fn persist_reasoning_selection(
    session: &mut Session,
    model: &Model,
    selection: &ReasoningConfig,
) -> Result<(), AgentError> {
    let state = session.responses_reasoning(&model.endpoint.id, &model.spec.id)?;
    if state
        .as_ref()
        .is_some_and(|(_, effective)| effective == selection)
    {
        return Ok(());
    }
    let ultra = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra);
    // Ultra is host orchestration, never a provider configuration_update.
    // Crossing this boundary starts a new baseline in the same conversation;
    // replay drops superseded effort updates, not messages or opaque outputs.
    let rebase = selection == &ultra
        || state
            .as_ref()
            .is_some_and(|(_, effective)| effective == &ultra);
    let (baseline, update) = match state {
        Some((baseline, _)) if !rebase => {
            validate_reasoning_update(model, selection)?;
            (
                baseline,
                Some(octet_ai::ResponsesConfigurationUpdate {
                    reasoning: selection.clone(),
                }),
            )
        }
        _ => {
            octet_ai::responses::validate_responses_input(
                model,
                &ResponsesInput::default(),
                selection,
                false,
            )?;
            (selection.clone(), None)
        }
    };
    session.append(EntryValue::ResponsesReasoning {
        endpoint: model.endpoint.id.clone(),
        model: model.spec.id.clone(),
        baseline,
        update,
    })?;
    Ok(())
}

pub(super) fn durable_responses_options(
    session: &Session,
    model: &Model,
    system: &str,
    requested_service_tier: Option<ServiceTier>,
) -> Result<Option<ResponsesOptions>, AgentError> {
    let service_tier = resolve_service_tier(model, requested_service_tier)?;
    let replay = exact_responses_replay(session, model, system);
    // A complete opaque replay carries ordered reasoning updates. Without one,
    // the codec re-encodes canonical history and request_reasoning_for_replay
    // selects the effective effort instead of replaying a stale baseline.
    match (replay, service_tier) {
        // No route-affine local window and no requested tier: keep the
        // historical `None`, which makes the codec fall back to canonical
        // replay with no Responses options at all.
        (None, None) => Ok(None),
        (replay, service_tier) => {
            // A requested tier rides on Responses options even when the session
            // has no window yet: the codec then replays canonically exactly as
            // it would without options, so the tier is never silently dropped.
            let options = replay.map_or_else(ResponsesOptions::default, |exact| {
                ResponsesOptions::full_replay(exact.input)
            });
            Ok(Some(match service_tier {
                Some(tier) => options.with_service_tier(tier),
                None => options,
            }))
        }
    }
}

pub(super) fn request_reasoning_for_replay(
    session: &Session,
    model: &Model,
    responses: Option<&ResponsesOptions>,
    selection: &ReasoningConfig,
) -> Result<ReasoningConfig, AgentError> {
    // Only complete route-affine replay retains the chronological updates
    // that override the pinned baseline. Canonical fallback has no updates,
    // so put the effective selection on the request itself.
    let state = session.responses_reasoning(&model.endpoint.id, &model.spec.id)?;
    let exact = responses.is_some_and(|options| options.input.is_some());
    Ok(match state {
        Some((baseline, _)) if exact => baseline,
        Some((_, effective)) => effective,
        None => selection.clone(),
    })
}

/// Validates a requested service tier against the route that will carry it.
///
/// The tier changes provider routing and billing, so it is sent only to an
/// endpoint whose declared runtime profile accepts the Responses `service_tier`
/// field ([`octet_ai::ResponsesRuntimeProfile::accepts_service_tier`], the Codex
/// subscription runtime today). Every other route — and every non-Responses
/// protocol, where the field could not be emitted at all — fails closed with the
/// codec's typed unsupported error instead of silently dropping the control.
pub(super) fn resolve_service_tier(
    model: &Model,
    requested: Option<ServiceTier>,
) -> Result<Option<ServiceTier>, AgentError> {
    let Some(tier) = requested else {
        return Ok(None);
    };
    if model.spec.protocol != Protocol::OpenAiResponses
        || !model
            .endpoint
            .runtime
            .responses_profile
            .accepts_service_tier()
    {
        return Err(AiError::Unsupported(octet_ai::UnsupportedError::ServiceTier).into());
    }
    Ok(Some(tier))
}

pub(super) fn native_responses_options(
    session: &Session,
    model: &Model,
    system: &str,
    requested_service_tier: Option<ServiceTier>,
) -> Result<ResponsesOptions, AgentError> {
    let service_tier = resolve_service_tier(model, requested_service_tier)?;
    let replay = session
        .responses_replay_snapshot(&model.endpoint.id, &model.spec.id)?
        .ok_or_else(|| {
            AgentError::InvalidCompactionPolicy(
                "native Responses mode requires complete route-affine opaque replay before every provider request"
                    .to_owned(),
            )
        })?;
    let options = ResponsesOptions::full_replay(octet_ai::responses::encode_responses_replay(
        model,
        (!system.is_empty()).then_some(system),
        &replay,
    )?);
    Ok(match service_tier {
        Some(tier) => options.with_service_tier(tier),
        None => options,
    })
}

pub(super) fn estimate_responses_request_tokens(
    input: &ResponsesInput,
    replay: &[ResponsesReplayItem],
    tools: &[ToolDef],
    instructions: Option<&str>,
) -> u64 {
    let mut bytes = CountingWriter::default();
    if serde_json::to_writer(&mut bytes, &(input, tools, instructions)).is_err() {
        return 64;
    }
    let (inline_payload_bytes, semantic_tokens) = responses_replay_media_adjustment(replay);
    // Opaque replay and native compact checkpoints are estimated from exactly
    // what will be serialized, never from canonical history they replaced.
    // Only canonical replay media is converted from base64 bytes to a semantic
    // modality estimate; opaque provider output remains fully byte-counted.
    bytes
        .0
        .saturating_sub(inline_payload_bytes)
        .div_ceil(4)
        .saturating_add(semantic_tokens)
        .saturating_add(64)
}

pub(super) fn estimate_compact_request_tokens(
    request: &ResponsesCompactRequest,
    replay: &[ResponsesReplayItem],
) -> u64 {
    let mut bytes = CountingWriter::default();
    if serde_json::to_writer(&mut bytes, request).is_err() {
        return 64;
    }
    let (inline_payload_bytes, semantic_tokens) = responses_replay_media_adjustment(replay);
    bytes
        .0
        .saturating_sub(inline_payload_bytes)
        .div_ceil(4)
        .saturating_add(semantic_tokens)
        .saturating_add(64)
}

pub(super) fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    let mut bytes = CountingWriter::default();
    if serde_json::to_writer(&mut bytes, messages).is_err() {
        return 64;
    }
    let (inline_payload_bytes, semantic_tokens) = request_media_adjustment(messages);
    bytes
        .0
        .saturating_sub(inline_payload_bytes)
        .div_ceil(4)
        .saturating_add(semantic_tokens)
        .saturating_add(16)
}

pub(super) fn usage_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input_tokens
            .saturating_add(usage.cache_read_tokens)
            .saturating_add(usage.cache_write_tokens)
            .saturating_add(usage.output_tokens)
    }
}

/// Provider usage is the best available tokenizer measurement of the prefix
/// through its assistant response. Add structural estimates only for messages
/// persisted after that response. Usage from before the latest compaction or
/// from a different route/model is stale and must not retrigger compaction.
pub(super) fn provider_context_estimate(session: &Session, model: &Model) -> Option<u64> {
    // Imported/legacy sessions may have history but no provider measurements.
    if session.usage_records().is_empty() {
        return None;
    }
    // Only the suffix after the newest usable measurement contributes. Walking
    // backwards avoids allocating/copying the entire active branch on startup,
    // context inspection, and capacity-cache rebuilds in long sessions.
    // Advance through the ledger at most once. Index only usable records for
    // this route/model, newest first, while retaining the constant-work common
    // case where the head assistant has the newest measurement.
    let mut usage_records = session.usage_records().iter().rev();
    let mut usage_by_assistant = HashMap::new();
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        #[cfg(test)]
        PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(visits.get() + 1));
        let entry = session.entry(id)?;
        match &entry.value {
            EntryValue::Compaction { .. }
            | EntryValue::ResponsesCompaction { .. }
            | EntryValue::BranchSummary { .. } => break,
            EntryValue::Message(message) => {
                if matches!(message, Message::Assistant(_)) {
                    let measured = usage_by_assistant.get(&entry.id).copied().or_else(|| {
                        for record in usage_records.by_ref() {
                            #[cfg(test)]
                            PROVIDER_CONTEXT_USAGE_VISITS
                                .with(|visits| visits.set(visits.get() + 1));
                            let crate::session::UsageRecordKind::AssistantTurn { assistant } =
                                &record.kind
                            else {
                                continue;
                            };
                            let tokens = usage_context_tokens(&record.usage);
                            if record.endpoint.as_ref() != Some(&model.endpoint.id)
                                || record.model.as_ref() != Some(&model.spec.id)
                                || tokens == 0
                            {
                                continue;
                            }
                            if assistant == &entry.id {
                                return Some(tokens);
                            }
                            usage_by_assistant.entry(assistant).or_insert(tokens);
                        }
                        None
                    });
                    if let Some(tokens) = measured {
                        // Estimate only after finding usable usage. Sessions with
                        // no measurement must not serialize their entire history.
                        let mut trailing = 0u64;
                        let mut tail = session.head_ref();
                        while let Some(tail_id) = tail.filter(|tail_id| *tail_id != id) {
                            #[cfg(test)]
                            PROVIDER_CONTEXT_ENTRY_VISITS
                                .with(|visits| visits.set(visits.get() + 1));
                            let tail_entry = session.entry(tail_id)?;
                            if let EntryValue::Message(message) = &tail_entry.value {
                                trailing = trailing.saturating_add(estimate_messages_tokens(
                                    std::slice::from_ref(message),
                                ));
                            }
                            tail = tail_entry.parent.as_ref();
                        }
                        return Some(tokens.saturating_add(trailing));
                    }
                }
            }
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    None
}

pub(super) fn reconcile_context_estimate(
    session: &Session,
    model: &Model,
    system: &str,
    messages: &[Message],
    tools: &[ToolDef],
) -> RequestContextEstimate {
    let structural_tokens = exact_responses_replay(session, model, system).map_or_else(
        || estimate_request_tokens(system, messages, tools),
        |exact| {
            estimate_responses_request_tokens(
                &exact.input,
                &exact.replay,
                tools,
                exact.instructions.as_deref(),
            )
        },
    );
    let provider_tokens = provider_context_estimate(session, model);
    let input_tokens = provider_tokens.map_or(structural_tokens, |provider| {
        structural_tokens.max(provider)
    });
    RequestContextEstimate {
        structural_tokens,
        provider_tokens,
        input_tokens,
    }
}

pub(super) fn serialized_tokens<T: Serialize>(value: &T) -> u64 {
    let mut bytes = CountingWriter::default();
    if serde_json::to_writer(&mut bytes, value).is_err() {
        return 0;
    }
    bytes.0.div_ceil(4)
}

pub(super) fn visible_compaction_summary(session: &Session) -> Option<String> {
    active_branch_entries(session)
        .into_iter()
        .rev()
        .find_map(|entry| {
            let EntryValue::Compaction { summary, .. } = &entry.value else {
                return None;
            };
            Some(format!("[summary of earlier conversation]\n{summary}"))
        })
}

pub(super) fn context_breakdown(
    session: &Session,
    model: &Model,
    system: &str,
    messages: &[Message],
    tools: &[ToolDef],
) -> ContextBreakdown {
    let estimate = reconcile_context_estimate(session, model, system, messages, tools);
    let mut remaining_structural = estimate.structural_tokens;
    let mut take = |requested: u64| {
        let accepted = requested.min(remaining_structural);
        remaining_structural = remaining_structural.saturating_sub(accepted);
        accepted
    };

    let instruction_tokens = take(serialized_tokens(&system));
    let summary = visible_compaction_summary(session);
    let mut conversation_tokens = 0u64;
    let mut tool_result_tokens = 0u64;
    let mut attachment_tokens = 0u64;
    let mut compaction_summary_tokens = 0u64;

    for message in messages {
        let message_slice = std::slice::from_ref(message);
        let mut bytes = CountingWriter::default();
        if serde_json::to_writer(&mut bytes, message).is_err() {
            continue;
        }
        let (inline_payload_bytes, semantic_media_tokens) = request_media_adjustment(message_slice);
        let media_tokens = take(semantic_media_tokens);
        attachment_tokens = attachment_tokens.saturating_add(media_tokens);
        let non_media_tokens = bytes.0.saturating_sub(inline_payload_bytes).div_ceil(4);
        let accepted = take(non_media_tokens);
        let is_tool = match message {
            Message::User(user) => user
                .content
                .iter()
                .any(|part| matches!(part, UserPart::ToolResult(_))),
            Message::Assistant(assistant) => assistant
                .content
                .iter()
                .any(|part| matches!(part, AssistantPart::ToolCall(_))),
        };
        let is_summary = summary.as_ref().is_some_and(|summary| {
            matches!(
                message,
                Message::User(user)
                    if user.content.len() == 1
                        && matches!(&user.content[0], UserPart::Text(text) if text == summary)
            )
        });
        if is_summary {
            compaction_summary_tokens = compaction_summary_tokens.saturating_add(accepted);
        } else if is_tool {
            tool_result_tokens = tool_result_tokens.saturating_add(accepted);
        } else {
            conversation_tokens = conversation_tokens.saturating_add(accepted);
        }
    }

    // Whatever remains in the serializer-derived total is request framing,
    // tool definitions, and provider/runtime system structure. Provider usage
    // above that structural estimate is authoritative but intentionally left in
    // `other` rather than assigned with fabricated precision.
    let system_tokens = remaining_structural;
    let other_tokens = estimate
        .input_tokens
        .saturating_sub(estimate.structural_tokens);
    let total_tokens = system_tokens
        .saturating_add(instruction_tokens)
        .saturating_add(conversation_tokens)
        .saturating_add(tool_result_tokens)
        .saturating_add(attachment_tokens)
        .saturating_add(compaction_summary_tokens)
        .saturating_add(other_tokens);
    debug_assert_eq!(total_tokens, estimate.input_tokens);

    ContextBreakdown {
        system_tokens,
        instruction_tokens,
        conversation_tokens,
        tool_result_tokens,
        attachment_tokens,
        compaction_summary_tokens,
        other_tokens,
        total_tokens,
        structural_tokens: estimate.structural_tokens,
        provider_tokens: estimate.provider_tokens,
        context_limit: model.spec.limits.context_window,
    }
}

pub(super) fn observe_context_tracker(
    tracker: &ContextTracker,
    session: &Session,
    model: &Model,
    system: &str,
    tools: &[ToolDef],
) -> Result<ContextBreakdown, SessionError> {
    let messages = session.context_ref()?;
    let breakdown = context_breakdown(session, model, system, &messages, tools);
    tracker.observe_context(breakdown.clone());
    Ok(breakdown)
}
