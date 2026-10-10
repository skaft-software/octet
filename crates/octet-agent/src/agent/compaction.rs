//! Context compaction: policy, summaries, native compaction and the capacity cache.

use super::*;
use crate::compaction::{
    run_session_operation_hooks, session_operation_branch, SessionCompactionReason,
    SessionCompactionReplacement, SessionOperation, SessionOperationDecision,
    SessionOperationError, SessionOperationHook, SessionSourceRevision,
};

const COMPACTION_VETO: &str = "compaction cancelled by extension";
const SESSION_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tree_tests;

/// Result of a durable tree navigation at the idle session boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeNavigationResult {
    /// Selected user/custom message text, returned without resubmission.
    pub editor_text: Option<String>,
    /// Durable abandoned-branch summary created at the target parent.
    pub summary_entry: Option<EntryId>,
}

fn session_operation_error(error: SessionOperationError) -> AgentError {
    match error {
        SessionOperationError::Cancelled => AgentError::Cancelled,
        other => AgentError::InvalidCompactionPolicy(other.to_string()),
    }
}

fn compaction_reason(reason: CompactionReason) -> SessionCompactionReason {
    match reason {
        CompactionReason::Threshold => SessionCompactionReason::Threshold,
        CompactionReason::Overflow => SessionCompactionReason::Overflow,
    }
}

fn is_compaction_veto(error: &AgentError) -> bool {
    matches!(error, AgentError::InvalidCompactionPolicy(reason) if reason == COMPACTION_VETO)
}

async fn intercept_compaction(
    session: &mut Session,
    hooks: &[Arc<dyn SessionOperationHook>],
    first_kept: &EntryId,
    reason: SessionCompactionReason,
    instructions: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<Option<SessionCompactionReplacement>, AgentError> {
    if hooks.is_empty() {
        return Ok(None);
    }
    let operation = SessionOperation::BeforeCompact {
        reason,
        first_kept: first_kept.clone(),
        preparation: prepare_handoff(session, first_kept)?,
        branch_entries: session_operation_branch(session).map_err(session_operation_error)?,
        custom_instructions: instructions.map(str::to_owned),
    };
    match run_session_operation_hooks(
        session,
        hooks,
        &operation,
        cancellation,
        SESSION_OPERATION_TIMEOUT,
    )
    .await
    .map_err(session_operation_error)?
    {
        SessionOperationDecision::Continue => Ok(None),
        SessionOperationDecision::Cancel => {
            Err(AgentError::InvalidCompactionPolicy(COMPACTION_VETO.into()))
        }
        SessionOperationDecision::ReplaceCompaction { replacement } => Ok(Some(replacement)),
    }
}

async fn observe_compaction(
    session: &mut Session,
    hooks: &[Arc<dyn SessionOperationHook>],
    reason: SessionCompactionReason,
    from_extension: bool,
    cancellation: &CancellationToken,
) -> Result<(), AgentError> {
    if hooks.is_empty() {
        return Ok(());
    }
    let entry = session
        .head_ref()
        .and_then(|id| session.entry(id))
        .expect("successful compaction publishes its entry")
        .clone();
    let committed_id = entry.id.clone();
    run_session_operation_hooks(
        session,
        hooks,
        &SessionOperation::Compacted {
            reason,
            entry,
            from_extension,
        },
        cancellation,
        SESSION_OPERATION_TIMEOUT,
    )
    .await
    .map_err(|_| {
        AgentError::InvalidCompactionPolicy(format!(
            "compaction committed as {}; post-commit session hook failed; do not retry",
            committed_id.0
        ))
    })?;
    Ok(())
}

pub(super) fn assistant_text(response: &octet_ai::Response) -> Option<String> {
    let text = response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            octet_ai::AssistantPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    (!text.trim().is_empty()).then_some(text)
}

/// Preserve the prior bitmap source verbatim, separating it from each newly
/// serialized transcript section (including a split-turn prefix).
pub(super) fn snapcompact_source(preparation: &HandoffPreparation) -> String {
    let mut source = String::new();
    for section in [
        preparation.previous_summary.clone().unwrap_or_default(),
        serialize_conversation(&preparation.messages),
        serialize_conversation(&preparation.turn_prefix_messages),
    ] {
        if !section.is_empty() {
            if !source.is_empty() {
                source.push_str("\n\n");
            }
            source.push_str(&section);
        }
    }
    source
}

pub(super) struct CompactionContext<'a> {
    pub(super) run_id: &'a str,
    pub(super) resource_owner: &'a str,
    pub(super) retry_hooks: &'a [Arc<dyn ProviderRetryHook>],
    pub(super) compaction_strategy: Option<&'a Arc<dyn CompactionStrategy>>,
    pub(super) session_operation_hooks: &'a [Arc<dyn SessionOperationHook>],
    pub(super) max_network_wait: Option<Duration>,
    pub(super) provider_retries_enabled: bool,
    pub(super) client: &'a AiClient,
    /// Active model, used for context-window sizing and the normal request.
    pub(super) model: &'a Model,
    /// Optional configured route used for the summary request itself.
    pub(super) compaction_model: &'a Model,
    pub(super) summary_operation: crate::events::ProviderOperation,
    pub(super) session: &'a mut Session,
    pub(super) usage: &'a mut Usage,
    pub(super) run_cost: &'a mut CostAccumulator,
    pub(super) cache_retention: CacheRetention,
    pub(super) reasoning: &'a ReasoningConfig,
    pub(super) reasoning_mode: ReasoningMode,
    pub(super) session_id: &'a str,
    pub(super) max_session_tokens: Option<u64>,
    pub(super) max_session_cost_microdollars: Option<u64>,
    pub(super) abort: &'a AbortFlag,
    pub(super) mode: AgentCompactionMode,
    pub(super) threshold_fraction: f64,
    pub(super) keep_recent_tokens: u64,
    pub(super) events: &'a mpsc::UnboundedSender<AgentEvent>,
    pub(super) context: &'a ContextTracker,
    pub(super) tool_generation: u64,
    pub(super) capacity: &'a mut ContextCapacityCache,
    /// Explicit span observer copied from the owning agent for this compaction.
    pub(super) telemetry: TelemetryContext,
}

pub(super) struct CapacityEstimate {
    pub(super) input_tokens: u64,
    pub(super) max_output_tokens: u64,
    pub(super) active_system: String,
    pub(super) effective_request: Option<Request>,
}

/// Total-only context accounting used by the pre-request capacity gate.
///
/// The first value is seeded from the exact context observation. For ordinary
/// canonical-message appends, later values advance from the cached head using
/// a per-message upper bound instead of serializing the complete history. A
/// branch checkout, compaction, tool-surface change, or replay-mode change
/// falls back to the exact estimator. Responses appends estimate only the new
/// route-affine opaque replay items, not the complete unchanged prefix. The detailed category breakdown remains
/// the on-demand telemetry path in [`context_breakdown`].
pub(super) struct ContextCapacityCache {
    pub(super) head: Option<EntryId>,
    pub(super) tool_generation: u64,
    pub(super) structural_tokens: u64,
    pub(super) provider_tokens: Option<u64>,
    pub(super) responses_items: Option<usize>,
    pub(super) valid: bool,
    #[cfg(test)]
    pub(super) full_rebuilds: usize,
}

