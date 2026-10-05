//! Provider failure classification, retry and replay recovery.

use super::*;

pub(super) fn retryable_before_generation(error: &AiError) -> bool {
    // A wrapper is replayable before generation only if its inner failure is.
    if let AiError::StreamFailure { inner, .. } = error {
        return retryable_before_generation(inner);
    }
    match error {
        AiError::NetworkUnavailable(_) => true,
        AiError::Http(error) => error.is_safe_to_retry(),
        AiError::Transport(error) => {
            !error.timeout && error.phase == octet_ai::TransportPhase::Connect
        }
        _ => false,
    }
}

pub(super) fn is_replayable_network_failure(error: &AiError) -> bool {
    // Wrapping a failure never changes its acceptance ambiguity.
    if let AiError::StreamFailure { inner, .. } = error {
        return is_replayable_network_failure(inner);
    }
    if matches!(error, AiError::NetworkUnavailable(_)) {
        return true;
    }
    matches!(
        error,
        AiError::Transport(transport)
            if !transport.timeout
                && transport.phase == octet_ai::TransportPhase::Connect
    )
}

pub(super) fn looks_like_context_error(error: &AiError) -> bool {
    // A mid-stream wrapper delegates classification to the failure that
    // actually ended the stream: a provider context-error frame inside a 2xx
    // stream must still be detected, while a wrapped transport timeout must
    // still never be mistaken for context overflow.
    if let AiError::StreamFailure { inner, .. } = error {
        return looks_like_context_error(inner);
    }
    // Transport timeouts often contain phrases such as "context deadline
    // exceeded". They are connectivity failures, not evidence that model
    // history is too large, and must never destroy full-fidelity context.
    if !matches!(
        error,
        AiError::Http(_) | AiError::Provider(_) | AiError::ResponsesFailed(_)
    ) {
        return false;
    }
    if matches!(error, AiError::Http(http) if http.status.as_u16() == 429)
        || matches!(
            error,
            AiError::Provider(provider) | AiError::ResponsesFailed(provider)
                if provider.code.as_deref().is_some_and(|code| {
                    let code = code.to_ascii_lowercase();
                    code.contains("rate_limit") || code.contains("throttl")
                }) || provider.kind.as_deref().is_some_and(|kind| {
                    let kind = kind.to_ascii_lowercase();
                    kind.contains("rate_limit") || kind.contains("throttl")
                })
        )
    {
        return false;
    }
    // Explicit auth/policy/quota codes outrank context-sounding text too.
    // Context length itself still owns the established compaction path.
    let (code, kind) = match error {
        AiError::Provider(provider) | AiError::ResponsesFailed(provider) => {
            (provider.code.as_deref(), provider.kind.as_deref())
        }
        AiError::Http(http) => (http.provider_code.as_deref(), None),
        _ => unreachable!("context candidates were narrowed above"),
    };
    // A bare request-size status is how a strict server reports a prompt that no
    // longer fits, so it must not veto the compaction path: a real local vLLM
    // answered HTTP 400 with `"type":"BadRequestError","code":400` plus
    // "maximum context length is 131072 tokens", and the numeric code selected
    // the permanent branch instead of compacting. Named policy/auth/quota codes
    // and every other 4xx still veto, because compaction cannot repair those.
    let recoverable_request_size_code = |code: &str| {
        code == "context_length_exceeded"
            || code
                .parse::<u16>()
                .ok()
                .is_some_and(|status| matches!(status, 400 | 413 | 422))
    };
    let veto = octet_ai::ProviderError {
        code: code
            .filter(|code| !recoverable_request_size_code(code))
            .map(str::to_owned),
        kind: kind
            .filter(|kind| !recoverable_request_size_code(kind))
            .map(str::to_owned),
        message: String::new(),
        request_id: None,
    };
    if veto.is_permanent() {
        return false;
    }
    let text = error.to_string().to_ascii_lowercase();
    [
        "context window exceeded",
        "context window exceeds",
        "context length exceeded",
        "context_length_exceeded",
        "model_context_window_exceeded",
        "maximum context length",
        "exceeds the context window",
        "exceeds model's maximum context length",
        "request_too_large",
        "too many tokens",
        "token limit",
        "prompt is too long",
        "input is too long",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(super) fn provider_requests_connection_refresh(error: &octet_ai::ProviderError) -> bool {
    let Some(code) = error.code.as_deref() else {
        return false;
    };
    let code = code.to_ascii_lowercase();
    code == "websocket_connection_limit_reached"
        || (code.contains("websocket") && code.contains("connection") && code.contains("limit"))
}

pub(super) fn permanent_provider_error(error: &octet_ai::ProviderError) -> bool {
    error.is_permanent()
}

pub(super) fn retryable_provider_error(error: &octet_ai::ProviderError) -> bool {
    if permanent_provider_error(error) {
        return false;
    }
    if provider_requests_connection_refresh(error) {
        // The provider rejected the generation because its long-lived socket
        // expired. The WebSocket pool retires that socket, so a retry opens a
        // fresh transport (or the safe HTTP fallback) before any generation.
        return true;
    }
    if error
        .code
        .as_deref()
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (500..600).contains(&code))
    {
        return true;
    }
    let text = format!(
        "{} {} {}",
        error.code.as_deref().unwrap_or_default(),
        error.kind.as_deref().unwrap_or_default(),
        error.message
    )
    .to_ascii_lowercase();
    [
        "rate_limit",
        "rate limit",
        "throttl",
        "overload",
        "temporarily_unavailable",
        "temporarily unavailable",
        "service_unavailable",
        "service unavailable",
        "server_error",
        "server error",
        "internal_error",
        "internal error",
        "timed out",
        "timeout",
        "try again",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(super) fn retryable_stream_start(error: &AiError) -> bool {
    if let AiError::StreamFailure { inner, .. } = error {
        return retryable_stream_start(inner);
    }
    // No visible output is not proof of nonacceptance. Only a safe connection
    // failure or an explicit provider rejection can authorize another request.
    retryable_before_generation(error)
        || matches!(error, AiError::Provider(provider) | AiError::ResponsesFailed(provider) if retryable_provider_error(provider))
}

pub(super) fn provider_retry_limit(error: &AiError) -> usize {
    // A mid-stream wrapper inherits the retry budget of the failure that
    // ended the stream, so introducing the wrapper changes no retry behavior.
    if let AiError::StreamFailure { inner, .. } = error {
        return provider_retry_limit(inner);
    }
    if !retryable_stream_start(error) {
        // Timeouts have consumed their deadline; post-send transport failures
        // and incomplete streams cannot establish nonacceptance. No budget is
        // available unless classification independently admits a safe replay.
        0
    } else if is_replayable_network_failure(error) {
        MAX_NETWORK_RETRIES
    } else {
        MAX_PROVIDER_RETRIES
    }
}

// Only the host-declared Codex runtime and ordinary local function generation
// qualify. Unknown opaque item kinds and server-side continuation/effect options
// fail closed. Neither model names nor absence of output establish eligibility.
pub(super) fn qualified_inference_replacement(model: &Model, request: &Request) -> bool {
    model.spec.protocol == octet_ai::Protocol::OpenAiResponses
        && model.endpoint.runtime.responses_profile == octet_ai::ResponsesRuntimeProfile::Codex
        && request.responses.as_ref().is_none_or(|options| {
            options.previous_response_id.is_none()
                && options.context_management.is_none()
                && !options.store
                && options.input.as_ref().is_none_or(|input| {
                    input.items().iter().all(|item| {
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
                    })
                })
        })
}

pub(super) fn interrupted_inference_error(error: &AiError) -> bool {
    match error {
        // This wrapper is codec-owned evidence that decoding failed inside an
        // already-open provider stream, not while validating a local request.
        // Pinned Codex skips malformed frame deserialization and retries its
        // terminal EOF/ResponseCompleted parse failures. Keep that recovery
        // authority bounded and qualified; structural/resource errors differ.
        AiError::StreamFailure { inner, .. } => {
            matches!(
                inner.as_ref(),
                AiError::Decode(
                    octet_ai::DecodeError::Json(_) | octet_ai::DecodeError::InvalidUtf8
                )
            ) || interrupted_inference_error(inner)
        }
        AiError::Transport(error) => matches!(
            error.phase,
            octet_ai::TransportPhase::Body | octet_ai::TransportPhase::ResponseHeaders
        ),
        AiError::Http(error) => error.status.as_u16() == 408 && error.is_safe_to_retry(),
        // Only terminal-boundary EOF errors qualify, never local request JSON,
        // UTF-8, schema-validation, resource-limit or state-machine violations.
        AiError::StreamProtocol(
            octet_ai::StreamProtocolError::MissingFinish
            | octet_ai::StreamProtocolError::PrematureEof
            | octet_ai::StreamProtocolError::ResponseNotResumable { .. },
        ) => true,
        AiError::ResponsesFailed(error) => !error.is_permanent(),
        AiError::Provider(error) => {
            !permanent_provider_error(error)
                && error
                    .code
                    .as_deref()
                    .into_iter()
                    .chain(error.kind.as_deref())
                    .any(|code| {
                        matches!(
                            code,
                            "server_error"
                                | "internal_error"
                                | "overloaded_error"
                                | "temporarily_unavailable"
                                | "service_unavailable"
                                | "websocket_connection_limit_reached"
                                | "rate_limit_exceeded"
                        )
                    })
        }
        _ => false,
    }
}

// Covers Codex's outer stream layer (six WS plus six HTTP logical attempts),
// not its nested HTTP admission sends. Never reset on transport changes.
pub(super) const MAX_INFERENCE_REPLACEMENTS: usize = 11;

// Five physical HTTP sends for each of six outer HTTP attempts. These are
// independent of streamed-generation replacements, with a shared finite cap
// covering six WS attempts plus thirty HTTP attempts at the Codex defaults.
pub(super) const MAX_OPENING_ADMISSION_REPLACEMENTS: usize = 29;

pub(super) const MAX_CUMULATIVE_PROVIDER_REPLACEMENTS: usize = 35;

pub(super) fn opening_transport_failure(error: &AiError) -> bool {
    match error {
        AiError::StreamFailure { inner, .. } => opening_transport_failure(inner),
        AiError::Transport(error) => matches!(
            error.phase,
            octet_ai::TransportPhase::Connect | octet_ai::TransportPhase::ResponseHeaders
        ),
        _ => false,
    }
}

pub(super) fn http_server_failure(error: &AiError) -> bool {
    match error {
        AiError::StreamFailure { inner, .. } => http_server_failure(inner),
        AiError::Http(error) => error.is_transient_server_error(),
        _ => false,
    }
}

pub(super) fn http_usage_unknown(error: &AiError) -> bool {
    match error {
        AiError::StreamFailure { inner, .. } => http_usage_unknown(inner),
        AiError::Http(error) => error.status.is_server_error() || error.status.as_u16() == 408,
        _ => false,
    }
}

#[derive(Default)]
pub(super) struct ProviderRecoveryBudget {
    pub(super) admission: usize,
    pub(super) stream: usize,
}

impl ProviderRecoveryBudget {
    pub(super) fn limit(&self, total: usize, recovery: &PendingProviderRecovery) -> usize {
        let consumed = if recovery.opening_admission() {
            self.admission
        } else if recovery.qualified && interrupted_inference_error(&recovery.error) {
            self.stream
        } else {
            total
        };
        total
            .saturating_add(recovery.replacement_limit().saturating_sub(consumed))
            .min(MAX_CUMULATIVE_PROVIDER_REPLACEMENTS)
    }

    pub(super) fn admit(&mut self, recovery: &PendingProviderRecovery) {
        if recovery.opening_admission() {
            self.admission += 1;
        } else {
            self.stream += 1;
        }
    }
}

pub(super) struct PendingProviderRecovery {
    pub(super) error: AiError,
    pub(super) qualified: bool,
    pub(super) saw_generation: bool,
    pub(super) opened: bool,
    pub(super) exposure: Option<UsageUncertaintyBound>,
}

impl PendingProviderRecovery {
    pub(super) fn waiting_for_network(&self) -> bool {
        self.qualified
            && !self.opened
            && !self.saw_generation
            && matches!(
                &self.error,
                AiError::Auth(octet_ai::AuthError::Unavailable) | AiError::NetworkUnavailable(_)
            )
    }

    pub(super) fn opening_admission(&self) -> bool {
        self.qualified
            && !self.saw_generation
            && (opening_transport_failure(&self.error) || http_server_failure(&self.error))
    }

    pub(super) fn replacement_limit(&self) -> usize {
        if self.opening_admission() {
            MAX_OPENING_ADMISSION_REPLACEMENTS
        } else if self.qualified
            && !self.opened
            && !self.saw_generation
            && matches!(self.error, AiError::Auth(octet_ai::AuthError::Unavailable))
        {
            MAX_NETWORK_RETRIES
        } else if self.qualified && interrupted_inference_error(&self.error) {
            MAX_INFERENCE_REPLACEMENTS
        } else if !self.saw_generation
            && if self.opened {
                retryable_stream_start(&self.error)
            } else {
                retryable_before_generation(&self.error)
            }
        {
            provider_retry_limit(&self.error)
        } else {
            0
        }
    }

    pub(super) fn usage_unknown(&self) -> bool {
        self.saw_generation
            // A replay-authorized gateway 5xx is not proof of zero billing.
            || http_usage_unknown(&self.error)
            || (self.opened && !retryable_before_generation(&self.error))
            || interrupted_inference_error(&self.error)
    }
}

pub(super) async fn wait_network_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(super) fn network_wait_delay(run_id: &str, attempt: usize) -> Duration {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    run_id.hash(&mut hash);
    attempt.hash(&mut hash);
    let base = 5_000u64.saturating_mul(1u64 << attempt.min(4));
    // Run-specific ±20% jitter; even the first retry waits at least four seconds.
    // A saturated counter still waits at the cap rather than overflowing/spinning.
    let jitter = 800 + hash.finish() % 401;
    Duration::from_millis(
        base.saturating_mul(jitter)
            .saturating_div(1_000)
            .min(60_000),
    )
}

pub(super) fn retry_after(error: &AiError, attempt: usize) -> Duration {
    if let AiError::StreamFailure { inner, .. } = error {
        return retry_after(inner, attempt);
    }
    if let AiError::Http(error) = error {
        if let Some(delay) = error.retry_after {
            return delay;
        }
    }
    if let AiError::Provider(error) | AiError::ResponsesFailed(error) = error {
        if error.code.as_deref() == Some("rate_limit_exceeded") {
            if let Some(delay) = provider_rate_limit_delay(&error.message) {
                return delay;
            }
        }
    }
    // Keep retries bounded and add a small deterministic stagger in lieu of a
    // rand dependency. The provider's Retry-After always takes precedence.
    let base = 200u64.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(base + (attempt as u64 * 37) % 100)
}

pub(super) fn provider_rate_limit_delay(message: &str) -> Option<Duration> {
    let lower = message.to_ascii_lowercase();
    let hint = lower.split_once("try again in")?.1.trim_start();
    let end = hint.find(|ch: char| !ch.is_ascii_digit() && ch != '.')?;
    let value = hint[..end].parse::<f64>().ok()?;
    let unit = hint[end..].trim_start();
    let seconds = if unit.starts_with("ms") {
        value / 1_000.0
    } else if unit.starts_with('s') {
        value
    } else {
        return None;
    };
    Duration::try_from_secs_f64(seconds).ok()
}

pub(super) type AuxiliaryDispatch = Arc<Mutex<Option<AiClient>>>;

pub(super) struct AuxiliaryRecovery<'a> {
    pub(super) dispatch: AuxiliaryDispatch,
    pub(super) session: &'a mut Session,
    pub(super) run_id: &'a str,
    pub(super) resource_owner: &'a str,
    pub(super) retry_hooks: &'a [Arc<dyn ProviderRetryHook>],
    pub(super) max_network_wait: Option<Duration>,
    pub(super) model: &'a Model,
    pub(super) qualified: bool,
    pub(super) enabled: bool,
    pub(super) hard_budget: bool,
    pub(super) exposure: Option<UsageUncertaintyBound>,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) token_limit: Option<u64>,
    pub(super) cost_limit: Option<u64>,
    pub(super) retention: CacheRetention,
    pub(super) abort: &'a AbortFlag,
    pub(super) events: &'a mpsc::UnboundedSender<AgentEvent>,
    pub(super) operation: crate::events::ProviderOperation,
    pub(super) session_id: &'a str,
}

impl AuxiliaryRecovery<'_> {
    pub(super) fn disarm(&mut self) {
        self.dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    pub(super) fn record_uncertainty(&mut self) -> Result<(), AgentError> {
        let first = !self.session.has_uncertain_usage();
        let recorded = self.session.record_usage_uncertainty_with_bound(
            self.model.endpoint.id.clone(),
            self.model.spec.id.clone(),
            match self.operation {
                crate::events::ProviderOperation::LocalCompaction => "local_compaction",
                crate::events::ProviderOperation::BranchSummary => "branch_summary",
                crate::events::ProviderOperation::NativeCompaction => "native_compaction",
                crate::events::ProviderOperation::TerminalGate => "terminal_gate",
            },
            self.exposure,
        );
        recorded?;
        if first {
            let _ = self.events.send(AgentEvent::ProviderUsageUncertain);
        }
        Ok(())
    }
}

impl Drop for AuxiliaryRecovery<'_> {
    fn drop(&mut self) {
        let pending = self
            .dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(AiClient::request_may_have_been_sent);
        if !pending {
            return;
        }
        // An outer UI future can be dropped without setting AbortFlag. The
        // transport tracker, not a guessed usage value, fences that exposure.
        let first = !self.session.has_uncertain_usage();
        // Persistence failure cannot be returned from Drop; the session also
        // retains this marker in memory so later hard budgets still fail closed.
        let _ = self.session.record_abandoned_usage_uncertainty(
            self.model.endpoint.id.clone(),
            self.model.spec.id.clone(),
            match self.operation {
                crate::events::ProviderOperation::LocalCompaction => "local_compaction",
                crate::events::ProviderOperation::BranchSummary => "branch_summary",
                crate::events::ProviderOperation::NativeCompaction => "native_compaction",
                crate::events::ProviderOperation::TerminalGate => "terminal_gate",
            },
            self.exposure,
        );
        if first {
            let _ = self.events.send(AgentEvent::ProviderUsageUncertain);
        }
    }
}

