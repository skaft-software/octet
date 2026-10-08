//! `AgentError` and the bounded, redacted public diagnostics built from it.

use super::*;

/// Errors surfaced by [`Agent`] APIs.
///
/// Before a run starts these are returned directly (from [`Agent::new`],
/// [`Agent::prompt`], [`RunControl::steer`]…). Once a run has started, every
/// failure is delivered as the single terminal
/// [`AgentEvent::RunFinished`]`{ reason: FinishReason::Failed(..) }` event —
/// there is no second asynchronous error channel.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Session persistence failed.
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    /// Inline user image could not be safely prepared before persistence.
    #[error("image input: {0}")]
    ImageInput(#[from] ImageInputError),
    /// Aggregate image preparation exceeded the bounded per-input budget.
    #[error("image input exceeds 8 images or 20 MiB of encoded image data")]
    ImageInputBatchLimit,
    /// The blocking image preparation worker could not complete.
    #[error("image input preparation worker failed")]
    ImagePreparationFailed,
    /// The inference layer failed.
    #[error("ai error: {0}")]
    Ai(#[from] AiError),
    /// Repeated non-timeout network failures exhausted automatic recovery.
    #[error(
        "network connection failed after {retries} retries. Are you connected to the internet? ({detail})"
    )]
    NetworkUnavailable {
        /// Number of replacement attempts made after the initial request.
        retries: usize,
        /// Bounded phase-only detail for diagnostics.
        detail: String,
    },
    /// A provider attempt failed after bounded in-process recovery, or its
    /// unresolved usage prevents enforcing a hard cumulative ceiling.
    #[error("provider recovery stopped after {retries} replacements (failed-attempt usage unknown: {usage_unknown}): {source}")]
    ProviderRecovery {
        /// Replacement attempts admitted in this logical turn.
        retries: usize,
        /// Whether accepted inference has unaccounted usage.
        usage_unknown: bool,
        /// Last bounded provider failure, including its progress when available.
        #[source]
        source: AiError,
    },
    /// Unknown accepted-attempt usage prevents a conservative hard ceiling.
    #[error(
        "session contains unsettled provider usage; hard token or cost ceilings cannot be enforced"
    )]
    UsageUncertain,
    /// A declared model maximum is not a provider-enforced output bound.
    #[error("hard token or cost ceilings require an enforceable provider output limit; this operation sends no output cap")]
    OutputLimitUnavailable,
    /// Planning estimates cannot prove a provider-tokenized input upper bound.
    #[error("hard token or cost ceilings require an enforceable provider input bound; this route supplies only a planning estimate")]
    InputLimitUnavailable,
    /// Effective provider context could not be prepared safely before dispatch.
    #[error("provider context preparation refused: {0}")]
    ProviderContextPreparation(&'static str),
    /// A host-owned maximum outage duration expired during recovery.
    #[error("network recovery exceeded the host outage limit of {limit:?} (failed-attempt usage unknown: {usage_unknown})")]
    NetworkWaitLimit {
        /// Host-configured elapsed outage allowance.
        limit: Duration,
        /// A cancelled opening may already have dispatched provider work.
        usage_unknown: bool,
    },
    /// Two tools were registered under the same name.
    #[error("duplicate tool name registered: {0}")]
    DuplicateTool(String),
    /// An extension attempted to register an invalid or colliding durable
    /// metadata namespace.
    #[error("invalid extension metadata namespace: {0}")]
    ExtensionMetadataNamespace(String),
    /// The configured collaboration runtime could not start an owning run.
    #[error("delegation error: {0}")]
    Delegation(String),
    /// The configured workspace root is unusable.
    #[error("invalid workspace: {0}")]
    Workspace(String),
    /// The provider ended a response without a normal completion signal.
    #[error("model response did not complete normally: {stop_reason}")]
    IncompleteResponse {
        /// Provider termination reason.
        stop_reason: String,
    },
    /// Provider-visible tool schemas exceeded the configured byte budget.
    #[error(
        "tool schema budget exceeded: {actual_bytes} bytes for {tool_count} definitions > {max_bytes} bytes; no tool definitions were sent"
    )]
    ToolSchemaBudgetExceeded {
        /// Exact serialized JSON byte count for the full tool-definition array.
        actual_bytes: usize,
        /// Number of definitions that would have been sent.
        tool_count: usize,
        /// Configured hard byte limit.
        max_bytes: usize,
    },
    /// The next billable request's conservative token reservation would cross
    /// the configured session token ceiling.
    #[error(
        "session token limit would be exceeded: current {current} + reserved {reserved} tokens > limit {limit}"
    )]
    TokenLimit {
        /// Durable session token usage before the request.
        current: u64,
        /// Conservative input plus maximum-output reservation.
        reserved: u64,
        /// Configured ceiling.
        limit: u64,
    },
    /// The next billable request's conservative reservation would cross the
    /// configured session spend ceiling.
    #[error(
        "session cost limit would be exceeded: current {current} µUSD + reserved {reserved} µUSD > limit {limit} µUSD"
    )]
    CostLimit {
        /// Durable session cost before the request.
        current: u64,
        /// Conservative worst-case request reservation.
        reserved: u64,
        /// Configured ceiling.
        limit: u64,
    },
    /// A spend ceiling was requested but the selected model has no trusted
    /// pricing from which the host can reserve the next request.
    #[error(
        "session cost limit cannot be enforced because model pricing is unavailable (limit {limit} µUSD)"
    )]
    CostUnavailable {
        /// Configured ceiling that cannot be enforced.
        limit: u64,
    },
    /// The request would exceed the model's context budget after compaction.
    #[error(
        "request context is too large: approximately {estimate} tokens exceeds the {budget}-token input budget"
    )]
    ContextExceeded {
        /// Estimated request size.
        estimate: u64,
        /// Maximum input size after reserving output capacity.
        budget: u64,
    },
    /// The configured autonomous compaction policy is invalid.
    #[error("invalid compaction policy: {0}")]
    InvalidCompactionPolicy(String),
    /// Internal autonomous work was cancelled before its commit point.
    #[error("operation cancelled")]
    Cancelled,
    /// A durable deferred-run mutation was refused (stale generation, unknown
    /// operation, closed store, or a persisted leaf that cannot be trusted).
    #[error("deferred run: {0}")]
    Deferred(#[from] DeferredRunError),
    /// The provider parked this request at a durable `deferred.suspended`
    /// leaf. This is **not** an inference failure and must never be retried as
    /// a generation request: a later permitted pass polls the recorded handle
    /// under exactly one generation-owned permit.
    #[error(
        "run suspended at deferred operation {operation_id} (poll {poll}, generation {generation}); a permitted deferred poll resumes it"
    )]
    DeferredSuspended {
        /// Durable operation identity of the parked request.
        operation_id: String,
        /// Poll number the parked leaf is at (`0` after the first suspension).
        poll: u64,
        /// Durable generation of the parked leaf.
        generation: u64,
    },
    /// The provider returned a deferred stop reason whose handle could not be
    /// trusted. The run failed closed instead of parking on an unvetted
    /// handle.
    #[error("deferred suspension refused: {diagnostic}")]
    DeferredSuspensionRefused {
        /// Diagnostic starting with Pi's exact invalid-handle wording.
        diagnostic: String,
    },
    /// An active-tool narrowing request named tools that are not in the
    /// host-policed registered surface; the request changed nothing. The list
    /// is sorted and bounded, and names the refused tools.
    #[error("cannot activate unknown tool(s): {0:?}")]
    UnknownActiveTools(Vec<String>),
    /// The host refused an otherwise validated active-tool narrowing request,
    /// for example because a concurrent extension catalog change removed a
    /// requested name before publication. The request changed nothing.
    #[error("active tool set refused: {0}")]
    ActiveToolSetRefused(String),
    /// A control message was sent after the run finished.
    #[error("the run has already finished")]
    RunEnded,
    /// Input was not accepted because the run's queued count or byte budget
    /// (including inputs awaiting durable delivery) is exhausted.
    #[error("the run control queue is full; input was not accepted")]
    ControlQueueFull,
}