impl ContextCapacityCache {
    pub(super) fn seeded(
        session: &Session,
        tool_generation: u64,
        context: &ContextBreakdown,
    ) -> Self {
        Self {
            head: session.head(),
            tool_generation,
            structural_tokens: context.structural_tokens,
            provider_tokens: context.provider_tokens,
            responses_items: None,
            valid: true,
            #[cfg(test)]
            full_rebuilds: 0,
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Advances over entries appended below the cached head.
    ///
    /// Only a local compaction changes the canonical message sequence among
    /// the entries handled here. All other non-message entries are invisible
    /// to canonical requests, while each message contributes a conservative
    /// standalone estimate. The entries are collected and validated before
    /// mutating the cache so a failed ancestry walk cannot leave a partial
    /// estimate behind.
    pub(super) fn advance_messages(&mut self, session: &Session) -> bool {
        if !self.valid {
            return false;
        }
        let cached_head = self.head.clone();
        let mut cursor = session.head_ref();
        if cursor == cached_head.as_ref() {
            return true;
        }

        let mut appended = Vec::new();
        while cursor != cached_head.as_ref() {
            let Some(id) = cursor else {
                return false;
            };
            let Some(entry) = session.entry(id) else {
                return false;
            };
            if matches!(
                entry.value,
                EntryValue::Compaction { .. } | EntryValue::BranchSummary { .. }
            ) {
                return false;
            }
            appended.push(entry);
            cursor = entry.parent.as_ref();
        }

        for entry in appended.into_iter().rev() {
            if let EntryValue::Message(message) = &entry.value {
                let delta = estimate_messages_tokens(std::slice::from_ref(message));
                self.structural_tokens = self.structural_tokens.saturating_add(delta);
                if let Some(provider) = self.provider_tokens.as_mut() {
                    *provider = provider.saturating_add(delta);
                }
            }
        }
        self.head = session.head();
        true
    }

    pub(super) fn advance_for_model(&mut self, session: &Session, model: &Model) -> bool {
        if model.spec.protocol != Protocol::OpenAiResponses {
            return self.advance_messages(session);
        }
        if !self.valid {
            return false;
        }
        let Ok(replay) = session.responses_replay_snapshot(&model.endpoint.id, &model.spec.id)
        else {
            return false;
        };
        let Some(replay) = replay else {
            return self.responses_items.is_none() && self.advance_messages(session);
        };
        let Some(first_new) = self.responses_items else {
            return false;
        };
        // A compacted window can grow beyond the old length: length alone is
        // not an invalidation fence. Check only the new durable ancestry.
        let mut cursor = session.head_ref();
        while cursor != self.head.as_ref() {
            let Some(entry) = cursor.and_then(|id| session.entry(id)) else {
                return false;
            };
            if matches!(
                entry.value,
                EntryValue::Compaction { .. }
                    | EntryValue::ResponsesCompaction { .. }
                    | EntryValue::BranchSummary { .. }
            ) {
                return false;
            }
            cursor = entry.parent.as_ref();
        }
        let Some(suffix) = replay.get(first_new..) else {
            return false;
        };
        if !suffix.is_empty() {
            let Ok(input) = octet_ai::responses::encode_responses_replay(model, None, suffix)
            else {
                return false;
            };
            // Standalone framing is a conservative upper bound on appending
            // the same opaque output/user items to the existing wire window.
            let delta = estimate_responses_request_tokens(&input, suffix, &[], None);
            self.structural_tokens = self.structural_tokens.saturating_add(delta);
            if let Some(provider) = &mut self.provider_tokens {
                *provider = provider.saturating_add(delta);
            }
        }
        self.responses_items = Some(replay.len());
        self.head = session.head();
        true
    }

    /// Replaces the cache with a route-accurate full estimate.
    pub(super) fn rebuild(
        &mut self,
        session: &Session,
        model: &Model,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        tool_generation: u64,
    ) -> RequestContextEstimate {
        let estimate = reconcile_context_estimate(session, model, system, messages, tools);
        self.head = session.head();
        self.tool_generation = tool_generation;
        self.structural_tokens = estimate.structural_tokens;
        self.provider_tokens = estimate.provider_tokens;
        self.responses_items = (model.spec.protocol == Protocol::OpenAiResponses)
            .then(|| {
                session
                    .responses_replay_snapshot(&model.endpoint.id, &model.spec.id)
                    .ok()
                    .flatten()
            })
            .flatten()
            .map(|items| items.len());
        self.valid = true;
        #[cfg(test)]
        {
            self.full_rebuilds = self.full_rebuilds.saturating_add(1);
        }
        estimate
    }

    pub(super) fn estimate(
        &mut self,
        session: &Session,
        model: &Model,
        system: &str,
        tools: &[ToolDef],
        tool_generation: u64,
    ) -> Result<RequestContextEstimate, SessionError> {
        let messages = session.context_ref()?;
        let can_advance = self.valid
            && self.tool_generation == tool_generation
            && self.advance_for_model(session, model);
        if !can_advance {
            self.rebuild(session, model, system, &messages, tools, tool_generation);
        }
        let input_tokens = self
            .provider_tokens
            .map_or(self.structural_tokens, |provider| {
                self.structural_tokens.max(provider)
            });
        Ok(RequestContextEstimate {
            structural_tokens: self.structural_tokens,
            provider_tokens: self.provider_tokens,
            input_tokens,
        })
    }

    /// Re-anchors provider reconciliation after a completed assistant turn.
    ///
    /// Provider usage is authoritative for the prefix through that assistant.
    /// A zero usage report is left on the incrementally advanced estimate,
    /// matching `provider_context_estimate`'s behavior of ignoring unusable
    /// records rather than replacing a usable older measurement with zero.
    pub(super) fn observe_assistant_response(
        &mut self,
        session: &Session,
        model: &Model,
        usage: &Usage,
    ) {
        if !self.advance_for_model(session, model) {
            self.invalidate();
            return;
        }
        let measured = usage_context_tokens(usage);
        if measured > 0 {
            self.provider_tokens = Some(measured);
        }
    }

    #[cfg(test)]
    pub(super) fn full_rebuilds(&self) -> usize {
        self.full_rebuilds
    }
}

impl CompactionContext<'_> {
    pub(super) async fn call(
        &mut self,
        system: &str,
        messages: Vec<Message>,
        output_tokens: u64,
    ) -> Result<Option<String>, AgentError> {
        // Row 3.5: the summary boundary owns its own provider request, which
        // nests under it. Both settle explicitly on every returned outcome.
        let summary_guard = self
            .telemetry
            .begin_typed::<SummarySpan>(EmptyAttributes {});
        let summary_request_guard =
            summary_guard
                .context()
                .begin_typed::<ProviderRequestSpan>(RequestAttributes {
                    operation: SpanOperation::Summary,
                });
        // Compaction is a normal provider request: retaining the stable session
        // affinity lets compatible providers reuse any common prefix and keeps
        // its accounting visible alongside autonomous turns.
        let request = Request {
            system: Some(system.to_owned()),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            max_output_tokens: Some(
                self.compaction_model
                    .spec
                    .limits
                    .max_output_tokens
                    .clamp(1, output_tokens),
            ),
            temperature: None,
            stop: Vec::new(),
            reasoning: octet_ai::select_auxiliary_reasoning(self.compaction_model)?,
            reasoning_mode: ReasoningMode::Standard,
            responses: None,
            output_format: OutputFormat::Text,
            output_modalities: OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: self.cache_retention,
            session_id: Some(self.session_id.to_owned()),
        };
        let input_tokens = estimate_request_tokens(
            request.system.as_deref().unwrap_or_default(),
            &request.messages,
            &request.tools,
        );
        let input_budget = self
            .compaction_model
            .spec
            .limits
            .context_window
            .saturating_sub(request.max_output_tokens.unwrap_or(output_tokens));
        if input_tokens > input_budget {
            return Err(AgentError::ContextExceeded {
                estimate: input_tokens,
                budget: input_budget,
            });
        }
        let reserved_output_tokens = reservation_output_tokens(
            self.session,
            self.compaction_model,
            request.max_output_tokens.unwrap_or(output_tokens),
            self.max_session_tokens,
            self.max_session_cost_microdollars,
        )?;
        reserve_request_tokens(
            self.session,
            input_tokens,
            reserved_output_tokens,
            self.max_session_tokens,
        )?;
        reserve_request_cost(
            self.session,
            self.compaction_model,
            input_tokens,
            reserved_output_tokens,
            self.max_session_cost_microdollars,
            request.cache_retention,
        )?;
        let response = recover_auxiliary(
            AuxiliaryRecovery {
                dispatch: AuxiliaryDispatch::default(),
                session: self.session,
                run_id: self.run_id,
                resource_owner: self.resource_owner,
                retry_hooks: self.retry_hooks,
                max_network_wait: self.max_network_wait,
                model: self.compaction_model,
                qualified: qualified_inference_replacement(self.compaction_model, &request),
                enabled: self.provider_retries_enabled,
                hard_budget: self.max_session_tokens.is_some()
                    || self.max_session_cost_microdollars.is_some(),
                exposure: request_uncertainty_bound(
                    self.compaction_model,
                    None,
                    reserved_output_tokens,
                    None,
                    request.cache_retention,
                ),
                input_tokens,
                output_tokens: reserved_output_tokens,
                token_limit: self.max_session_tokens,
                cost_limit: self.max_session_cost_microdollars,
                retention: request.cache_retention,
                abort: self.abort,
                events: self.events,
                operation: self.summary_operation,
                session_id: self.session_id,
            },
            |deadline, dispatch| {
                auxiliary_complete(
                    self.client,
                    self.compaction_model,
                    request.clone(),
                    deadline,
                    self.max_network_wait,
                    dispatch,
                )
            },
            |session, response| {
                session.record_compaction_usage(
                    self.compaction_model.endpoint.id.clone(),
                    self.compaction_model.spec.id.clone(),
                    response.usage,
                    response.cost,
                )?;
                add_usage(self.usage, &response.usage);
                self.run_cost.add(response.cost);
                Ok(())
            },
        )
        .await?;
        // Billing has settled; cancellation suppresses summary publication.
        if self.abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        if !matches!(
            response.stop_reason,
            StopReason::EndTurn | StopReason::StopSequence
        ) {
            summary_request_guard.finish(false);
            summary_guard.finish(false);
            return Ok(None);
        }
        if response
            .message
            .content
            .iter()
            .any(|part| matches!(part, AssistantPart::ToolCall(_)))
        {
            return Err(AgentError::IncompleteResponse {
                stop_reason: "tool-free summary attempted to call a tool".into(),
            });
        }
        let text = assistant_text(&response).ok_or_else(|| AgentError::IncompleteResponse {
            stop_reason: "compaction summary was empty or whitespace-only".to_owned(),
        })?;
        // This is shared by autonomous compaction and explicit callers such as
        // `/compact`; reject bad provider output before either path can merge
        // it into a durable handoff.
        validate_compaction_summary_part(&text)?;
        CompletionAttributes::usage(&response.usage)
            .with_inference(response.inference.as_ref())
            .with_uncertainty(self.session.has_uncertain_usage())
            .record(&summary_request_guard.span);
        summary_request_guard.finish(false);
        summary_guard.finish(false);
        Ok(Some(text))
    }

    /// Generate a Pi-compatible structured handoff, including a dedicated
    /// summary when the retained boundary splits the current turn.
    pub(super) async fn summarize(
        &mut self,
        preparation: &HandoffPreparation,
    ) -> Result<Option<String>, AgentError> {
        let history = if preparation.messages.is_empty() {
            preparation
                .previous_summary
                .clone()
                .or_else(|| Some("No prior history.".to_owned()))
        } else {
            self.call(
                SUMMARIZATION_SYSTEM_PROMPT,
                vec![build_handoff_message(preparation)],
                SUMMARY_OUTPUT_TOKENS,
            )
            .await?
        };
        let Some(mut summary) = history else {
            return Ok(None);
        };
        validate_compaction_summary_part(&summary)?;

        if !preparation.turn_prefix_messages.is_empty() {
            let Some(prefix_summary) = self
                .call(
                    SUMMARIZATION_SYSTEM_PROMPT,
                    vec![build_turn_prefix_handoff_message(
                        &preparation.turn_prefix_messages,
                    )],
                    TURN_PREFIX_OUTPUT_TOKENS,
                )
                .await?
            else {
                return Ok(None);
            };
            append_compaction_turn_prefix(&mut summary, &prefix_summary)?;
        }

        Ok(Some(summary))
    }

    /// Render every source character, including an earlier bitmap checkpoint.
    /// One deadline covers all sequential chunks and their image validation;
    /// neither timeout nor cancellation can commit a partial checkpoint.
    pub(super) async fn render_snapcompact(
        &mut self,
        preparation: &HandoffPreparation,
    ) -> Result<SnapcompactCheckpoint, AgentError> {
        const RENDER_DEADLINE: Duration = Duration::from_secs(120);
        let deadline = tokio::time::Instant::now() + RENDER_DEADLINE;
        let expired = || {
            AgentError::InvalidCompactionPolicy(
                "snapcompact rendering exceeded the 120-second deadline; history was not discarded"
                    .into(),
            )
        };
        let strategy = self.compaction_strategy.expect("selected strategy exists");
        let source = snapcompact_source(preparation);
        if source.trim().is_empty() || source.len() > 256 * 1024 {
            return Err(AgentError::InvalidCompactionPolicy(
                "snapcompact source is empty or exceeds 256 KiB; history was not discarded".into(),
            ));
        }
        let mut frames = Vec::new();
        let mut total_bytes = 0usize;
        let limits = self
            .model
            .spec
            .preset
            .image_input_limits
            .unwrap_or(FALLBACK_IMAGE_LIMITS);
        let chars: Vec<char> = source.chars().collect();
        for chunk in chars.chunks(2048) {
            let text: String = chunk.iter().collect();
            let rendered = tokio::select! {
                biased;
                _ = self.abort.wait() => return Err(AgentError::Cancelled),
                _ = tokio::time::sleep_until(deadline) => return Err(expired()),
                result = strategy.render(&self.model.spec.id.0, &text, self.resource_owner) => result
                    .map_err(|error| AgentError::InvalidCompactionPolicy(format!(
                        "snapcompact extension failed: {error}"
                    )))?,
            };
            if rendered.is_empty() {
                return Err(AgentError::InvalidCompactionPolicy(
                    "snapcompact returned no frames for a transcript slice".into(),
                ));
            }
            // Decoding and validating up to 32 extension frames is CPU work;
            // keep it off the async worker and within the same operation deadline.
            let existing_frames = frames.len();
            let cancelled = Arc::new(AtomicBool::new(false));
            let guard = CancelBlockingImages(Arc::clone(&cancelled));
            let worker = tokio::task::spawn_blocking(move || {
                let mut images = Vec::with_capacity(rendered.len().min(256));
                let mut bytes = total_bytes;
                for frame in rendered {
                    if cancelled.load(Ordering::Acquire) {
                        return Err(AgentError::Cancelled);
                    }
                    if !frame.starts_with(b"\x89PNG\r\n\x1a\n") || frame.len() > 384 * 1024 {
                        return Err(AgentError::InvalidCompactionPolicy(
                            "snapcompact returned an empty, invalid, or oversized PNG".into(),
                        ));
                    }
                    bytes = bytes.saturating_add(frame.len());
                    if existing_frames + images.len() >= 256 || bytes > 8 * 1024 * 1024 {
                        return Err(AgentError::InvalidCompactionPolicy(
                            "snapcompact exceeded 256 frames or 8 MiB; history was not discarded"
                                .into(),
                        ));
                    }
                    let image = Media::image_bytes(
                        bytes::Bytes::from(frame),
                        "image/png".parse().expect("static MIME"),
                    );
                    let Media::Image(image_data) = &image else {
                        unreachable!("image_bytes creates image media")
                    };
                    let validated = octet_ai::prepare_user_image(image_data, limits)?;
                    if !matches!((&validated.source, &image_data.source),
                        (ImageSource::Inline(a), ImageSource::Inline(b)) if a == b)
                    {
                        return Err(AgentError::InvalidCompactionPolicy(
                            "snapcompact frame exceeds model image dimensions; history was not discarded".into(),
                        ));
                    }
                    images.push(image);
                }
                Ok((images, bytes))
            });
            let (images, bytes) = tokio::select! {
                biased;
                _ = self.abort.wait() => return Err(AgentError::Cancelled),
                _ = tokio::time::sleep_until(deadline) => return Err(expired()),
                result = worker => result.map_err(|_| AgentError::InvalidCompactionPolicy(
                    "snapcompact frame validation worker failed; history was not discarded".into()
                ))??,
            };
            drop(guard);
            frames.extend(images);
            total_bytes = bytes;
        }
        if self.abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(expired());
        }
        Ok(SnapcompactCheckpoint {
            source_text: source,
            frames,
        })
    }