// Auxiliary calls own an immutable, exclusively borrowed session snapshot until
// they settle. No controls are committed or tool schema is dispatched inside
// them, so a replacement may rebuild the same request from that snapshot.
// The accepted-attempt guard remains armed until `settle` durably records known
// usage. Cancellation may discard the response, never its billing evidence.
// Output/history commits remain caller-owned, after cancellation checks.
pub(super) async fn recover_auxiliary<T, F, Fut, S>(
    mut context: AuxiliaryRecovery<'_>,
    mut call: F,
    settle: S,
) -> Result<T, AgentError>
where
    F: FnMut(Option<tokio::time::Instant>, AuxiliaryDispatch) -> Fut,
    Fut: std::future::Future<Output = Result<T, AgentError>>,
    S: FnOnce(&mut Session, &T) -> Result<(), AgentError>,
{
    let mut retries = 0usize;
    let mut recovery_budget = ProviderRecoveryBudget::default();
    let mut network_waits = 0usize;
    let mut network_deadline = None;
    let mut usage_unknown = false;
    loop {
        let result = tokio::select! {
            biased;
            _ = context.abort.wait() => return Err(AgentError::Cancelled),
            result = call(network_deadline, Arc::clone(&context.dispatch)) => result,
        };
        let error = match result {
            Ok(value) => {
                // A successful future can set abort later in the same poll.
                // Account first, and keep Drop's uncertainty fallback armed
                // if the synced known-usage append fails.
                settle(context.session, &value)?;
                context.disarm();
                return Ok(value);
            }
            Err(AgentError::Ai(error)) => error,
            Err(
                error @ AgentError::NetworkWaitLimit {
                    usage_unknown: true,
                    ..
                },
            ) => {
                context.record_uncertainty()?;
                context.disarm();
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let presend = matches!(
            &error,
            AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Connect,
                ..
            }) | AiError::NetworkUnavailable(_)
                | AiError::Auth(octet_ai::AuthError::Unavailable)
        );
        // A body/error response proves opening has recovered. A later
        // interruption starts a new outage only after positive pre-send evidence.
        if matches!(
            &error,
            AiError::Http(_)
                | AiError::Provider(_)
                | AiError::ResponsesFailed(_)
                | AiError::StreamFailure { .. }
                | AiError::StreamProtocol(_)
                | AiError::Transport(octet_ai::TransportError {
                    phase: octet_ai::TransportPhase::Body,
                    ..
                })
        ) {
            network_deadline = None;
        }
        let saw_generation = !presend
            && !opening_transport_failure(&error)
            && !http_server_failure(&error)
            && !retryable_before_generation(&error);
        let recovery = PendingProviderRecovery {
            error,
            qualified: context.qualified,
            opened: !presend,
            // complete() hides generation progress. Only explicit rejection
            // or pre-send evidence can establish absence of generation.
            saw_generation,
            exposure: context.exposure,
        };
        if recovery.usage_unknown() {
            context.record_uncertainty()?;
        }
        // Either durable uncertainty or positive no-exposure evidence now
        // owns this failed attempt. A replacement gets a fresh tracker.
        context.disarm();
        usage_unknown |= recovery.usage_unknown();
        let waiting = recovery.waiting_for_network();
        if waiting && network_deadline.is_none() {
            network_deadline = context
                .max_network_wait
                .and_then(|limit| tokio::time::Instant::now().checked_add(limit));
        }
        // Tool-free summaries can replace a transient interrupted generation
        // on ordinary routes too. Keep the qualified Codex recovery envelope
        // intact; never stack another outer loop around it.
        let summary_policy = SummarizationRetryPolicy::default();
        let summary_retry = matches!(
            context.operation,
            crate::events::ProviderOperation::LocalCompaction
                | crate::events::ProviderOperation::BranchSummary
        ) && !context.qualified
            && (retryable_stream_start(&recovery.error)
                || interrupted_inference_error(&recovery.error));
        let limit = if summary_retry {
            summary_policy.attempts().saturating_sub(1)
        } else {
            recovery_budget.limit(retries, &recovery)
        };
        if !context.enabled
            || (!waiting && retries >= limit)
            || (context.hard_budget
                && usage_unknown
                && uncertainty_blocks_ceiling(
                    context.session,
                    context.token_limit,
                    context.cost_limit,
                ))
        {
            return Err(AgentError::ProviderRecovery {
                retries,
                usage_unknown,
                source: recovery.error,
            });
        }
        let delay = if waiting {
            let delay = network_wait_delay(context.session_id, network_waits);
            network_waits = network_waits.saturating_add(1);
            delay
        } else {
            let mut delay = retry_after(&recovery.error, retries);
            retries += 1;
            if summary_retry {
                delay = delay.max(summary_policy.backoff_for_retry(retries));
            }
            delay
        };
        let decision_future = provider_retry_decision(ProviderRetryRequest {
            hooks: context.retry_hooks,
            context: ProviderRetryContext {
                operation: Some(context.operation),
                run_id: context.run_id.to_owned(),
                resource_owner: context.resource_owner.to_owned(),
                attempt: if waiting { network_waits } else { retries },
                max_attempts: (!waiting).then_some(limit),
                host_delay: delay,
                kind: if waiting {
                    ProviderRetryKind::WaitingForNetwork
                } else if (recovery.qualified || summary_retry)
                    && interrupted_inference_error(&recovery.error)
                {
                    ProviderRetryKind::InterruptedInference
                } else {
                    ProviderRetryKind::BeforeGeneration
                },
            },
            abort: context.abort,
        });
        let decision = tokio::select! {
            biased;
            _ = context.abort.wait() => return Err(AgentError::Cancelled),
            _ = wait_network_deadline(network_deadline) => return Err(AgentError::NetworkWaitLimit { limit: context.max_network_wait.unwrap(), usage_unknown }),
            decision = decision_future => decision,
        };

        if context.abort.is_set() {
            return Err(AgentError::Cancelled);
        }
        if !decision.proceed {
            return Err(AgentError::ProviderRecovery {
                retries: retries.saturating_sub(usize::from(!waiting)),
                usage_unknown,
                source: recovery.error,
            });
        }
        // Each physical replacement needs its own reservation, including any
        // previously accepted attempts charged at their admission bounds.
        reserve_request_tokens(
            context.session,
            context.input_tokens,
            context.output_tokens,
            context.token_limit,
        )?;
        reserve_request_cost(
            context.session,
            context.model,
            context.input_tokens,
            context.output_tokens,
            context.cost_limit,
            context.retention,
        )?;
        if !waiting {
            recovery_budget.admit(&recovery);
        }
        let delay = delay.saturating_add(decision.additional_delay);
        let _ = context.events.send(AgentEvent::ProviderOperationRetry {
            operation: context.operation,
            attempt: if waiting { network_waits } else { retries },
            max_attempts: (!waiting).then_some(limit),
            delay,
            error: {
                let mut diagnostic = public_ai_error_diagnostic(
                    &recovery.error,
                    &context.model.endpoint.id.0,
                    &context.model.spec.id.0,
                );
                if usage_unknown {
                    diagnostic = format!("failed_usage=unknown {diagnostic}");
                }
                if matches!(
                    context.operation,
                    crate::events::ProviderOperation::LocalCompaction
                        | crate::events::ProviderOperation::BranchSummary
                ) {
                    diagnostic = SummarizationRetryScheduled {
                        attempt: retries,
                        max_attempts: limit.saturating_add(1),
                        delay,
                        error: diagnostic,
                    }
                    .diagnostic();
                }
                truncate_public_diagnostic(&mut diagnostic);
                diagnostic
            },
        });
        tokio::select! {
            biased;
            _ = context.abort.wait() => return Err(AgentError::Cancelled),
            _ = wait_network_deadline(network_deadline) => return Err(AgentError::NetworkWaitLimit { limit: context.max_network_wait.unwrap(), usage_unknown }),
            _ = tokio::time::sleep(delay) => {},
        }
    }
}