/// Format an agent failure for a public frontend.
///
/// Provider failures retain operationally useful, bounded metadata: the route,
/// execution phase, HTTP status/class, provider error code, safe provider
/// message, retry hint, terminal stop reason, and request ID when supplied.
/// Request bodies, URLs,
/// credentials, and arbitrary response payloads are never copied into this
/// string. The AI client redacts at the transport boundary; this function
/// applies the final field allow-list and byte bound for UI/RPC consumers.
pub fn public_error_diagnostic(error: &AgentError, endpoint: &str, model: &str) -> String {
    match error {
        AgentError::Ai(error) => public_ai_error_diagnostic(error, endpoint, model),
        AgentError::ProviderRecovery {
            retries,
            usage_unknown,
            source,
        } => {
            let mut diagnostic = format!(
                "replacements={retries} failed_usage={} ",
                if *usage_unknown {
                    "unknown"
                } else {
                    "not_observed"
                }
            );
            diagnostic.push_str(&public_ai_error_diagnostic(source, endpoint, model));
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        AgentError::IncompleteResponse { stop_reason } => {
            let mut diagnostic = provider_phase_diagnostic(endpoint, model, "response completion");
            append_provider_field(&mut diagnostic, "reason", Some(stop_reason));
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        _ => match provider_failure_phase(error) {
            Some(phase) => provider_phase_diagnostic(endpoint, model, phase),
            None => error.to_string(),
        },
    }
}

pub(super) const AMBIGUOUS_ACCEPTANCE_HINT: &str =
    "Provider acceptance and failed-attempt usage are uncertain. Inspect provider state before retrying explicitly.";

/// Format an inference-layer error for a user-facing retry or terminal event.
/// The same allow-list is used for both paths so retry messages cannot expose
/// more provider data than the final failure message.
pub(super) fn public_ai_error_diagnostic(error: &AiError, endpoint: &str, model: &str) -> String {
    let context = |phase| provider_phase_diagnostic(endpoint, model, phase);
    match error {
        AiError::Http(http) => format_http_diagnostic(&context("HTTP response"), http),
        // A deferred poll refusal never reaches a provider: it is decided from
        // the durable permit/generation before any request work starts, so only
        // the static refusal text is published.
        AiError::Deferred(refusal) => {
            let mut diagnostic = context("deferred poll refused");
            append_provider_field(&mut diagnostic, "detail", Some(&refusal.to_string()));
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        AiError::Provider(provider) | AiError::ResponsesFailed(provider) => {
            let mut diagnostic = context("response body (provider error)");
            append_provider_field(&mut diagnostic, "code", provider.code.as_deref());
            append_provider_field(&mut diagnostic, "kind", provider.kind.as_deref());
            append_provider_field(&mut diagnostic, "detail", Some(&provider.message));
            append_provider_field(
                &mut diagnostic,
                "request_id",
                provider.request_id.as_deref(),
            );
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        AiError::Transport(transport) | AiError::NetworkUnavailable(transport) => {
            let phase = match (transport.phase, transport.timeout) {
                (octet_ai::TransportPhase::Connect, false) => "connection",
                (octet_ai::TransportPhase::Connect, true) => "connection timeout",
                (octet_ai::TransportPhase::ResponseHeaders, false) => "response headers",
                (octet_ai::TransportPhase::ResponseHeaders, true) => "response headers timeout",
                (octet_ai::TransportPhase::Body, false) => "response body",
                (octet_ai::TransportPhase::Body, true) => "response body timeout",
            };
            let mut diagnostic = context(phase);
            if transport.phase != octet_ai::TransportPhase::Connect {
                append_provider_field(&mut diagnostic, "hint", Some(AMBIGUOUS_ACCEPTANCE_HINT));
            }
            append_provider_field(&mut diagnostic, "detail", Some(&transport.message));
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        // A mid-stream failure is annotated with the wire progress that
        // distinguishes "died after 400 frames" from "never started": raw
        // provider frames, decoded events, retained content bytes, and
        // elapsed time. The inner diagnostic is first bounded to leave room
        // for this compact field, so a verbose inner detail can never push
        // the progress out of the truncation window.
        AiError::StreamFailure { inner, progress } => {
            let progress = format_stream_progress(progress);
            let mut diagnostic = public_ai_error_diagnostic(inner, endpoint, model);
            let budget = MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES
                .saturating_sub(progress.len())
                .saturating_sub("stream_progress=".len() + 1);
            truncate_public_diagnostic_to(&mut diagnostic, budget);
            append_provider_field(&mut diagnostic, "stream_progress", Some(&progress));
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        // The remaining variants previously surfaced as a bare phase label
        // with their message discarded. Their text is already
        // credential-redacted at the request boundary, so a bounded
        // `detail` field is safe to show.
        AiError::Config(error) => detail_diagnostic(&context("request preparation"), error),
        AiError::Batch(error) => detail_diagnostic(&context("request preparation"), error),
        AiError::Auth(error) => detail_diagnostic(&context("authentication"), error),
        AiError::Validation(error) => detail_diagnostic(&context("request preparation"), error),
        AiError::Unsupported(error) => detail_diagnostic(&context("request preparation"), error),
        AiError::Decode(error) => detail_diagnostic(&context("response decoding"), error),
        AiError::Pricing(error) => detail_diagnostic(&context("usage accounting"), error),
        AiError::StreamProtocol(
            error @ (octet_ai::StreamProtocolError::MissingFinish
            | octet_ai::StreamProtocolError::PrematureEof),
        ) => {
            let mut diagnostic = context("stream protocol");
            append_provider_field(&mut diagnostic, "hint", Some(AMBIGUOUS_ACCEPTANCE_HINT));
            append_provider_field(&mut diagnostic, "detail", Some(&error.to_string()));
            truncate_public_diagnostic(&mut diagnostic);
            diagnostic
        }
        AiError::StreamProtocol(error) => detail_diagnostic(&context("stream protocol"), error),
        AiError::Canceled => context("request cancellation"),
    }
}

/// One-line enrichment for an error variant: the phase context plus the
/// (already credential-redacted) error text as a bounded `detail` field.
pub(super) fn detail_diagnostic(prefix: &str, error: &impl std::fmt::Display) -> String {
    let mut diagnostic = prefix.to_string();
    append_provider_field(&mut diagnostic, "detail", Some(&error.to_string()));
    truncate_public_diagnostic(&mut diagnostic);
    diagnostic
}

/// Compact, greppable rendering of mid-stream progress counters.
pub(super) fn format_stream_progress(progress: &octet_ai::StreamProgress) -> String {
    let first_byte = if progress.first_body_seen {
        "seen"
    } else {
        "none"
    };
    let last_event = progress
        .last_event_ms
        .map_or_else(|| "none".to_owned(), |elapsed| format!("{elapsed}ms"));
    format!(
        "frames={} events={} content={}B buffered={}B first_byte={} elapsed={}ms last_event={}",
        progress.provider_events,
        progress.decoded_events,
        progress.content_bytes,
        progress.buffered_bytes,
        first_byte,
        progress.elapsed_ms,
        last_event,
    )
}

pub(super) const MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES: usize = 2 * 1024;

pub(super) const MAX_PUBLIC_PROVIDER_FIELD_BYTES: usize = 512;

pub(super) fn format_http_diagnostic(prefix: &str, error: &octet_ai::HttpError) -> String {
    let mut diagnostic = format!("{prefix} status={}", error.status.as_u16());
    if let Some(summary) = http_status_summary(error.status.as_u16()) {
        diagnostic.push_str(" (");
        diagnostic.push_str(summary);
        diagnostic.push(')');
    }
    append_provider_field(&mut diagnostic, "code", error.provider_code.as_deref());
    if let Some(message) = provider_error_message(error.body_snippet.as_deref()) {
        append_provider_field(&mut diagnostic, "detail", Some(&message));
    }
    if let Some(delay) = error.retry_after {
        append_provider_field(
            &mut diagnostic,
            "retry_after",
            Some(&format!("{}s", delay.as_secs())),
        );
    }
    append_provider_field(&mut diagnostic, "request_id", error.request_id.as_deref());
    truncate_public_diagnostic(&mut diagnostic);
    diagnostic
}

pub(super) fn provider_error_message(body: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body?).ok()?;
    let error = value.get("error").unwrap_or(&value);
    ["message", "detail", "description"]
        .iter()
        .find_map(|field| error.get(*field).and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .filter(|message| !message.trim().is_empty())
}

pub(super) fn append_provider_field(output: &mut String, name: &str, value: Option<&str>) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let value = compact_public_provider_field(value);
    if value.is_empty() {
        return;
    }
    output.push(' ');
    output.push_str(name);
    output.push('=');
    output.push_str(&value);
}

pub(super) fn compact_public_provider_field(value: &str) -> String {
    let value = redact_common_secret_patterns(value);
    let mut output = String::with_capacity(value.len().min(MAX_PUBLIC_PROVIDER_FIELD_BYTES));
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            continue;
        }
        if output
            .len()
            .saturating_add(character.len_utf8())
            .saturating_add('…'.len_utf8())
            > MAX_PUBLIC_PROVIDER_FIELD_BYTES
        {
            output.push('…');
            break;
        }
        output.push(character);
    }
    output
}

/// Defense in depth for callers that construct `AgentError` values without
/// passing through octet-ai's credential redactor. The normal transport path
/// performs exact credential redaction; these common bearer/key forms prevent
/// accidental leakage from provider-authentication messages at the UI boundary.
pub(super) fn redact_common_secret_patterns(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while !rest.is_empty() {
        let lower = rest.to_ascii_lowercase();
        let Some((offset, marker)) = ["sk-", "bearer ", "api_key=", "https://", "http://"]
            .iter()
            .filter_map(|marker| lower.find(marker).map(|offset| (offset, *marker)))
            .min_by_key(|(offset, _)| *offset)
        else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..offset]);
        output.push_str(match marker {
            "bearer " => "Bearer [REDACTED]",
            "https://" | "http://" => "[URL]",
            _ => "[REDACTED]",
        });
        let token_start = offset + marker.len();
        let token_end = rest[token_start..]
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '"' | '\'' | ')' | ']' | '}' | ',' | ';')
            })
            .map_or(rest.len(), |end| token_start + end);
        rest = &rest[token_end..];
    }
    output
}