    pub(super) fn preferred_boundary(&self) -> Result<Option<EntryId>, AgentError> {
        let Some(candidate) =
            choose_first_kept_by_tokens(self.session, self.keep_recent_tokens, |message| {
                estimate_messages_tokens(std::slice::from_ref(message))
            })?
        else {
            return Ok(None);
        };
        // Pi's cut-point fallback may select the oldest visible message when
        // the token budget exceeds the available history. That is a no-op for
        // an agent compaction unless a split-turn prefix is available; allow
        // the episode fallback below to make progress in that case.
        let preparation = prepare_handoff(self.session, &candidate)?;
        if preparation.messages.is_empty() && preparation.turn_prefix_messages.is_empty() {
            Ok(None)
        } else {
            Ok(Some(candidate))
        }
    }

    pub(super) fn oldest_reducible_boundary(&self) -> Option<EntryId> {
        turn_starts(self.session).get(1).cloned()
    }

    pub(super) fn begin_compaction(
        &self,
        system: &str,
        tools: &[ToolDef],
        reason: CompactionReason,
    ) -> Result<u64, AgentError> {
        observe_context_tracker(self.context, self.session, self.model, system, tools)?;
        let id = self.context.compaction_started(reason);
        let _ = self.events.send(AgentEvent::CompactionStarted { reason });
        Ok(id)
    }

    pub(super) fn finish_compaction(
        &self,
        id: u64,
        system: &str,
        tools: &[ToolDef],
        reason: CompactionReason,
        operation: &Result<CompactionInfo, AgentError>,
        provider_model: &Model,
    ) {
        let after = operation.as_ref().ok().and_then(|_| {
            observe_context_tracker(self.context, self.session, self.model, system, tools).ok()
        });
        self.context
            .compaction_finished(id, after, operation.is_ok());
        let event_result = match operation {
            Ok(info) => Ok(info.clone()),
            Err(error) => Err(public_error_diagnostic(
                error,
                &provider_model.endpoint.id.0,
                &provider_model.spec.id.0,
            )),
        };
        let _ = self.events.send(AgentEvent::CompactionFinished {
            reason,
            result: event_result,
        });
    }