// A reconnected response body has its provider deadlines, not the outage
// deadline. AiClient::complete hides this boundary, so collect the stream here.
pub(super) async fn auxiliary_complete(
    client: &AiClient,
    model: &Model,
    request: Request,
    deadline: Option<tokio::time::Instant>,
    max_network_wait: Option<Duration>,
    dispatch: AuxiliaryDispatch,
) -> Result<octet_ai::Response, AgentError> {
    let client = client.track_request_dispatch();
    *dispatch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(client.clone());
    let mut stream = tokio::select! {
        biased;
        _ = wait_network_deadline(deadline) => return Err(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: client.request_may_have_been_sent() }),
        result = client.stream(model, request) => result?,
    };
    let mut response = None;
    while let Some(event) = stream.next().await {
        if let StreamEvent::Finished(value) = event? {
            if let Some(error) = incomplete_responses_error(model, &value) {
                return Err(error.into());
            }
            response = Some(value);
        }
    }
    response
        .ok_or_else(|| AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish).into())
}

pub(super) async fn auxiliary_compact(
    client: &AiClient,
    model: &Model,
    request: ResponsesCompactRequest,
    deadline: Option<tokio::time::Instant>,
    max_network_wait: Option<Duration>,
    dispatch: AuxiliaryDispatch,
) -> Result<octet_ai::ResponsesCompactResponse, AgentError> {
    let client = client.track_request_dispatch();
    *dispatch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(client.clone());
    let pending = tokio::select! {
        biased;
        _ = wait_network_deadline(deadline) => return Err(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: client.request_may_have_been_sent() }),
        result = client.open_compact_responses(model, request) => result?,
    };
    Ok(pending.complete().await?)
}

