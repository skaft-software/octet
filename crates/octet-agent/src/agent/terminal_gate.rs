//! The terminal gate that reviews a final answer before the run ends.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalGateDecision {
    Return,
    Continue,
}

#[derive(Debug, Serialize)]
pub(super) struct TerminalActionReceipt {
    pub(super) tool: String,
    pub(super) arguments: String,
    pub(super) status: &'static str,
    pub(super) result: String,
}

/// Lossy gate-only evidence, never the authoritative input or tool result.
/// Natural runs have no instance and perform none of these projections.
#[derive(Default)]
pub(super) struct TerminalGateEvidence {
    pub(super) prior_context: String,
    pub(super) requests: VecDeque<String>,
    pub(super) request_bytes: usize,
    pub(super) requests_omitted: usize,
    pub(super) receipts: VecDeque<TerminalActionReceipt>,
    pub(super) actions_omitted: usize,
}

impl TerminalGateEvidence {
    pub(super) fn for_run(
        policy: CompletionPolicy,
        session: &Session,
        input: &UserInput,
    ) -> Result<Option<Self>, SessionError> {
        if policy != CompletionPolicy::TerminalGate {
            return Ok(None);
        }
        let mut evidence = Self {
            prior_context: recent_conversational_context(&session.context()?),
            ..Self::default()
        };
        #[cfg(test)]
        TERMINAL_GATE_INITIAL_SUMMARIES.with(|count| count.set(count.get() + 1));
        evidence.record_request(&input.text_summary());
        Ok(Some(evidence))
    }

    pub(super) fn record_request(&mut self, summary: &str) {
        let summary = bounded_gate_text(summary, TERMINAL_GATE_TEXT_LIMIT);
        while self.requests.len() >= TERMINAL_GATE_REQUEST_LIMIT
            || self.request_bytes + summary.len() > TERMINAL_GATE_REQUEST_BYTES
        {
            // The initial request is never evicted; the budget fits it plus
            // the incoming latest request even at four UTF-8 bytes per char.
            let removed = self.requests.remove(1).expect("initial and latest fit");
            self.request_bytes -= removed.len();
            self.requests_omitted += 1;
        }
        self.request_bytes += summary.len();
        self.requests.push_back(summary);
    }

    pub(super) fn record_action(
        &mut self,
        tool: &str,
        arguments: &str,
        is_error: bool,
        result: &str,
    ) {
        if self.receipts.len() == TERMINAL_GATE_RECEIPT_LIMIT {
            // Exactly the original capsule's first 12 plus rolling last 12,
            // in delivery order, without retaining the intervening payloads.
            let _ = self.receipts.remove(TERMINAL_GATE_RECEIPT_LIMIT / 2);
            self.actions_omitted += 1;
        }
        self.receipts.push_back(TerminalActionReceipt {
            tool: bounded_gate_text(tool, TERMINAL_GATE_TOOL_NAME_LIMIT),
            arguments: bounded_gate_text(arguments, TERMINAL_GATE_ARGUMENT_LIMIT),
            status: if is_error { "error" } else { "ok" },
            result: bounded_gate_text(result, TERMINAL_GATE_RESULT_LIMIT),
        });
    }
}

pub(super) fn bounded_gate_text(text: &str, max_chars: usize) -> String {
    #[cfg(test)]
    TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(count.get() + 1));
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let half = max_chars.saturating_sub(32) / 2;
    let head = text.chars().take(half).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(half)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head}\n[… {count} chars total …]\n{tail}")
}