    pub(super) async fn compact_native_responses(
        &mut self,
        system: &str,
        tools: &[ToolDef],
        reason: CompactionReason,
    ) -> Result<CompactionInfo, AgentError> {
        if !self.session_operation_hooks.is_empty() {
            let first_kept = self.session.head().ok_or_else(|| {
                AgentError::InvalidCompactionPolicy("no history to compact".into())
            })?;
            if intercept_compaction(
                self.session,
                self.session_operation_hooks,
                &first_kept,
                compaction_reason(reason),
                None,
                &self.abort.cancellation,
            )
            .await?
            .is_some()
            {
                return Err(AgentError::InvalidCompactionPolicy(
                    "text replacement cannot replace a native Responses checkpoint".into(),
                ));
            }
        }
        let id = self.begin_compaction(system, tools, reason)?;
        // Row 3.5: one compaction boundary. Dropped guards settle as
        // errors, so the explicit settle below marks only real success.
        let compaction_guard = self
            .telemetry
            .begin_typed::<CompactionSpan>(EmptyAttributes {});
        let operation_started = std::time::Instant::now();
        let usage_before = *self.usage;
        let cost_before = self.run_cost.microdollars;
        let unpriced_before = self.run_cost.unpriced_operations;
        let mut operation = async {
            if self.model.spec.protocol != Protocol::OpenAiResponses {
                return Err(AgentError::InvalidCompactionPolicy(
                    "native Responses compaction requires an OpenAI Responses model route"
                        .to_owned(),
                ));
            }
            if current_head_is_native_checkpoint(self.session, self.model) {
                return Err(AgentError::InvalidCompactionPolicy(
                    "native Responses compaction made no progress since the previous checkpoint"
                        .to_owned(),
                ));
            }
            let replay = self
                .session
                .responses_replay_snapshot(&self.model.endpoint.id, &self.model.spec.id)?
                .ok_or_else(|| {
                    AgentError::InvalidCompactionPolicy(
                        "native Responses compaction requires complete route-affine opaque replay"
                            .to_owned(),
                    )
                })?;
            let input = octet_ai::responses::encode_responses_replay(self.model, None, &replay)?;
            let instructions = (!system.is_empty()).then_some(system);
            let request = ResponsesCompactRequest::for_model(
                self.model,
                input,
                instructions.map(str::to_owned),
                tools,
                self.reasoning,
                self.reasoning_mode,
                &OutputFormat::Text,
                self.cache_retention,
                Some(self.session_id),
            )?;
            let input_tokens = estimate_compact_request_tokens(&request, &replay);
            require_enforceable_output_cap(
                self.session,
                None,
                self.max_session_tokens,
                self.max_session_cost_microdollars,
            )?;
            reserve_request_tokens(
                self.session,
                input_tokens,
                self.model.spec.limits.max_output_tokens,
                self.max_session_tokens,
            )?;
            reserve_request_cost(
                self.session,
                self.model,
                input_tokens,
                self.model.spec.limits.max_output_tokens,
                self.max_session_cost_microdollars,
                self.cache_retention,
            )?;
            let covered_through = self.session.head().ok_or(SessionError::EmptySession)?;
            let response = recover_auxiliary(
                AuxiliaryRecovery {
                    dispatch: AuxiliaryDispatch::default(),
                    session: self.session,
                    run_id: self.run_id,
                    resource_owner: self.resource_owner,
                    retry_hooks: self.retry_hooks,
                    max_network_wait: self.max_network_wait,
                    model: self.model,
                    qualified: self.model.endpoint.runtime.responses_profile
                        == octet_ai::ResponsesRuntimeProfile::Codex
                        && request.input.items().iter().all(|item| {
                            matches!(
                                item.as_json()
                                    .get("type")
                                    .and_then(serde_json::Value::as_str),
                                Some(
                                    "message"
                                        | "reasoning"
                                        | "function_call"
                                        | "function_call_output"
                                        | "compaction"
                                )
                            )
                        }),
                    enabled: self.provider_retries_enabled,
                    hard_budget: self.max_session_tokens.is_some()
                        || self.max_session_cost_microdollars.is_some(),
                    exposure: None, // Compact has no provider-enforced output cap.
                    input_tokens,
                    output_tokens: self.model.spec.limits.max_output_tokens,
                    token_limit: self.max_session_tokens,
                    cost_limit: self.max_session_cost_microdollars,
                    retention: self.cache_retention,
                    abort: self.abort,
                    events: self.events,
                    operation: crate::events::ProviderOperation::NativeCompaction,
                    session_id: self.session_id,
                },
                |deadline, dispatch| {
                    auxiliary_compact(
                        self.client,
                        self.model,
                        request.clone(),
                        deadline,
                        self.max_network_wait,
                        dispatch,
                    )
                },
                |session, response| {
                    let cost = self.model.spec.pricing.as_ref().and_then(|pricing| {
                        octet_ai::pricing::cost_of(pricing, &response.usage).ok()
                    });
                    session.record_compaction_usage(
                        self.model.endpoint.id.clone(),
                        self.model.spec.id.clone(),
                        response.usage,
                        cost,
                    )?;
                    add_usage(self.usage, &response.usage);
                    self.run_cost.add(cost);
                    Ok(())
                },
            )
            .await?;
            if self.abort.is_set() {
                return Err(AgentError::Cancelled);
            }
            validate_native_compact_output(&response.output)?;
            let checkpoint = self.session.append_responses_compaction(
                self.model.endpoint.id.clone(),
                self.model.spec.id.clone(),
                response.output,
            )?;
            Ok(CompactionInfo {
                kind: CompactionKind::NativeResponses {
                    checkpoint,
                    covered_through: covered_through.clone(),
                },
                summary: String::new(),
                first_kept: covered_through,
                usage: Usage::default(),
                elapsed: Duration::ZERO,
                cost_microdollars: None,
            })
        }
        .await;
        if let Ok(info) = operation.as_mut() {
            info.usage = usage_since(*self.usage, usage_before);
            info.elapsed = operation_started.elapsed();
            info.cost_microdollars = self
                .model
                .spec
                .pricing
                .as_ref()
                .filter(|_| self.run_cost.unpriced_operations == unpriced_before)
                .map(|_| self.run_cost.microdollars.saturating_sub(cost_before));
        }

        self.finish_compaction(id, system, tools, reason, &operation, self.model);
        compaction_guard.finish(operation.is_err());
        if operation.is_ok() {
            observe_compaction(
                self.session,
                self.session_operation_hooks,
                compaction_reason(reason),
                false,
                &self.abort.cancellation,
            )
            .await?;
        }
        operation
    }

    pub(super) async fn compact_boundary(
        &mut self,
        first_kept: EntryId,
        system: &str,
        tools: &[ToolDef],
        reason: CompactionReason,
    ) -> Result<CompactionInfo, AgentError> {
        let replacement = intercept_compaction(
            self.session,
            self.session_operation_hooks,
            &first_kept,
            compaction_reason(reason),
            None,
            &self.abort.cancellation,
        )
        .await?;
        let from_extension = replacement.is_some();
        let first_kept = replacement
            .as_ref()
            .map_or(first_kept, |value| value.first_kept.clone());
        let id = self.begin_compaction(system, tools, reason)?;
        // Row 3.5: one compaction boundary. Dropped guards settle as
        // errors, so the explicit settle below marks only real success.
        let compaction_guard = self
            .telemetry
            .begin_typed::<CompactionSpan>(EmptyAttributes {});
        // Summary requests issued inside this operation are children of the
        // compaction boundary, not of the turn that triggered the compaction.
        // The caller's scope is restored before the settled status is written.
        let turn_scope = std::mem::replace(&mut self.telemetry, compaction_guard.context());
        let operation_started = std::time::Instant::now();
        let usage_before = *self.usage;
        let cost_before = self.run_cost.microdollars;
        let unpriced_before = self.run_cost.unpriced_operations;
        let mut operation = async {
            let preparation = prepare_handoff(self.session, &first_kept)?;
            if preparation.messages.is_empty() && preparation.turn_prefix_messages.is_empty() {
                return Err(AgentError::ContextExceeded {
                    estimate: 0,
                    budget: self
                        .model
                        .spec
                        .limits
                        .context_window
                        .saturating_sub(self.model.spec.limits.max_output_tokens),
                });
            }
            if let Some(replacement) = replacement {
                validate_compaction_summary_part(&replacement.summary)?;
                if self.abort.is_set() { return Err(AgentError::Cancelled); }
                self.session.compact_with_details(replacement.summary.clone(), first_kept.clone(), preparation.details)?;
                return Ok(CompactionInfo {
                    kind: CompactionKind::Local, summary: replacement.summary, first_kept,
                    usage: Usage::default(), elapsed: Duration::ZERO, cost_microdollars: None,
                });
            }
            if self.compaction_strategy.is_some()
                && self.model.spec.effective_input_modalities().contains(Modality::Image)
            {
                let checkpoint = self.render_snapcompact(&preparation).await?;
                let summary = finish_validated_compaction_handoff(
                    "Earlier conversation is encoded in the attached bitmap frames. Read each frame in order before continuing.".into(),
                    &preparation.details,
                )?;
                let preview = self.session.preview_compaction_context(&first_kept, &summary, &checkpoint)?;
                let estimate = estimate_request_tokens(system, &preview, tools);
                let budget = self.model.spec.limits.context_window
                    .saturating_sub(agent_compaction_reserve_tokens(self.model, self.reasoning));
                if estimate > budget {
                    return Err(AgentError::ContextExceeded { estimate, budget });
                }
                if self.abort.is_set() { return Err(AgentError::Cancelled); }
                self.session.compact_snapcompact(summary.clone(), first_kept.clone(),
                    preparation.details, checkpoint)?;
                return Ok(CompactionInfo {
                    kind: CompactionKind::Snapcompact, summary, first_kept,
                    usage: Usage::default(), elapsed: Duration::ZERO,
                    cost_microdollars: None,
                });
            }
            let summary = match self.summarize(&preparation).await? {
                Some(summary) => {
                    finish_validated_compaction_handoff(summary, &preparation.details)?
                }
                None => {
                    return Err(AgentError::IncompleteResponse {
                        stop_reason: "compaction summary did not finish normally".to_owned(),
                    });
                }
            };
            if self.abort.is_set() {
                return Err(AgentError::Cancelled);
            }
            self.session.compact_with_details(
                summary.clone(),
                first_kept.clone(),
                preparation.details,
            )?;
            Ok(CompactionInfo {
                kind: CompactionKind::Local,
                summary,
                first_kept,
                usage: Usage::default(),
                elapsed: Duration::ZERO,
                cost_microdollars: None,
            })
        }
        .await;
        if let Ok(info) = operation.as_mut() {
            info.usage = usage_since(*self.usage, usage_before);
            info.elapsed = operation_started.elapsed();
            info.cost_microdollars = self
                .compaction_model
                .spec
                .pricing
                .as_ref()
                .filter(|_| self.run_cost.unpriced_operations == unpriced_before)
                .map(|_| self.run_cost.microdollars.saturating_sub(cost_before));
            if from_extension {
                info.cost_microdollars = None;
            }
        }

        self.telemetry = turn_scope;
        self.finish_compaction(id, system, tools, reason, &operation, self.compaction_model);
        compaction_guard.finish(operation.is_err());
        if operation.is_ok() {
            observe_compaction(
                self.session,
                self.session_operation_hooks,
                compaction_reason(reason),
                from_extension,
                &self.abort.cancellation,
            )
            .await?;
        }
        operation
    }