// Responses maps only unknown response.incomplete reasons to Other. Treat
// these terminal partial generations like response.failed before any assistant
// or auxiliary usage commit; known length/filter reasons retain their semantics.
pub(super) fn incomplete_responses_error(
    model: &Model,
    response: &octet_ai::Response,
) -> Option<AiError> {
    if model.spec.protocol == Protocol::OpenAiResponses {
        if let StopReason::Other(reason) = &response.stop_reason {
            return Some(AiError::ResponsesFailed(octet_ai::ProviderError {
                code: Some(reason.clone()),
                kind: None,
                message: "Responses generation incomplete".into(),
                request_id: None,
            }));
        }
    }
    None
}

pub(super) struct ProviderRetryDecision {
    pub(super) proceed: bool,
    pub(super) additional_delay: Duration,
}

pub(super) struct ProviderRetryRequest<'a> {
    pub(super) hooks: &'a [Arc<dyn ProviderRetryHook>],
    pub(super) context: ProviderRetryContext,
    pub(super) abort: &'a AbortFlag,
}

pub(super) async fn provider_retry_decision(
    request: ProviderRetryRequest<'_>,
) -> ProviderRetryDecision {
    let ProviderRetryRequest {
        hooks,
        context,
        abort,
    } = request;
    if abort.is_set() {
        return ProviderRetryDecision {
            proceed: false,
            additional_delay: Duration::ZERO,
        };
    }
    let deadline = tokio::time::Instant::now() + PROVIDER_RETRY_HOOK_BUDGET;
    let mut additional_delay = Duration::ZERO;
    for hook in hooks {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let advice = tokio::select! {
            biased;
            _ = abort.wait() => return ProviderRetryDecision {
                proceed: false,
                additional_delay: Duration::ZERO,
            },
            result = tokio::time::timeout(remaining, hook.provider_retry(&context)) => result.ok(),
        };
        match advice {
            Some(ProviderRetryAdvice::Stop) => {
                return ProviderRetryDecision {
                    proceed: false,
                    additional_delay: Duration::ZERO,
                };
            }
            Some(ProviderRetryAdvice::Delay { additional }) => {
                additional_delay = additional_delay
                    .saturating_add(additional)
                    .min(MAX_PROVIDER_RETRY_ADDITIONAL_DELAY);
            }
            Some(ProviderRetryAdvice::NoOpinion | ProviderRetryAdvice::Retry) | None => {}
        }
    }
    ProviderRetryDecision {
        proceed: !abort.is_set(),
        additional_delay,
    }
}