pub(super) fn message_visible_text(message: &Message) -> Option<String> {
    let text = match message {
        Message::User(user) => user
            .content
            .iter()
            .filter_map(|part| match part {
                UserPart::Text(text) => Some(text.as_str()),
                UserPart::Media(Media::Audio(audio)) => audio
                    .transcript
                    .as_deref()
                    .filter(|transcript| !transcript.trim().is_empty())
                    .or(Some("[audio]")),
                UserPart::Media(Media::Image(_)) => Some("[image]"),
                UserPart::ToolResult(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .filter_map(|part| match part {
                AssistantPart::Text(text) => Some(text.as_str()),
                AssistantPart::Media(Media::Audio(audio)) => audio
                    .transcript
                    .as_deref()
                    .filter(|transcript| !transcript.trim().is_empty())
                    .or(Some("[generated audio]")),
                AssistantPart::Media(Media::Image(_)) => Some("[generated image]"),
                AssistantPart::Reasoning(_)
                | AssistantPart::ProviderMetadata(_)
                | AssistantPart::ToolCall(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (!text.trim().is_empty()).then_some(text)
}

pub(super) fn recent_conversational_context(messages: &[Message]) -> String {
    let mut selected = messages
        .iter()
        .rev()
        .filter_map(message_visible_text)
        .take(2)
        .collect::<Vec<_>>();
    selected.reverse();
    bounded_gate_text(&selected.join("\n---\n"), TERMINAL_GATE_TEXT_LIMIT)
}

pub(super) fn terminal_gate_capsule(
    evidence: &TerminalGateEvidence,
    candidate: &AssistantMessage,
) -> String {
    let candidate =
        message_visible_text(&Message::Assistant(candidate.clone())).unwrap_or_default();
    let capsule = serde_json::json!({
        "prior_context": evidence.prior_context,
        "requests": evidence.requests,
        "requests_omitted": evidence.requests_omitted,
        "candidate": bounded_gate_text(&candidate, TERMINAL_GATE_TEXT_LIMIT),
        "actions_omitted": evidence.actions_omitted,
        "actions": evidence.receipts,
    })
    .to_string();
    debug_assert!(capsule.len() <= TERMINAL_GATE_CAPSULE_BYTES);
    capsule
}

pub(super) fn parse_terminal_gate(response: &octet_ai::Response) -> Option<TerminalGateDecision> {
    if !matches!(
        response.stop_reason,
        StopReason::EndTurn | StopReason::StopSequence
    ) {
        return None;
    }
    match assistant_text(response)?.trim() {
        "R" => Some(TerminalGateDecision::Return),
        "C" => Some(TerminalGateDecision::Continue),
        _ => None,
    }
}

pub(super) struct TerminalGateContext<'a> {
    pub(super) run_id: &'a str,
    pub(super) resource_owner: &'a str,
    pub(super) retry_hooks: &'a [Arc<dyn ProviderRetryHook>],
    pub(super) max_network_wait: Option<Duration>,
    pub(super) provider_retries_enabled: bool,
    pub(super) events: &'a mpsc::UnboundedSender<AgentEvent>,
    pub(super) client: &'a AiClient,
    pub(super) model: &'a Model,
    pub(super) session: &'a mut Session,
    pub(super) usage: &'a mut Usage,
    pub(super) run_cost: &'a mut CostAccumulator,
    pub(super) cache_retention: CacheRetention,
    pub(super) session_id: &'a str,
    pub(super) max_session_tokens: Option<u64>,
    pub(super) max_session_cost_microdollars: Option<u64>,
    pub(super) abort: &'a AbortFlag,
}

impl TerminalGateContext<'_> {
    pub(super) async fn decide(
        &mut self,
        capsule: String,
    ) -> Result<TerminalGateDecision, AgentError> {
        for _ in 0..TERMINAL_GATE_ATTEMPTS {
            let request = Request {
                system: Some(TERMINAL_GATE_SYSTEM.to_owned()),
                messages: vec![Message::User(UserMessage {
                    content: vec![UserPart::Text(capsule.clone())],
                })],
                tools: Vec::new(),
                tool_choice: ToolChoice::None,
                max_output_tokens: Some(1),
                temperature: Some(0.0),
                stop: Vec::new(),
                reasoning: octet_ai::select_auxiliary_reasoning(self.model)?,
                reasoning_mode: ReasoningMode::Standard,
                responses: None,
                output_format: OutputFormat::Text,
                output_modalities: OutputModalities::Text,
                compatibility: CompatibilityMode::Strict,
                cache_retention: self.cache_retention,
                session_id: Some(format!("{}:terminal-gate", self.session_id)),
            };
            let input_tokens = estimate_request_tokens(
                request.system.as_deref().unwrap_or_default(),
                &request.messages,
                &request.tools,
            );
            let budget = self.model.spec.limits.context_window.saturating_sub(1);
            if input_tokens > budget {
                return Err(AgentError::ContextExceeded {
                    estimate: input_tokens,
                    budget,
                });
            }
            let reserved_output_tokens = reservation_output_tokens(
                self.session,
                self.model,
                1,
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
                self.model,
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
                    model: self.model,
                    qualified: qualified_inference_replacement(self.model, &request),
                    enabled: self.provider_retries_enabled,
                    hard_budget: self.max_session_tokens.is_some()
                        || self.max_session_cost_microdollars.is_some(),
                    exposure: request_uncertainty_bound(
                        self.model,
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
                    operation: crate::events::ProviderOperation::TerminalGate,
                    session_id: self.session_id,
                },
                |deadline, dispatch| {
                    auxiliary_complete(
                        self.client,
                        self.model,
                        request.clone(),
                        deadline,
                        self.max_network_wait,
                        dispatch,
                    )
                },
                |session, response| {
                    session.record_terminal_gate_usage(
                        self.model.endpoint.id.clone(),
                        self.model.spec.id.clone(),
                        response.usage,
                        response.cost,
                        parse_terminal_gate(response)
                            .map(|decision| decision == TerminalGateDecision::Return),
                    )?;
                    add_usage(self.usage, &response.usage);
                    self.run_cost.add(response.cost);
                    Ok(())
                },
            )
            .await?;
            if self.abort.is_set() {
                return Err(AgentError::Cancelled);
            }
            let decision = parse_terminal_gate(&response);
            if let Some(decision) = decision {
                return Ok(decision);
            }
        }
        Err(AgentError::IncompleteResponse {
            stop_reason: "terminal gate returned neither R nor C after two attempts".to_owned(),
        })
    }
}