    pub(super) async fn ensure_capacity(
        &mut self,
        system: &str,
        tools: &[ToolDef],
        compaction_reserve_tokens: u64,
        provider_output_ceiling: u64,
        preparation: Option<&ProviderContextPreparation<'_>>,
    ) -> Result<CapacityEstimate, AgentError> {
        if !self
            .model
            .spec
            .effective_input_modalities()
            .contains(Modality::Image)
            && self.session.has_snapcompact_context()?
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "active history contains bitmap frames; switch to a vision-capable model before continuing".into(),
            ));
        }
        let context_window = self.model.spec.limits.context_window;
        let budget = context_window.saturating_sub(compaction_reserve_tokens);
        let threshold = ((context_window as f64) * self.threshold_fraction).floor() as u64;
        let resolve = |input_tokens, active_system, mut effective_request: Option<Request>| {
            let max_output_tokens = resolve_request_max_output_tokens(
                context_window,
                input_tokens,
                provider_output_ceiling,
            );
            if let Some(request) = effective_request.as_mut() {
                request.max_output_tokens = Some(max_output_tokens);
            }
            CapacityEstimate {
                input_tokens,
                max_output_tokens,
                active_system,
                effective_request,
            }
        };
        let mut native_attempted = false;
        loop {
            let active_system = system.to_owned();
            // Hook preparation precedes every estimate, including after a
            // compaction changes the durable head. The cache's canonical prefix
            // must never size a projected request.
            let effective_request = match preparation {
                Some(preparation) => {
                    let request = self.canonical_provider_request(
                        &active_system,
                        tools,
                        provider_output_ceiling,
                        preparation,
                    )?;
                    let context = crate::extension::ProviderContextProjectionContext {
                        resource_owner: self.resource_owner.to_owned(),
                        session_id: self.session_id.to_owned(),
                        head: self.session.head(),
                        tool_generation: self.tool_generation,
                    };
                    // The hook future owns its effective snapshot, independent
                    // of Session. This owning driver services private append
                    // leaves while it waits, then freezes against the post-hook
                    // head without rerunning a mutating hook after its append.
                    let projection = project_provider_context(
                        request,
                        preparation.hooks,
                        &context,
                        self.model,
                        self.abort,
                        self.session,
                    );
                    Some(Box::pin(projection).await?)
                }
                None => None,
            };
            let estimate = match effective_request.as_ref() {
                Some(request) => effective_request_estimate(request),
                None => {
                    self.capacity
                        .estimate(
                            self.session,
                            self.model,
                            &active_system,
                            tools,
                            self.tool_generation,
                        )?
                        .input_tokens
                }
            };
            let over_capacity = estimate > budget;
            let over_threshold = estimate.saturating_add(compaction_reserve_tokens) > threshold;
            if !over_capacity && (self.mode == AgentCompactionMode::Disabled || !over_threshold) {
                return Ok(resolve(estimate, active_system, effective_request));
            }
            if self.mode == AgentCompactionMode::Disabled {
                return Err(AgentError::ContextExceeded { estimate, budget });
            }
            let reason = if over_capacity {
                CompactionReason::Overflow
            } else {
                CompactionReason::Threshold
            };
            if self.mode == AgentCompactionMode::NativeResponses {
                // One native compaction attempt per capacity check. If the
                // provider returns an output that does not make progress, do
                // not loop forever or silently switch to local summarization.
                if native_attempted {
                    if over_capacity {
                        return Err(AgentError::ContextExceeded { estimate, budget });
                    }
                    return Ok(resolve(estimate, active_system, effective_request));
                }
                match self
                    .compact_native_responses(&active_system, tools, reason)
                    .await
                {
                    Ok(_) => {}
                    Err(error) if is_compaction_veto(&error) => {
                        if over_capacity {
                            return Err(AgentError::ContextExceeded { estimate, budget });
                        }
                        return Ok(resolve(estimate, active_system, effective_request));
                    }
                    Err(error) => return Err(error),
                }
                native_attempted = true;
                continue;
            }
            // `keep_recent_tokens` is a preference, not permission to sail past
            // the configured threshold. If the retained episodes themselves
            // are unusually large, compact the oldest reducible episode.
            let boundary = self
                .preferred_boundary()?
                .or_else(|| self.oldest_reducible_boundary());
            if let Some(first_kept) = boundary {
                match self
                    .compact_boundary(first_kept, &active_system, tools, reason)
                    .await
                {
                    Ok(_) => continue,
                    Err(error) if is_compaction_veto(&error) => {
                        // A veto suppresses threshold compaction, not the host's
                        // hard context bound, and must not re-run the same hook.
                        if over_capacity {
                            return Err(AgentError::ContextExceeded { estimate, budget });
                        }
                        return Ok(resolve(estimate, active_system, effective_request));
                    }
                    Err(error) => return Err(error),
                }
            }
            if estimate <= budget {
                return Ok(resolve(estimate, active_system, effective_request));
            }
            return Err(AgentError::ContextExceeded { estimate, budget });
        }
    }

    pub(super) async fn force_one_boundary(
        &mut self,
        system: &str,
        tools: &[ToolDef],
        compaction_reserve_tokens: u64,
    ) -> Result<(), AgentError> {
        if !self
            .model
            .spec
            .effective_input_modalities()
            .contains(Modality::Image)
            && self.session.has_snapcompact_context()?
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "active history contains bitmap frames; switch to a vision-capable model before continuing".into(),
            ));
        }
        if self.mode == AgentCompactionMode::NativeResponses {
            let active_system = system.to_owned();
            self.compact_native_responses(&active_system, tools, CompactionReason::Overflow)
                .await?;
            return Ok(());
        }
        let boundary = if self.mode == AgentCompactionMode::Local {
            self.preferred_boundary()?
                .or_else(|| self.oldest_reducible_boundary())
        } else {
            None
        };
        if let Some(first_kept) = boundary {
            self.compact_boundary(first_kept, system, tools, CompactionReason::Overflow)
                .await?;
            return Ok(());
        }
        let estimate = self
            .capacity
            .estimate(
                self.session,
                self.model,
                system,
                tools,
                self.tool_generation,
            )?
            .input_tokens;
        let budget = self
            .model
            .spec
            .limits
            .context_window
            .saturating_sub(compaction_reserve_tokens);
        Err(AgentError::ContextExceeded { estimate, budget })
    }
}

impl Agent {
    /// Configure the model used for autonomous context summaries. Passing
    /// `None` keeps summaries on the active conversation model.
    pub fn set_compaction_model(&mut self, model: Option<Model>) {
        self.compaction_model = model;
        self.sync_delegation_runtime_settings();
    }

    /// Read-only access to the autonomous compaction model, if overridden.
    pub fn compaction_model(&self) -> Option<&Model> {
        self.compaction_model.as_ref()
    }

    /// Approximate token budget used to migrate one deprecated retained turn.
    pub(super) const LEGACY_COMPACTION_TOKENS_PER_TURN: u64 = 1_000;

    /// Configure autonomous compaction with the deprecated turn-count API.
    ///
    /// Each retained turn maps to a 1,000-token tail budget. Use
    /// [`Self::set_compaction_token_policy`] for exact token control.
    #[deprecated(note = "use set_compaction_token_policy")]
    pub fn set_compaction_policy(
        &mut self,
        enabled: bool,
        threshold_fraction: f64,
        keep_recent_turns: usize,
    ) -> Result<(), AgentError> {
        self.set_compaction_token_policy(
            enabled,
            threshold_fraction,
            u64::try_from(keep_recent_turns)
                .unwrap_or(u64::MAX)
                .saturating_mul(Self::LEGACY_COMPACTION_TOKENS_PER_TURN),
        )
    }

    /// Configure autonomous compaction with the deprecated turn-count API.
    #[deprecated(note = "use set_compaction_token_mode")]
    pub fn set_compaction_mode(
        &mut self,
        mode: AgentCompactionMode,
        threshold_fraction: f64,
        keep_recent_turns: usize,
    ) -> Result<(), AgentError> {
        self.set_compaction_token_mode(
            mode,
            threshold_fraction,
            u64::try_from(keep_recent_turns)
                .unwrap_or(u64::MAX)
                .saturating_mul(Self::LEGACY_COMPACTION_TOKENS_PER_TURN),
        )
    }