pub(super) fn assistant_persistence_context(
    run_id: &str,
    resource_owner: &str,
    assistant: &AssistantMessage,
    stop_reason: StopReason,
) -> AssistantPersistenceContext {
    let mut text_bytes = 0usize;
    let mut tool_call_count = 0usize;
    let mut reasoning_part_count = 0usize;
    let mut media_part_count = 0usize;
    for part in &assistant.content {
        match part {
            AssistantPart::Text(text) => text_bytes = text_bytes.saturating_add(text.len()),
            AssistantPart::ToolCall(_) => tool_call_count = tool_call_count.saturating_add(1),
            AssistantPart::Reasoning(_) => {
                reasoning_part_count = reasoning_part_count.saturating_add(1)
            }
            // Opaque provider continuation metadata has no user-visible
            // content and must not affect persistence accounting.
            AssistantPart::ProviderMetadata(_) => {}
            AssistantPart::Media(_) => media_part_count = media_part_count.saturating_add(1),
        }
    }
    AssistantPersistenceContext {
        run_id: run_id.to_owned(),
        resource_owner: resource_owner.to_owned(),
        model: assistant.model.clone(),
        protocol: assistant.protocol,
        stop_reason,
        text_bytes,
        tool_call_count,
        reasoning_part_count,
        media_part_count,
    }
}