pub(super) fn truncate_public_diagnostic(diagnostic: &mut String) {
    truncate_public_diagnostic_to(diagnostic, MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES)
}

pub(super) fn truncate_public_diagnostic_to(diagnostic: &mut String, budget: usize) {
    if diagnostic.len() <= budget {
        return;
    }
    let mut end = budget.saturating_sub('…'.len_utf8());
    while end > 0 && !diagnostic.is_char_boundary(end) {
        end -= 1;
    }
    diagnostic.truncate(end);
    diagnostic.push('…');
}

pub(super) fn http_status_summary(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "bad request",
        401 => "authentication failed",
        402 => "payment or credits required",
        403 => "forbidden",
        404 => "route or model not found",
        408 => "request timeout",
        409 => "conflict",
        413 => "request too large",
        422 => "request rejected",
        429 => "rate limited",
        500..=599 => "provider unavailable",
        _ => return None,
    })
}

pub(super) fn provider_failure_phase(error: &AgentError) -> Option<&'static str> {
    match error {
        AgentError::Ai(error) | AgentError::ProviderRecovery { source: error, .. } => {
            Some(ai_error_phase(error))
        }
        AgentError::NetworkUnavailable { .. } => Some("connection"),
        // Handled with an extra `reason=` field by `public_error_diagnostic`;
        // kept here so the phase table stays exhaustive.
        AgentError::IncompleteResponse { .. } => Some("response completion"),
        // A durable-store refusal is a run-lifecycle failure, not a provider
        // phase; a suspended run is control flow rather than a failure, and an
        // invalid handle fails before the request is ever sent.
        AgentError::Deferred(_) => Some("deferred run"),
        AgentError::DeferredSuspensionRefused { .. } => Some("deferred suspension"),
        AgentError::DeferredSuspended { .. } => None,
        AgentError::Session(_)
        | AgentError::ImageInput(_)
        | AgentError::ImageInputBatchLimit
        | AgentError::ImagePreparationFailed
        | AgentError::DuplicateTool(_)
        | AgentError::ExtensionMetadataNamespace(_)
        | AgentError::Delegation(_)
        | AgentError::Workspace(_)
        | AgentError::ToolSchemaBudgetExceeded { .. }
        | AgentError::TokenLimit { .. }
        | AgentError::CostLimit { .. }
        | AgentError::CostUnavailable { .. }
        | AgentError::ContextExceeded { .. }
        | AgentError::InvalidCompactionPolicy(_)
        | AgentError::Cancelled
        | AgentError::UsageUncertain
        | AgentError::OutputLimitUnavailable
        | AgentError::InputLimitUnavailable
        | AgentError::ProviderContextPreparation(_)
        | AgentError::NetworkWaitLimit { .. }
        | AgentError::UnknownActiveTools(_)
        | AgentError::ActiveToolSetRefused(_)
        | AgentError::RunEnded
        | AgentError::ControlQueueFull => None,
    }
}