    /// Current deprecated turn-count compaction policy.
    #[deprecated(note = "use compaction_token_policy")]
    pub fn compaction_policy(&self) -> (bool, f64, usize) {
        let (enabled, threshold, tokens) = self.compaction_token_policy();
        (
            enabled,
            threshold,
            usize::try_from(tokens / Self::LEGACY_COMPACTION_TOKENS_PER_TURN)
                .unwrap_or(usize::MAX)
                .max(1),
        )
    }

    /// Configure autonomous context compaction for subsequent runs.
    ///
    /// `threshold_fraction` is the fraction of the complete model context
    /// available to current input plus the independently resolved compaction
    /// reserve. The default `1.0` therefore adds no percentage buffer.
    /// `keep_recent_tokens` is the approximate verbatim tail budget; when the
    /// retained tail alone exceeds the configured threshold or capacity,
    /// recovery advances the boundary until the request fits.
    pub fn set_compaction_token_policy(
        &mut self,
        enabled: bool,
        threshold_fraction: f64,
        keep_recent_tokens: u64,
    ) -> Result<(), AgentError> {
        self.set_compaction_token_mode(
            if enabled {
                AgentCompactionMode::Local
            } else {
                AgentCompactionMode::Disabled
            },
            threshold_fraction,
            keep_recent_tokens,
        )
    }