pub(super) async fn collect_persistence_metadata(
    hooks: &[RegisteredPersistenceMetadataHook],
    context: &AssistantPersistenceContext,
    abort: &AbortFlag,
) -> Option<EntryMetadata> {
    if hooks.is_empty() || abort.is_set() {
        return None;
    }
    let deadline = tokio::time::Instant::now() + PERSISTENCE_METADATA_HOOK_BUDGET;
    let mut metadata = EntryMetadata::default();
    for registered in hooks {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() || abort.is_set() {
            break;
        }
        let proposal = tokio::select! {
            biased;
            _ = abort.wait() => break,
            result = tokio::time::timeout(remaining, registered.hook.before_assistant_persist(context)) => result.ok().flatten(),
        };
        let Some(proposal) = proposal else {
            continue;
        };
        let process_generation = proposal.process_generation();
        metadata.extension_metadata.insert(
            registered.namespace.clone(),
            ExtensionEntryMetadata {
                public: proposal.public,
                value: proposal.value,
                provenance: ExtensionMetadataProvenance {
                    extension: registered.namespace.clone(),
                    process_generation,
                },
            },
        );
    }
    (!metadata.extension_metadata.is_empty()).then_some(metadata)
}

pub(super) fn provider_retry_diagnostic(model: &Model, error: &AiError) -> String {
    let diagnostic = public_ai_error_diagnostic(error, &model.endpoint.id.0, &model.spec.id.0);
    if is_replayable_network_failure(error) {
        format!("Network connection lost. Are you connected to the internet? {diagnostic}")
    } else {
        diagnostic
    }
}