pub(super) fn ai_error_phase(error: &AiError) -> &'static str {
    match error {
        AiError::Config(_)
        | AiError::Batch(_)
        | AiError::Validation(_)
        | AiError::Unsupported(_) => "request preparation",
        AiError::Auth(_) => "authentication",
        AiError::Http(_) => "HTTP response",
        AiError::Transport(error) | AiError::NetworkUnavailable(error) => {
            match (error.phase, error.timeout) {
                (octet_ai::TransportPhase::Connect, false) => "connection",
                (octet_ai::TransportPhase::Connect, true) => "connection timeout",
                (octet_ai::TransportPhase::ResponseHeaders, false) => "response headers",
                (octet_ai::TransportPhase::ResponseHeaders, true) => "response headers timeout",
                (octet_ai::TransportPhase::Body, false) => "response body",
                (octet_ai::TransportPhase::Body, true) => "response body timeout",
            }
        }
        AiError::Provider(_) | AiError::ResponsesFailed(_) => "response body (provider error)",
        AiError::Decode(_) => "response decoding",
        AiError::Pricing(_) => "usage accounting",
        AiError::StreamProtocol(_) => "stream protocol",
        // A mid-stream wrapper reports the phase of the failure that
        // actually ended the stream, so the UI shows e.g. "response
        // decoding", not an uninformative wrapper label.
        AiError::StreamFailure { inner, .. } => ai_error_phase(inner),
        // Decided from the durable permit and generation, before any provider
        // request work, so it is its own phase rather than "request preparation".
        AiError::Deferred(_) => "deferred poll refused",
        AiError::Canceled => "request cancellation",
    }
}

pub(super) fn provider_phase_diagnostic(endpoint: &str, model: &str, phase: &str) -> String {
    format!("provider={endpoint} model={model} phase={phase}")
}