    /// Configure the autonomous compaction strategy for subsequent runs.
    pub fn set_compaction_token_mode(
        &mut self,
        mode: AgentCompactionMode,
        threshold_fraction: f64,
        keep_recent_tokens: u64,
    ) -> Result<(), AgentError> {
        if !threshold_fraction.is_finite() || threshold_fraction <= 0.0 || threshold_fraction > 1.0
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "threshold fraction must be finite and between 0 and 1".to_owned(),
            ));
        }
        if keep_recent_tokens == 0 {
            return Err(AgentError::InvalidCompactionPolicy(
                "keep_recent_tokens must be at least 1".to_owned(),
            ));
        }
        if mode == AgentCompactionMode::NativeResponses
            && self.model.spec.protocol != Protocol::OpenAiResponses
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "native Responses compaction requires an OpenAI Responses model route".to_owned(),
            ));
        }
        if mode == AgentCompactionMode::NativeResponses
            && self
                .session
                .responses_replay_snapshot(&self.model.endpoint.id, &self.model.spec.id)?
                .is_none()
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "native Responses compaction requires complete route-affine opaque replay on the active branch"
                    .to_owned(),
            ));
        }
        self.auto_compaction_mode = mode;
        self.compaction_threshold_fraction = threshold_fraction;
        self.compaction_keep_recent_tokens = keep_recent_tokens;
        self.sync_delegation_runtime_settings();
        Ok(())
    }

    /// Current autonomous compaction policy `(enabled, threshold, keep)`.
    pub fn compaction_token_policy(&self) -> (bool, f64, u64) {
        (
            self.auto_compaction_mode != AgentCompactionMode::Disabled,
            self.compaction_threshold_fraction,
            self.compaction_keep_recent_tokens,
        )
    }

    /// Current autonomous compaction strategy.
    pub fn compaction_mode(&self) -> AgentCompactionMode {
        self.auto_compaction_mode
    }

    /// Minimum output headroom used by autonomous capacity checks.
    pub fn compaction_reserve_tokens(&self) -> u64 {
        agent_compaction_reserve_tokens(&self.model, &self.reasoning)
    }

    /// Perform one idle local compaction through awaited session interception.
    /// The host idle driver owns this call; a transport task must not mutate the
    /// Agent directly. A successful return follows the durable checkpoint and
    /// its after-hook. After-hook errors explicitly retain the committed ID.
    pub async fn compact_session_with_instructions(
        &mut self,
        instructions: Option<&str>,
        cancellation: CancellationToken,
        mut on_event: impl FnMut(AgentEvent),
    ) -> Result<CompactionInfo, AgentError> {
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let instructions = instructions
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if instructions.is_some_and(|value| {
            value.len() > 16 * 1024
                || value
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        }) {
            return Err(AgentError::InvalidCompactionPolicy(
                "invalid compaction instructions".into(),
            ));
        }
        if self.auto_compaction_mode == AgentCompactionMode::NativeResponses {
            return Err(AgentError::InvalidCompactionPolicy(
                "cancellable manual session compaction requires local mode".into(),
            ));
        }
        self.cache_warmer
            .cancel(&mut self.session, "manual session compaction")?;
        let started = std::time::Instant::now();
        let usage_start = self.session.usage_records().len();
        let cost_before = self.session.total_cost_microdollars();
        let first_kept = choose_first_kept_by_tokens(
            &self.session,
            self.compaction_keep_recent_tokens,
            |message| estimate_messages_tokens(std::slice::from_ref(message)),
        )?
        .filter(|id| {
            prepare_handoff(&self.session, id).is_ok_and(|preparation| {
                !preparation.messages.is_empty() || !preparation.turn_prefix_messages.is_empty()
            })
        })
        .or_else(|| turn_starts(&self.session).get(1).cloned())
        .ok_or_else(|| AgentError::InvalidCompactionPolicy("no safe history to compact".into()))?;
        let replacement = intercept_compaction(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            &first_kept,
            SessionCompactionReason::Manual,
            instructions,
            &cancellation,
        )
        .await?;
        let from_extension = replacement.is_some();
        let first_kept = replacement
            .as_ref()
            .map_or(first_kept, |value| value.first_kept.clone());
        let preparation = prepare_handoff(&self.session, &first_kept)?;
        if preparation.messages.is_empty() && preparation.turn_prefix_messages.is_empty() {
            return Err(AgentError::InvalidCompactionPolicy(
                "compaction would make no progress".into(),
            ));
        }
        let source_head = self.session.head();
        let summary = match replacement {
            Some(replacement) => {
                validate_compaction_summary_part(&replacement.summary)?;
                replacement.summary
            }
            None => {
                let model = self
                    .compaction_model
                    .clone()
                    .unwrap_or_else(|| self.model.clone());
                let system = match instructions {
                    Some(instructions) => format!("{SUMMARIZATION_SYSTEM_PROMPT}\n\nAdditional user instructions for this handoff:\n{instructions}"),
                    None => SUMMARIZATION_SYSTEM_PROMPT.to_owned(),
                };
                let mut summary = if preparation.messages.is_empty() {
                    preparation
                        .previous_summary
                        .clone()
                        .unwrap_or_else(|| "No prior history.".into())
                } else {
                    self.summarize_with_retry(
                        &model,
                        &system,
                        vec![build_handoff_message(&preparation)],
                        SUMMARY_OUTPUT_TOKENS,
                        cancellation.clone(),
                        &mut on_event,
                    )
                    .await?
                };
                if !preparation.turn_prefix_messages.is_empty() {
                    let prefix = self
                        .summarize_with_retry(
                            &model,
                            &system,
                            vec![build_turn_prefix_handoff_message(
                                &preparation.turn_prefix_messages,
                            )],
                            TURN_PREFIX_OUTPUT_TOKENS,
                            cancellation.clone(),
                            &mut on_event,
                        )
                        .await?;
                    append_compaction_turn_prefix(&mut summary, &prefix)?;
                }
                finish_validated_compaction_handoff(summary, &preparation.details)?
            }
        };
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if self.session.head() != source_head {
            return Err(session_operation_error(SessionOperationError::StaleSource));
        }
        // Summary calls can append real accounting, but never change the source
        // branch. The sole writer's append fence rejects concurrent file writers.
        self.session.compact_with_details(
            summary.clone(),
            first_kept.clone(),
            preparation.details,
        )?;
        let mut usage = Usage::default();
        let records = &self.session.usage_records()[usage_start..];
        for record in records {
            add_usage(&mut usage, &record.usage);
        }
        let cost_microdollars = (!from_extension
            && records
                .iter()
                .all(|record| record.cost.is_some() || record.cost_microdollars.is_some()))
        .then(|| {
            self.session
                .total_cost_microdollars()
                .saturating_sub(cost_before)
        });
        let info = CompactionInfo {
            kind: CompactionKind::Local,
            summary,
            first_kept,
            usage,
            elapsed: started.elapsed(),
            cost_microdollars,
        };
        observe_compaction(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            SessionCompactionReason::Manual,
            from_extension,
            &cancellation,
        )
        .await?;
        Ok(info)
    }

    /// Navigate an existing session tree at the host's idle boundary. Before
    /// hooks may veto; the after-event follows the real synced head record.
    pub async fn navigate_session_tree(
        &mut self,
        target: Option<EntryId>,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        if let Some(id) = &target {
            if self.session.entry(id).is_none() {
                return Err(SessionError::UnknownEntry(id.clone()).into());
            }
        }
        self.cache_warmer
            .cancel(&mut self.session, "session tree navigation")?;
        let old_head = self.session.head();
        let before = SessionOperation::BeforeTree {
            target_id: target.clone(),
            old_head: old_head.clone(),
            preparation: None,
        };
        match run_session_operation_hooks(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            &before,
            &cancellation,
            SESSION_OPERATION_TIMEOUT,
        )
        .await
        .map_err(session_operation_error)?
        {
            SessionOperationDecision::Continue => {}
            SessionOperationDecision::Cancel => {
                return Err(AgentError::InvalidCompactionPolicy(
                    "tree navigation cancelled by extension".into(),
                ))
            }
            SessionOperationDecision::ReplaceCompaction { .. } => {
                unreachable!("driver validates event-specific decisions")
            }
        }
        let revision =
            SessionSourceRevision::capture(&self.session).map_err(session_operation_error)?;
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        revision
            .validate(&self.session)
            .map_err(session_operation_error)?;
        match &target {
            Some(id) => self.session.checkout(id.clone())?,
            None => self.session.checkout_root()?,
        }
        run_session_operation_hooks(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            &SessionOperation::Tree {
                old_head,
                new_head: target,
                summary_entry: None,
            },
            &cancellation,
            SESSION_OPERATION_TIMEOUT,
        )
        .await
        .map_err(|_| {
            AgentError::InvalidCompactionPolicy(
                "tree navigation committed; post-commit session hook failed; do not retry".into(),
            )
        })?;
        Ok(())
    }

    /// Navigate a selected tree entry, optionally carrying only the branch
    /// being left into a durable summary. User/custom entries select their
    /// parent and return their text to the editor; selecting the head is a no-op.
    /// Failure before commit never moves the head. A post-commit hook failure
    /// explicitly reports that navigation is already durable and must not retry.
    pub async fn navigate_session_tree_with_summary(
        &mut self,
        target: EntryId,
        summarize: bool,
        custom_instructions: Option<&str>,
        cancellation: CancellationToken,
        on_event: impl FnMut(AgentEvent),
    ) -> Result<TreeNavigationResult, AgentError> {
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let old_head = self.session.head();
        if old_head.as_ref() == Some(&target) {
            return Ok(TreeNavigationResult::default());
        }
        let selected = self
            .session
            .entry(&target)
            .ok_or_else(|| SessionError::UnknownEntry(target.clone()))?;
        let (new_parent, editor_text) = match &selected.value {
            EntryValue::Message(Message::User(user))
                if !user
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::ToolResult(_))) =>
            {
                let text = user
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                (selected.parent.clone(), Some(text))
            }
            _ => (Some(target.clone()), None),
        };
        let instructions = custom_instructions
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if instructions.is_some_and(|text| {
            text.len() > 16 * 1024
                || text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        }) {
            return Err(AgentError::InvalidCompactionPolicy(
                "invalid branch summary instructions".into(),
            ));
        }
        self.cache_warmer
            .cancel(&mut self.session, "session tree navigation")?;
        let branch = session_operation_branch(&self.session).map_err(session_operation_error)?;
        let source_ids = branch.iter().map(|entry| &entry.id).collect::<HashSet<_>>();
        let mut common_ancestor_id = Some(target.clone());
        while let Some(id) = common_ancestor_id.as_ref() {
            if source_ids.contains(id) {
                break;
            }
            common_ancestor_id = self
                .session
                .entry(id)
                .expect("validated session ancestry")
                .parent
                .clone();
        }
        let span_start = common_ancestor_id.as_ref().map_or(0, |id| {
            branch
                .iter()
                .position(|entry| &entry.id == id)
                .expect("shared source ancestor")
                + 1
        });
        let entries_to_summarize = branch[span_start..].to_vec();
        let mut messages = Vec::new();
        let mut inherited = crate::compaction::CompactionDetails::default();
        for entry in &entries_to_summarize {
            match &entry.value {
                EntryValue::Message(message) => messages.push(message.clone()),
                EntryValue::BranchSummary {
                    summary, details, ..
                } => {
                    messages.push(crate::session::branch_summary_message(summary));
                    inherited
                        .read_files
                        .extend(details.read_files.iter().cloned());
                    inherited
                        .modified_files
                        .extend(details.modified_files.iter().cloned());
                }
                EntryValue::Compaction {
                    summary,
                    snapcompact,
                    details,
                    ..
                } => {
                    let text = snapcompact.as_ref().map_or(summary.as_str(), |checkpoint| {
                        checkpoint.source_text.as_str()
                    });
                    messages.push(Message::User(UserMessage {
                        content: vec![UserPart::Text(format!(
                            "[summary of earlier conversation]\n{text}"
                        ))],
                    }));
                    inherited
                        .read_files
                        .extend(details.read_files.iter().cloned());
                    inherited
                        .modified_files
                        .extend(details.modified_files.iter().cloned());
                }
                _ => {}
            }
        }
        let preparation = crate::compaction::prepare_branch_handoff(messages, &inherited);
        let before = SessionOperation::BeforeTree {
            target_id: Some(target),
            old_head: old_head.clone(),
            preparation: Some(crate::compaction::TreeNavigationPreparation {
                common_ancestor_id,
                entries_to_summarize,
                user_wants_summary: summarize,
                custom_instructions: instructions.map(str::to_owned),
            }),
        };
        match run_session_operation_hooks(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            &before,
            &cancellation,
            SESSION_OPERATION_TIMEOUT,
        )
        .await
        .map_err(session_operation_error)?
        {
            SessionOperationDecision::Continue => {}
            SessionOperationDecision::Cancel => {
                return Err(AgentError::InvalidCompactionPolicy(
                    "tree navigation cancelled by extension".into(),
                ))
            }
            SessionOperationDecision::ReplaceCompaction { .. } => {
                unreachable!("driver validates event-specific decisions")
            }
        }
        let revision =
            SessionSourceRevision::capture(&self.session).map_err(session_operation_error)?;
        let summary = if summarize && !preparation.messages.is_empty() {
            Some(
                self.summarize_branch_with_instructions(
                    &preparation,
                    instructions,
                    cancellation.clone(),
                    on_event,
                )
                .await?,
            )
        } else {
            None
        };
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        revision
            .validate_tree(&self.session)
            .map_err(session_operation_error)?;
        let summary_entry = match summary {
            Some(summary) => Some(self.session.branch_with_summary_from(
                new_parent,
                old_head.clone().expect("nonempty source span has a head"),
                summary,
                preparation.details,
            )?),
            None => {
                match new_parent {
                    Some(id) => self.session.checkout(id)?,
                    None => self.session.checkout_root()?,
                }
                None
            }
        };
        let committed_head = self.session.head();
        let committed_summary = summary_entry.as_ref().map(|id| {
            self.session
                .entry(id)
                .expect("summary just committed")
                .clone()
        });
        run_session_operation_hooks(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            &SessionOperation::Tree {
                old_head,
                new_head: committed_head.clone(),
                summary_entry: committed_summary,
            },
            &cancellation,
            SESSION_OPERATION_TIMEOUT,
        )
        .await
        .map_err(|_| {
            AgentError::InvalidCompactionPolicy(format!(
                "tree navigation committed at {}; post-commit session hook failed; do not retry",
                committed_head.as_ref().map_or("root", |id| id.0.as_str()),
            ))
        })?;
        Ok(TreeNavigationResult {
            editor_text,
            summary_entry,
        })
    }

    /// Runs a tool-free summary through the same cancellable retry, hard-budget,
    /// telemetry and durable usage path as autonomous compaction. Retries are
    /// delivered as `ProviderOperationRetry`, not compaction failures. The
    /// caller commits the returned summary once, after this method succeeds.
    pub async fn summarize_with_retry(
        &mut self,
        model: &Model,
        system: &str,
        messages: Vec<Message>,
        output_tokens: u64,
        cancellation: CancellationToken,
        on_event: impl FnMut(AgentEvent),
    ) -> Result<String, AgentError> {
        self.summary_call(
            model,
            system,
            messages,
            output_tokens,
            crate::events::ProviderOperation::LocalCompaction,
            cancellation,
            on_event,
        )
        .await
    }

    /// Produces a structured abandoned-branch handoff using the same retry and
    /// accounting consumer as compaction, with branch-specific retry identity.
    /// It does not checkout or append a summary; the caller owns that one commit.
    pub async fn summarize_branch_with_retry(
        &mut self,
        preparation: &crate::compaction::BranchHandoffPreparation,
        cancellation: CancellationToken,
        on_event: impl FnMut(AgentEvent),
    ) -> Result<String, AgentError> {
        self.summarize_branch_with_instructions(preparation, None, cancellation, on_event)
            .await
    }

    async fn summarize_branch_with_instructions(
        &mut self,
        preparation: &crate::compaction::BranchHandoffPreparation,
        instructions: Option<&str>,
        cancellation: CancellationToken,
        on_event: impl FnMut(AgentEvent),
    ) -> Result<String, AgentError> {
        let model = self
            .compaction_model
            .clone()
            .unwrap_or_else(|| self.model.clone());
        let system = instructions.map_or_else(
            || SUMMARIZATION_SYSTEM_PROMPT.to_owned(),
            |instructions| {
                format!("{SUMMARIZATION_SYSTEM_PROMPT}\n\nAdditional focus: {instructions}")
            },
        );
        let summary = self
            .summary_call(
                &model,
                &system,
                vec![crate::compaction::build_branch_handoff_message(preparation)],
                SUMMARY_OUTPUT_TOKENS,
                crate::events::ProviderOperation::BranchSummary,
                cancellation,
                on_event,
            )
            .await?;
        let handoff = crate::compaction::finish_branch_handoff(summary, &preparation.details);
        validate_compaction_summary_part(&handoff)?;
        Ok(handoff)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn summary_call(
        &mut self,
        model: &Model,
        system: &str,
        messages: Vec<Message>,
        output_tokens: u64,
        operation: crate::events::ProviderOperation,
        cancellation: CancellationToken,
        mut on_event: impl FnMut(AgentEvent),
    ) -> Result<String, AgentError> {
        if output_tokens == 0 {
            return Err(AgentError::InvalidCompactionPolicy(
                "summary output limit must be positive".into(),
            ));
        }
        let abort = AbortFlag {
            cancellation,
            ..AbortFlag::default()
        };
        let (events, mut receiver) = mpsc::unbounded_channel();
        let tracker = ContextTracker::default();
        let breakdown = self.request_context_breakdown()?;
        let (tool_generation, _) = self.extensions.tool_snapshot();
        let mut capacity = ContextCapacityCache::seeded(&self.session, tool_generation, &breakdown);
        let mut usage = Usage::default();
        let mut cost = CostAccumulator::default();
        let mut context = CompactionContext {
            run_id: &self.session_id,
            resource_owner: &self.resource_owner,
            retry_hooks: &self.extensions.provider_retry_hooks,
            compaction_strategy: self.extensions.compaction_strategy.as_ref(),
            session_operation_hooks: &self.extensions.session_operation_hooks,
            max_network_wait: self.max_network_wait,
            provider_retries_enabled: self.provider_retries_enabled,
            client: &self.client,
            model: &self.model,
            compaction_model: model,
            summary_operation: operation,
            session: &mut self.session,
            usage: &mut usage,
            run_cost: &mut cost,
            cache_retention: self.cache_retention,
            reasoning: &self.reasoning,
            reasoning_mode: self.reasoning_mode,
            session_id: &self.session_id,
            max_session_tokens: self.max_session_tokens,
            max_session_cost_microdollars: self.max_session_cost_microdollars,
            abort: &abort,
            mode: self.auto_compaction_mode,
            threshold_fraction: self.compaction_threshold_fraction,
            keep_recent_tokens: self.compaction_keep_recent_tokens,
            events: &events,
            context: &tracker,
            tool_generation,
            capacity: &mut capacity,
            telemetry: self.telemetry.clone(),
        };
        // Keep the provider/retry future off the caller's state machine: manual
        // compaction is polled beneath Serve's session and command futures on a
        // normal Tokio worker stack.
        let mut call = Box::pin(context.call(system, messages, output_tokens));
        let result = loop {
            tokio::select! {
                biased;
                event = receiver.recv() => if let Some(event) = event { on_event(event); },
                result = &mut call => break result,
            }
        };
        while let Ok(event) = receiver.try_recv() {
            on_event(event);
        }
        result?.ok_or_else(|| AgentError::IncompleteResponse {
            stop_reason: "summary did not finish normally".into(),
        })
    }

    /// Performs one native Responses compaction while the agent is idle.
    ///
    /// The complete unpruned provider output is durably appended as a
    /// route-affine branch checkpoint and becomes the next replay base.
    pub async fn compact_responses_native(&mut self) -> Result<CompactionInfo, AgentError> {
        if !self.extensions.session_operation_hooks.is_empty() {
            let first_kept = self.session.head().ok_or_else(|| {
                AgentError::InvalidCompactionPolicy("no history to compact".into())
            })?;
            if intercept_compaction(
                &mut self.session,
                &self.extensions.session_operation_hooks,
                &first_kept,
                SessionCompactionReason::Manual,
                None,
                &CancellationToken::default(),
            )
            .await?
            .is_some()
            {
                return Err(AgentError::InvalidCompactionPolicy(
                    "text replacement cannot replace a native Responses checkpoint".into(),
                ));
            }
        }
        if self.model.spec.protocol != Protocol::OpenAiResponses {
            return Err(AgentError::InvalidCompactionPolicy(
                "native Responses compaction requires an OpenAI Responses model route".to_owned(),
            ));
        }
        if current_head_is_native_checkpoint(&self.session, &self.model) {
            return Err(AgentError::InvalidCompactionPolicy(
                "native Responses compaction made no progress since the previous checkpoint"
                    .to_owned(),
            ));
        }
        let replay = self
            .session
            .responses_replay_snapshot(&self.model.endpoint.id, &self.model.spec.id)?
            .ok_or_else(|| {
                AgentError::InvalidCompactionPolicy(
                    "native Responses compaction requires complete route-affine opaque replay"
                        .to_owned(),
                )
            })?;
        if replay
            .iter()
            .any(|item| matches!(item, ResponsesReplayItem::ConfigurationUpdate(_)))
        {
            return Err(AgentError::InvalidCompactionPolicy(
                "standalone native compact cannot preserve reasoning updates; use local compaction"
                    .into(),
            ));
        }
        let input = octet_ai::responses::encode_responses_replay(&self.model, None, &replay)?;
        let active_system = self.system.clone();
        let instructions = (!active_system.is_empty()).then_some(active_system.as_str());
        let tools = self.extensions.model_tool_definitions(&self.resource_owner);
        require_tool_schema_budget(&tools, self.tool_schema_budget_bytes)?;
        if replay.is_empty() {
            return Err(AgentError::InvalidCompactionPolicy(
                "native Responses compaction requires non-empty replay".to_owned(),
            ));
        }
        let request = ResponsesCompactRequest::for_model(
            &self.model,
            input,
            instructions.map(str::to_owned),
            &tools,
            &self.reasoning,
            self.reasoning_mode,
            &OutputFormat::Text,
            self.cache_retention,
            Some(&self.session_id),
        )?;
        let input_tokens = estimate_compact_request_tokens(&request, &replay);
        require_enforceable_output_cap(
            &self.session,
            None,
            self.max_session_tokens,
            self.max_session_cost_microdollars,
        )?;
        reserve_request_tokens(
            &self.session,
            input_tokens,
            self.model.spec.limits.max_output_tokens,
            self.max_session_tokens,
        )?;
        reserve_request_cost(
            &self.session,
            &self.model,
            input_tokens,
            self.model.spec.limits.max_output_tokens,
            self.max_session_cost_microdollars,
            self.cache_retention,
        )?;
        let covered_through = self.session.head().ok_or(SessionError::EmptySession)?;
        let operation_started = std::time::Instant::now();
        let abort = AbortFlag::default();
        let (events, _receiver) = mpsc::unbounded_channel();
        let mut cost = None;
        let response = recover_auxiliary(
            AuxiliaryRecovery {
                dispatch: AuxiliaryDispatch::default(),
                session: &mut self.session,
                run_id: &self.session_id,
                resource_owner: &self.resource_owner,
                retry_hooks: &self.extensions.provider_retry_hooks,
                max_network_wait: self.max_network_wait,
                model: &self.model,
                qualified: self.model.endpoint.runtime.responses_profile
                    == octet_ai::ResponsesRuntimeProfile::Codex
                    && request.input.items().iter().all(|item| {
                        matches!(
                            item.as_json()
                                .get("type")
                                .and_then(serde_json::Value::as_str),
                            Some(
                                "message"
                                    | "reasoning"
                                    | "function_call"
                                    | "function_call_output"
                                    | "compaction"
                            )
                        )
                    }),

                enabled: self.provider_retries_enabled,
                hard_budget: self.max_session_tokens.is_some()
                    || self.max_session_cost_microdollars.is_some(),
                exposure: None, // Compact has no provider-enforced output cap.
                input_tokens,
                output_tokens: self.model.spec.limits.max_output_tokens,
                token_limit: self.max_session_tokens,
                cost_limit: self.max_session_cost_microdollars,
                retention: self.cache_retention,
                abort: &abort,
                events: &events,
                operation: crate::events::ProviderOperation::NativeCompaction,
                session_id: &self.session_id,
            },
            |deadline, dispatch| {
                auxiliary_compact(
                    &self.client,
                    &self.model,
                    request.clone(),
                    deadline,
                    self.max_network_wait,
                    dispatch,
                )
            },
            |session, response| {
                cost =
                    self.model.spec.pricing.as_ref().and_then(|pricing| {
                        octet_ai::pricing::cost_of(pricing, &response.usage).ok()
                    });
                session.record_compaction_usage(
                    self.model.endpoint.id.clone(),
                    self.model.spec.id.clone(),
                    response.usage,
                    cost,
                )?;
                Ok(())
            },
        )
        .await?;
        validate_native_compact_output(&response.output)?;
        let checkpoint = self.session.append_responses_compaction(
            self.model.endpoint.id.clone(),
            self.model.spec.id.clone(),
            response.output,
        )?;
        observe_compaction(
            &mut self.session,
            &self.extensions.session_operation_hooks,
            SessionCompactionReason::Manual,
            false,
            &CancellationToken::default(),
        )
        .await?;
        Ok(CompactionInfo {
            kind: CompactionKind::NativeResponses {
                checkpoint,
                covered_through: covered_through.clone(),
            },
            summary: String::new(),
            first_kept: covered_through,
            usage: response.usage,
            elapsed: operation_started.elapsed(),
            cost_microdollars: cost.map(|cost| cost.total),
        })
    }
}