pub(super) fn provider_failure(error: AiError, retries: usize) -> AgentError {
    if is_replayable_network_failure(&error) {
        AgentError::NetworkUnavailable {
            retries,
            detail: ai_error_phase(&error).to_owned(),
        }
    } else {
        error.into()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_recovery_call(
    call_index: usize,
    tool: Arc<dyn Tool>,
    hooks: &[Arc<dyn ToolCallHook>],
    broker: &EffectBroker,
    generation: u64,
    run_id: &str,
    call: &ToolCall,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    registered_tools: &[String],
    session: &mut Session,
) -> Result<Result<ToolOutput, ToolError>, AgentError> {
    let parsed = call
        .arguments_value()
        .map_err(|error| ToolError::new(format!("invalid tool arguments: {error}")));
    let result = match parsed {
        Err(error) => Err(error),
        Ok(args) => {
            let active_skills = session
                .head()
                .and_then(|head| session.resolve_active_skills(&head).ok())
                .map(|state| state.active_skills)
                .unwrap_or_default();
            let (progress_tx, mut progress_rx) =
                mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
            let progress_sink = ToolProgressSink::live(progress_tx)
                .with_tool_call_identity(call.id.0.clone(), None);
            let mut context = ToolContext {
                workspace: &sandbox.workspace,
                sandbox,
                execution_scope: tool_scope,
                resource_owner,
                active_skills: &active_skills,
                registered_tools,
                progress: progress_sink,
                cancellation: CancellationToken::default(),
            };
            let args =
                match transform_tool_arguments(hooks, tool.as_ref(), &call.name, args, &context)
                    .await
                {
                    Ok(args) => args,
                    Err(error) => return Ok(Err(error)),
                };
            let effect = match tool.effect(&args, &context) {
                Ok(effect) => effect,
                Err(error) => return Ok(Err(error)),
            };
            if !effect_is_repeatable_observation(effect) {
                return Ok(Err(ToolError::new(format!(
                    "indeterminate after restart: octet did not replay this host-classified effect for `{}`.\n{}",
                    call.name, synthesize_interruption(session.invocation_partial_output(call_index)?.as_deref()).text
                ))));
            }
            let invocation = session.tool_invocation(call_index)?;
            context.progress = context.progress.with_invocation(invocation.clone());
            // Bind admission to the same exact classification used by the
            // replay gate. A second classification could otherwise authorize
            // an effect different from the one that passed replay admission.
            let intent = match EffectIntent::new(
                resource_owner,
                run_id,
                generation,
                call.id.0.clone(),
                &call.name,
                effect,
                &args,
            ) {
                Ok(intent) => intent,
                Err(error) => return Ok(Err(ToolError::new(error.to_string()))),
            };
            let effect_reservation = match broker.reserve(&intent, None).await {
                Ok(reservation) => reservation,
                Err(error) => return Ok(Err(ToolError::new(error.to_string()))),
            };
            // Retain hook arguments before the execution admission point so no
            // payload-sized allocation separates commit from dispatch.
            let hook_arguments = args.clone();
            for hook in hooks {
                if let Err(error) = hook.before_tool_call(&call.name, &args, &context).await {
                    return Ok(Err(error));
                }
            }
            if let Err(error) = effect_reservation.commit(&intent) {
                return Ok(Err(ToolError::new(error.to_string())));
            }
            invocation
                .clear_partial_output()
                .map_err(|e| SessionError::Limit(e.to_string()))?;
            let execute = tool.execute(args, &context);
            tokio::pin!(execute);
            let result = loop {
                tokio::select! {
                    result = &mut execute => break result,
                    progress = progress_rx.recv() => {
                        if let Some(ToolProgress::SessionEvent(event, reply)) = progress {
                            match session.append(*event) {
                                Ok(entry_id) => {
                                    if let Ok(mut slot) = reply.lock() {
                                        if let Some(sender) = slot.take() {
                                            let _ = sender.send(Ok(entry_id));
                                        }
                                    }
                                }
                                Err(error) => {
                                    let message = error.to_string();
                                    if let Ok(mut slot) = reply.lock() {
                                        if let Some(sender) = slot.take() {
                                            let _ = sender.send(Err(message));
                                        }
                                    }
                                    return Err(AgentError::Session(error));
                                }
                            }
                        }
                    }
                }
            };
            // A tool can enqueue a final semantic event just before returning.
            // Apply every already-accepted event before writing its result.
            while let Ok(progress) = progress_rx.try_recv() {
                if let ToolProgress::SessionEvent(event, reply) = progress {
                    match session.append(*event) {
                        Ok(entry_id) => {
                            if let Ok(mut slot) = reply.lock() {
                                if let Some(sender) = slot.take() {
                                    let _ = sender.send(Ok(entry_id));
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            if let Ok(mut slot) = reply.lock() {
                                if let Some(sender) = slot.take() {
                                    let _ = sender.send(Err(message));
                                }
                            }
                            return Err(AgentError::Session(error));
                        }
                    }
                }
            }
            settle_tool_result_hooks(hooks, &call.name, &hook_arguments, result, &context).await
        }
    };
    Ok(result)
}

pub(super) async fn open_provider_stream(
    client: &AiClient,
    model: &Model,
    request: Request,
    abort: &AbortFlag,
) -> Result<Option<octet_ai::ResponseStream>, AiError> {
    tokio::select! {
        biased;
        _ = abort.wait() => Ok(None),
        result = client.stream(model, request) => result.map(Some),
    }
}
