//! The agent: configuration, the procedural run loop, and run control.

mod background_tools;
mod native_steering;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_core::Stream;
use futures_util::StreamExt;
use octet_ai::{
    AiClient, AiError, AssistantMessage, AssistantPart, AudioPayload, CacheRetention,
    CompatibilityMode, Cost, DecodeError, ImageInputError, ImageInputLimits, ImageSource, Media,
    Message, Modality, Model, OutputFormat, OutputModalities, Protocol, ReasoningConfig,
    ReasoningMode, Request, ResponsesCompactRequest, ResponsesInput, ResponsesOptions,
    ResponsesReplayItem, ServiceTier, StopReason, StreamEvent, ToolCall, ToolCallArgumentError,
    ToolChoice, ToolDef, ToolResult, ToolResultPart, Usage, UserMessage, UserPart,
    PICODOLLARS_PER_MICRODOLLAR,
};
use serde::Serialize;
use tokio::sync::{mpsc, watch};

use crate::compaction::{
    build_handoff_message, build_turn_prefix_handoff_message, choose_first_kept_by_tokens,
    finish_handoff_bounded, prepare_handoff, serialize_conversation, HandoffPreparation,
    DEFAULT_KEEP_RECENT_TOKENS, MAX_COMPACTION_HANDOFF_BYTES, SUMMARIZATION_SYSTEM_PROMPT,
    SUMMARY_OUTPUT_TOKENS, TURN_PREFIX_OUTPUT_TOKENS,
};
use crate::context::{ContextBreakdown, ContextSnapshot, ContextTracker};
use crate::delegation::{
    enable_root_delegation, DelegationBinding, DelegationConfig, DelegationError,
    DelegationRuntimeSettings, DelegationTemplate, SessionDelegationHandle,
};
use crate::effect::{
    EffectBroker, EffectIntent, EffectReservation, ToolEffect, ToolPolicyDenialCode,
};
use crate::events::{
    AgentEvent, CompactionInfo, CompactionKind, CompactionReason, Control as UnreservedControl,
    DeferredRunResumed, DeferredRunSuspended, DelegationTelemetrySnapshot, FinishReason,
    OutputChannel, QueueDeliveryMode, ToolPolicyDecision,
};
use crate::extension::{
    AssistantPersistenceContext, CompactionStrategy, EventObserver, ExtensionHost,
    ProviderRetryAdvice, ProviderRetryContext, ProviderRetryHook, ProviderRetryKind,
    RegisteredPersistenceMetadataHook, ToolCallHook, MAX_PROVIDER_RETRY_ADDITIONAL_DELAY,
    MAX_REFUSED_ACTIVE_TOOL_NAMES,
};
use crate::extension_process::{ExtensionProcess, EXTENSION_FEATURE_AGENT_SESSIONS};
use crate::input::{InputPart, UserInput};
use crate::sandbox::SandboxConfig;
use crate::session::{
    now_unix_millis, DelegatedUsage, EntryId, EntryMetadata, EntryValue, ExtensionEntryMetadata,
    ExtensionMetadataProvenance, Session, SessionError, SessionRunOutcome, SnapcompactCheckpoint,
    UsageRecordKind,
};
use crate::telemetry::{
    schema::{
        CompactionSpan, CompletionAttributes, DeferredRunAttributes, DeferredRunSpan,
        EmptyAttributes, ProviderOperation as SpanOperation, ProviderRequestSpan,
        ProviderStreamSpan, RequestAttributes, RunSpan, SummarySpan, ToolAttributes, ToolSpan,
        TurnSpan,
    },
    spans::{SpanGuard, TelemetryContext},
};
use crate::tool::{
    batch_requests_termination, collect_tool_prompt_contributions, content_hash,
    AdaptivePreviewCoalescer, CancellationToken, OutputStream, PartialOutputCheckpointSink,
    PreviewPublication, ReplaySafety, Tool, ToolConcurrency, ToolContext, ToolError, ToolOutput,
    ToolOutputContentPart, ToolOutputDetails, ToolOutputMediaKind, ToolProgress,
    ToolProgressDecoration, ToolProgressSink, ToolPromptContribution, PROGRESS_CHANNEL_CAPACITY,
};
/// Which permit one resume pass owns: exactly one poll, or observation only.
///
/// Re-exported here because [`Agent::resume_deferred_run`] is the public entry
/// point that consumes it; the durable decision core owns the definition.
pub use crate::tools::deferred::DeferredResumeIntent;
use crate::tools::deferred::{
    AdmittedDeferredPoll, DeferredHandle, DeferredPollCompletion, DeferredPollOutcome,
    DeferredPollRefusal, DeferredResponseDeclaration, DeferredResumeStart, DeferredRunCancellation,
    DeferredRunError, DeferredRunRecord, DeferredStopReason, DeferredSuspendDecision,
    DeferredSuspendFailure, ModelIdentity, SuspendedRunObservation,
};
use crate::tools::durability::{synthesize_interruption, InvocationHandle};
#[cfg(any(unix, windows))]
use crate::tools::{BashCheckpointPublisher, BASH_CHECKPOINT_INTERVAL, BASH_CHECKPOINT_MAX_BYTES};
use crate::tools::{SummarizationRetryPolicy, SummarizationRetryScheduled};

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

const AMBIGUOUS_ACCEPTANCE_HINT: &str =
    "Provider acceptance and failed-attempt usage are uncertain. Inspect provider state before retrying explicitly.";

/// Format an inference-layer error for a user-facing retry or terminal event.
/// The same allow-list is used for both paths so retry messages cannot expose
/// more provider data than the final failure message.
fn public_ai_error_diagnostic(error: &AiError, endpoint: &str, model: &str) -> String {
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
fn detail_diagnostic(prefix: &str, error: &impl std::fmt::Display) -> String {
    let mut diagnostic = prefix.to_string();
    append_provider_field(&mut diagnostic, "detail", Some(&error.to_string()));
    truncate_public_diagnostic(&mut diagnostic);
    diagnostic
}

/// Compact, greppable rendering of mid-stream progress counters.
fn format_stream_progress(progress: &octet_ai::StreamProgress) -> String {
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

const MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES: usize = 2 * 1024;
const MAX_PUBLIC_PROVIDER_FIELD_BYTES: usize = 512;

fn format_http_diagnostic(prefix: &str, error: &octet_ai::HttpError) -> String {
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

fn provider_error_message(body: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body?).ok()?;
    let error = value.get("error").unwrap_or(&value);
    ["message", "detail", "description"]
        .iter()
        .find_map(|field| error.get(*field).and_then(serde_json::Value::as_str))
        .map(ToOwned::to_owned)
        .filter(|message| !message.trim().is_empty())
}

fn append_provider_field(output: &mut String, name: &str, value: Option<&str>) {
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

fn compact_public_provider_field(value: &str) -> String {
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
fn redact_common_secret_patterns(value: &str) -> String {
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

/// Renders the model-visible "available tools" section for `tools`.
///
/// Returns `None` when no registered tool contributes a snippet: an empty
/// section would only enlarge the prompt. Entries follow registration order and
/// carriage is normalized, so the same registration always renders the same
/// bytes (a provider prefix must not churn between turns). The result is
/// bounded by [`MAX_TOOL_PROMPT_SECTION_BYTES`] on a character boundary.
fn render_tool_prompt_section<'a>(tools: impl IntoIterator<Item = &'a dyn Tool>) -> Option<String> {
    let contributions =
        collect_tool_prompt_contributions(tools.into_iter().take(MAX_TOOL_PROMPT_SECTION_TOOLS));
    if contributions.is_empty() {
        return None;
    }
    let mut section = String::from("Available tools:");
    for contribution in contributions {
        section.push_str("\n- ");
        section.push_str(contribution.name.trim());
        section.push_str(": ");
        section.push_str(contribution.snippet.trim());
        for guideline in contribution.guidelines {
            section.push_str("\n  - ");
            section.push_str(guideline.trim());
        }
    }
    if section.len() > MAX_TOOL_PROMPT_SECTION_BYTES {
        let mut end = MAX_TOOL_PROMPT_SECTION_BYTES.saturating_sub('…'.len_utf8());
        while end > 0 && !section.is_char_boundary(end) {
            end -= 1;
        }
        section.truncate(end);
        section.push('…');
    }
    Some(section)
}

fn truncate_public_diagnostic(diagnostic: &mut String) {
    truncate_public_diagnostic_to(diagnostic, MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES)
}

fn truncate_public_diagnostic_to(diagnostic: &mut String, budget: usize) {
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

fn http_status_summary(status: u16) -> Option<&'static str> {
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

fn provider_failure_phase(error: &AgentError) -> Option<&'static str> {
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
        | AgentError::NetworkWaitLimit { .. }
        | AgentError::UnknownActiveTools(_)
        | AgentError::ActiveToolSetRefused(_)
        | AgentError::RunEnded
        | AgentError::ControlQueueFull => None,
    }
}

fn ai_error_phase(error: &AiError) -> &'static str {
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

fn provider_phase_diagnostic(endpoint: &str, model: &str, phase: &str) -> String {
    format!("provider={endpoint} model={model} phase={phase}")
}

/// How an agent decides that a natural no-tool response is complete.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompletionPolicy {
    /// Accept the first normal no-tool response.
    #[default]
    Natural,
    /// Treat a normal no-tool response as a candidate and ask an isolated,
    /// one-token evidence gate whether control should return to the user.
    TerminalGate,
}

/// Autonomous context-reduction strategy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentCompactionMode {
    /// Disable autonomous compaction.
    Disabled,
    /// Generate a provider-independent summary and retain a canonical tail.
    #[default]
    Local,
    /// Use OpenAI Responses native opaque compaction on the active route.
    NativeResponses,
}

/// Configuration for [`Agent::new`].
pub struct AgentConfig {
    /// The inference client.
    pub client: AiClient,
    /// The resolved model to converse with.
    pub model: Model,
    /// The session holding (and persisting) conversation history.
    pub session: Session,
    /// The system prompt (empty string for none).
    pub system: String,
    /// Capability gates and limits for tool execution.
    pub sandbox: SandboxConfig,
    /// Mandatory deterministic broker for every model-requested tool effect.
    pub effect_broker: EffectBroker,
    /// Registered tools and event observers. Register [`CoreTools`](crate::tools::CoreTools)
    /// here for the built-in `read`/`edit`/`write`/`bash`/`search` tools.
    pub extensions: ExtensionHost,
    /// Maximum model turns per run; exceeding it finishes the run with
    /// [`FinishReason::MaxTurns`].  `None` disables the limit.
    pub max_turns: Option<u64>,
    /// Reasoning configuration applied to every model request in this agent's
    /// runs. Use [`ReasoningConfig::Off`] to disable reasoning (the historical
    /// default). Unsupported configurations are rejected by `octet-ai`'s
    /// validation when the run opens its stream, surfacing as
    /// [`FinishReason::Failed`].
    pub reasoning: ReasoningConfig,
    /// Reasoning execution mode applied independently from effort.
    pub reasoning_mode: ReasoningMode,
    /// Prompt-cache retention policy for model turns. Defaults to short in
    /// application configuration, matching pi.
    pub cache_retention: CacheRetention,
    /// Optional explicit cache-affinity ID. When absent, the stable session
    /// path-derived key is used.
    pub session_id: Option<String>,
}

struct RunLifecycle {
    finished: AtomicBool,
    dropped: AtomicBool,
}

/// Owns the session borrow inside the generated run stream. Rust drops stream
/// locals when [`Run`] is dropped, so this guard is the only place that can
/// durably pair unresolved calls before the mutable session borrow is released.
struct RunSessionGuard<'a> {
    session: &'a mut Session,
    lifecycle: Arc<RunLifecycle>,
}

impl std::ops::Deref for RunSessionGuard<'_> {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.session
    }
}

impl std::ops::DerefMut for RunSessionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.session
    }
}

impl Drop for RunSessionGuard<'_> {
    fn drop(&mut self) {
        if !self.lifecycle.finished.load(Ordering::Acquire) {
            // Drop cannot report an I/O error, but attempting the append here
            // closes the old gap where a deliberate stream drop followed by a
            // process crash was mistaken for an unclean tool interruption.
            let _ = persist_pending_cancellations(self.session);
        }
    }
}

/// A stateful agent: one session, one model, one authoritative head.
///
/// The agent owns its [`Session`]; runs borrow the agent mutably
/// (`&mut self`), so there is exactly one mutable head and no cloned or
/// detached conversation state.
pub struct Agent {
    client: AiClient,
    model: Model,
    session: Session,
    extensions: ExtensionHost,
    sandbox: SandboxConfig,
    effect_broker: EffectBroker,
    system: String,
    max_turns: Option<u64>,
    reasoning: ReasoningConfig,
    reasoning_mode: ReasoningMode,
    cache_retention: CacheRetention,
    /// Hard limit for the exact JSON schemas exposed to a provider request.
    tool_schema_budget_bytes: usize,
    /// Optional provider route used for autonomous context summaries.
    /// Defaults to the active model when unset.
    compaction_model: Option<Model>,
    auto_compaction_mode: AgentCompactionMode,
    compaction_threshold_fraction: f64,
    compaction_keep_recent_tokens: u64,
    session_id: String,
    resource_owner: String,
    bash_owner: BashOwnerLease,
    tool_scope: String,
    completion_policy: CompletionPolicy,
    output_modalities: OutputModalities,
    max_output_tokens: u64,
    /// Requested provider service tier for this agent's Responses requests.
    /// `None` sends no tier. Set through [`Agent::set_service_tier`], which
    /// refuses a route whose declared profile does not accept the field.
    service_tier: Option<ServiceTier>,
    /// Stable semantic source key persisted with user-submitted prompts.
    prompt_model_source: Option<String>,
    /// Opt-in model-visible tool section rendered from the registered tools'
    /// prompt contributions. Off by default so every existing host keeps a
    /// byte-identical system prompt; set through
    /// [`Agent::set_tool_prompt_section_enabled`].
    tool_prompt_section: bool,
    /// Opt-in durable partial-output checkpoints for one tool's live calls (row
    /// 4.7). Off by default; set through
    /// [`Agent::enable_partial_output_checkpoints`].
    #[cfg(any(unix, windows))]
    partial_output_checkpoints: Option<PartialOutputCheckpointConfig>,
    prompt_color: Option<String>,
    /// One-shot user-visible text for the next prompt. Model-only context is
    /// persisted in the message body for exact replay instead.
    prompt_display_text: Option<String>,
    max_session_tokens: Option<u64>,
    max_session_cost_microdollars: Option<u64>,
    provider_retries_enabled: bool,
    max_network_wait: Option<Duration>,
    /// Explicit owner-only image presentation; never inherited by child agents.
    owner_tool_images_enabled: bool,
    /// Child sessions owned by the delegation manager are observed by their
    /// parent even when they do not carry a nested delegation binding.
    ultra_observation_managed: bool,
    delegation: Option<DelegationBinding>,
    delegation_model_resolver: Option<Arc<dyn crate::delegation::AgentModelResolver>>,
    last_run_lifecycle: Option<Arc<RunLifecycle>>,
    /// Explicit, caller-owned span observer for the run/turn/provider/tool
    /// boundaries. Inert by default: dropping to
    /// [`NOOP_TELEMETRY_CONTEXT`](crate::telemetry::spans::NOOP_TELEMETRY_CONTEXT)
    /// loses observations, never accounting.
    telemetry: TelemetryContext,
}

// Embedders can reopen the same durable session before dropping the old Agent.
// Retire Bash retention only after the last live Agent with that owner leaves.
static BASH_OWNER_LEASES: std::sync::LazyLock<Mutex<HashMap<String, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

struct BashOwnerLease(String);

impl BashOwnerLease {
    fn acquire(owner: &str) -> Self {
        let mut owners = BASH_OWNER_LEASES
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *owners.entry(owner.to_owned()).or_default() += 1;
        Self(owner.to_owned())
    }
}

impl Drop for BashOwnerLease {
    fn drop(&mut self) {
        let mut owners = BASH_OWNER_LEASES
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let leases = owners.get_mut(&self.0).expect("live Bash owner lease");
        *leases -= 1;
        if *leases == 0 {
            owners.remove(&self.0);
            // Keep retirement serialized with acquisition; the tool offloads
            // filesystem cleanup so no slow unlink runs under this fence.
            crate::tools::BashTool::release_owner(&self.0);
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if self
            .last_run_lifecycle
            .as_ref()
            .is_some_and(|lifecycle| lifecycle.dropped.load(Ordering::Acquire))
        {
            // Persist cancellation before the session owner disappears. This
            // makes dropping a run safe even when the next agent is reopened
            // from the same session file rather than reusing this Agent.
            let _ = persist_pending_cancellations(&mut self.session);
        }

        if let Some(delegation) = &self.delegation {
            delegation.request_shutdown();
        }

        // Tool process groups are owned by per-call RAII guards. There are no
        // persistent shell sessions to clean up when the agent is dropped.
    }
}

/// Aggregate result of [`Agent::complete`].
#[derive(Debug)]
pub struct RunOutput {
    /// Concatenated visible text from all turns.
    pub text: String,
    /// Completed generated media from committed turns, in event order.
    pub media: Vec<Media>,
    /// Total token usage across the run.
    pub usage: Usage,
    /// Known microdollar subtotal for this run, not a full bill when
    /// `Session::has_unpriced_usage` or `Session::has_uncertain_usage` is true.
    pub cost_microdollars: u64,
    /// Session entry ID after the run.
    pub head: EntryId,
    /// How the run ended (never [`FinishReason::Failed`]; failures are
    /// returned as `Err` instead).
    pub reason: FinishReason,
}

/// Conservative estimate of the model-visible input for the next request.
///
/// `structural_tokens` comes from octet's request serializer. When available,
/// `provider_tokens` is the latest tokenizer measurement for the same route
/// and model after the latest compaction, plus structurally estimated trailing
/// messages. `input_tokens` is the larger of those two values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestContextEstimate {
    /// Structural estimate of the complete provider request.
    pub structural_tokens: u64,
    /// Provider-authoritative prefix measurement reconciled to the current head.
    pub provider_tokens: Option<u64>,
    /// Conservative input estimate used by autonomous capacity checks.
    pub input_tokens: u64,
}

/// A streaming agent run: the event stream plus a clonable control handle.
///
/// The run is driven by the caller — poll it with [`Run::next`] (or as a
/// [`Stream`]), typically inside `tokio::select!` alongside user input.
/// Dropping the run cancels the in-flight model stream and any running tool
/// (child processes included).
pub struct Run<'a> {
    stream: Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'a>>,
    control: RunControl,
    lifecycle: Arc<RunLifecycle>,
    context: Arc<ContextTracker>,
    delegation: Option<DelegationBinding>,
}

impl Run<'_> {
    /// Open an extension-negotiated child session by its opaque presentation
    /// reference while this run is active.
    ///
    /// Mirrors [`Agent::open_delegated_session_reference`] so live UI (for
    /// example the mid-run `/subagents` transcript drill-in) can read a worker
    /// transcript read-only without owning the root session. The delegation
    /// manager state lock is only taken to resolve the reference to a path.
    pub fn open_delegated_session_reference(
        &self,
        extension_principal: &str,
        reference: &str,
    ) -> Result<Option<Session>, AgentError> {
        let Some(binding) = self.delegation.as_ref() else {
            return Ok(None);
        };
        binding.open_session_reference(extension_principal, reference)
    }

    /// Returns a clonable handle for sending control messages while the run's
    /// event stream is being consumed.
    pub fn control(&self) -> RunControl {
        self.control.clone()
    }

    /// Returns an owned snapshot of incrementally tracked response,
    /// tool-boundary, and provider token-usage state.
    /// Session-owned delegation handle, when delegation is enabled.
    ///
    /// Host-side callers use it to resolve a worker's launchable interactive
    /// handle (`agent-session:<sha256>` -> session-owned transcript) without
    /// widening what an extension can see.
    pub fn session_delegation(&self) -> Option<SessionDelegationHandle> {
        self.delegation
            .as_ref()
            .map(DelegationBinding::session_handle)
    }

    /// Returns an owned snapshot of incrementally tracked response,
    /// tool-boundary, and provider token-usage state.
    pub fn context_snapshot(&self) -> ContextSnapshot {
        self.context.snapshot()
    }

    /// Consumes the run and returns its settled context snapshot.
    ///
    /// An unfinished run is first marked as dropped, matching the normal
    /// cancellation semantics of [`Drop`]. A run that already delivered its
    /// terminal event retains that terminal state.
    pub fn into_context_snapshot(self) -> ContextSnapshot {
        let context = Arc::clone(&self.context);
        drop(self);
        context.snapshot()
    }

    /// Returns the next event, or `None` after the terminal
    /// [`AgentEvent::RunFinished`] has been delivered.
    pub async fn next(&mut self) -> Option<AgentEvent> {
        self.stream.next().await
    }
}

impl Drop for Run<'_> {
    fn drop(&mut self) {
        if !self.lifecycle.finished.load(Ordering::Acquire) {
            self.lifecycle.dropped.store(true, Ordering::Release);
            self.context.run_dropped();
            if let Some(delegation) = &self.delegation {
                // A dropped run ends the turn, not the fleet. Workers are
                // owned by the session and stay reattachable.
                delegation.detach_run();
            }
        }
    }
}

impl Stream for Run<'_> {
    type Item = AgentEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.stream.as_mut().poll_next(cx)
    }
}

// Reservations follow semantic input out of the bounded ingress channel and
// into pending steering/follow-up batches, until persistence or run termination.
const MAX_PENDING_CONTROL_INPUTS: usize = 64;
const MAX_PENDING_CONTROL_BYTES: usize = 64 * 1024 * 1024;

struct ControlReservation {
    _count: tokio::sync::OwnedSemaphorePermit,
    _bytes: tokio::sync::OwnedSemaphorePermit,
}

struct ReservedPayload {
    input: UserInput,
    // None only for a prepared steering input not yet submitted.
    reservation: Option<ControlReservation>,
}

enum ReservedInput {
    Ready(ReservedPayload),
    Retractable(PreparedSteering),
}

impl ReservedInput {
    fn push_pending(self, pending: &mut Vec<Self>) {
        // Recalled payloads release their permits immediately. Remove their
        // empty queue slots before admitting more, so repeated editing cannot
        // accumulate an unbounded backlog of receipt tombstones.
        pending.retain(Self::is_pending);
        if self.is_pending() {
            pending.push(self);
        }
    }

    fn is_pending(&self) -> bool {
        match self {
            Self::Ready(_) => true,
            Self::Retractable(prepared) => prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some(),
        }
    }

    fn claim(self) -> Option<ReservedPayload> {
        match self {
            Self::Ready(payload) => Some(payload),
            Self::Retractable(prepared) => prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        }
    }
}

/// A single-use steering submission with a receipt available before sending.
///
/// Create with [`Self::new`], keep the receipt in the frontend, and move this
/// value into [`RunControl::steer_retractable`]. Dropping the submission (including
/// a cancelled send future) releases its input and any admission reservation.
/// Unlike the receipt, this value cannot be cloned or submitted twice.
pub struct PreparedSteering {
    receipt: SteeringReceipt,
    owner: Option<Arc<tokio::sync::Semaphore>>,
}

impl PreparedSteering {
    /// Prepares an input and its independently clonable recall receipt.
    /// Preparation does not reserve run capacity or start asynchronous work.
    pub fn new(input: impl Into<UserInput>) -> (Self, SteeringReceipt) {
        let receipt = SteeringReceipt {
            payload: Arc::new(Mutex::new(Some(ReservedPayload {
                input: input.into(),
                reservation: None,
            }))),
            recalled: CancellationToken::default(),
        };
        (
            Self {
                receipt: receipt.clone(),
                owner: None,
            },
            receipt,
        )
    }
}

impl Drop for PreparedSteering {
    fn drop(&mut self) {
        self.receipt
            .payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

/// Clone-safe authority to recall one exact prepared steering input.
/// Identical text in different submissions has independent receipts.
#[derive(Clone)]
pub struct SteeringReceipt {
    payload: Arc<Mutex<Option<ReservedPayload>>>,
    recalled: CancellationToken,
}

impl std::fmt::Debug for SteeringReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteeringReceipt")
            .field("pending", &self.is_pending())
            .finish_non_exhaustive()
    }
}

impl SteeringReceipt {
    /// Whether this input is still eligible for recall. This is a snapshot;
    /// only [`Self::try_retract`] establishes that recall actually won.
    pub fn is_pending(&self) -> bool {
        self.payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    /// Removes this input before its persistence claim, returning true only
    /// for the caller that won recall. Success guarantees no session append or
    /// delivery event for this input and releases any admission reservation.
    ///
    /// Returns false once delivery has claimed the input, even if persistence
    /// is still in progress or later fails; also returns false after an earlier
    /// recall or after the submission is dropped. Receipt clones share this
    /// same one-shot authority. No lock is held during filesystem persistence.
    pub fn try_retract(&self) -> bool {
        let payload = self
            .payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if payload.is_none() {
            return false;
        }
        drop(payload);
        self.recalled.cancel();
        true
    }
}

enum Control {
    SetReasoning(ReasoningConfig),
    Steer(ReservedInput),
    FollowUp(ReservedInput),
    FinishNow(ReservedInput),
    SetSteeringMode(QueueDeliveryMode),
    SetFollowUpMode(QueueDeliveryMode),
    Abort,
}

/// Logical retained payload bytes, including part slots, media, references and
/// transcripts. Count inline data directly rather than allocating base64 JSON.
fn control_input_bytes(input: &UserInput) -> usize {
    input.parts.iter().fold(
        input
            .parts
            .len()
            .saturating_mul(std::mem::size_of::<InputPart>()),
        |total, part| {
            let bytes = match part {
                InputPart::Text(text) => text.len(),
                InputPart::Media(Media::Image(image)) => {
                    let source = match &image.source {
                        ImageSource::Inline(data) => data.len(),
                        ImageSource::Url(url) => url.as_str().len(),
                        ImageSource::ProviderRef(reference) => reference.id.len(),
                    };
                    source.saturating_add(
                        image
                            .media_type
                            .as_ref()
                            .map_or(0, |mime| mime.as_ref().len()),
                    )
                }
                InputPart::Media(Media::Audio(audio)) => {
                    let source = match &audio.payload {
                        AudioPayload::Inline(data) => data.len(),
                        AudioPayload::ProviderRef(reference) => reference.id.len(),
                        AudioPayload::InlineWithProviderRef { data, reference } => {
                            data.len().saturating_add(reference.id.len())
                        }
                    };
                    source.saturating_add(audio.transcript.as_ref().map_or(0, String::len))
                }
            };
            total.saturating_add(bytes)
        },
    )
}

/// Clonable control handle for an active [`Run`].
///
/// Steering, follow-up and FinishNow share a 64-input / 64-MiB logical payload
/// budget, including inputs drained into pending delivery batches. Saturation
/// returns [`AgentError::ControlQueueFull`] before acceptance. Successful sends
/// remain reserved until durable delivery or run termination; cancellation
/// bypasses this queue entirely.
#[derive(Clone)]
pub struct RunControl {
    reasoning_model: Option<Model>,
    ultra_observed: bool,
    admission: Arc<std::sync::Mutex<bool>>,
    tx: mpsc::Sender<Control>,
    pending_count: Arc<tokio::sync::Semaphore>,
    pending_bytes: Arc<tokio::sync::Semaphore>,
    abort: Arc<AbortFlag>,
}

impl RunControl {
    /// Queues a host-authoritative effort change without interrupting generation.
    /// The latest pending selection applies at the next response boundary;
    /// acceptance is not a provider acknowledgement.
    pub async fn set_reasoning(&self, reasoning: ReasoningConfig) -> Result<(), AgentError> {
        let model = self.reasoning_model.as_ref().ok_or_else(|| {
            AgentError::Ai(
                octet_ai::ConfigError::Parse(
                    "reasoning updates require a qualified Responses route".into(),
                )
                .into(),
            )
        })?;
        if reasoning == ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra) {
            require_ultra_observation(&reasoning, self.ultra_observed)?;
            octet_ai::responses::validate_responses_input(
                model,
                &ResponsesInput::default(),
                &reasoning,
                false,
            )?;
        } else {
            validate_reasoning_update(model, &reasoning)?;
        }
        let permit = self.tx.reserve().await.map_err(|_| AgentError::RunEnded)?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(Control::SetReasoning(reasoning));
        Ok(())
    }

    fn reserve_input(&self, input: UserInput) -> Result<ReservedInput, AgentError> {
        let reservation = self.reserve_input_capacity(&input)?;
        Ok(ReservedInput::Ready(ReservedPayload {
            input,
            reservation: Some(reservation),
        }))
    }

    fn reserve_input_capacity(&self, input: &UserInput) -> Result<ControlReservation, AgentError> {
        let bytes = control_input_bytes(input);
        if bytes > MAX_PENDING_CONTROL_BYTES {
            return Err(AgentError::ControlQueueFull);
        }
        let count = self
            .pending_count
            .clone()
            .try_acquire_owned()
            .map_err(|_| AgentError::ControlQueueFull)?;
        let bytes = self
            .pending_bytes
            .clone()
            .try_acquire_many_owned(bytes as u32)
            .map_err(|_| AgentError::ControlQueueFull)?;
        Ok(ControlReservation {
            _count: count,
            _bytes: bytes,
        })
    }

    fn reserve_control(&self, control: UnreservedControl) -> Result<Control, AgentError> {
        if self.tx.is_closed()
            || !*self
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        {
            return Err(AgentError::RunEnded);
        }
        Ok(match control {
            UnreservedControl::Steer(input) => Control::Steer(self.reserve_input(input)?),
            UnreservedControl::FollowUp(input) => Control::FollowUp(self.reserve_input(input)?),
            UnreservedControl::FinishNow(input) => Control::FinishNow(self.reserve_input(input)?),
            UnreservedControl::SetSteeringMode(mode) => Control::SetSteeringMode(mode),
            UnreservedControl::SetFollowUpMode(mode) => Control::SetFollowUpMode(mode),
            UnreservedControl::Abort => Control::Abort,
        })
    }

    async fn send(&self, control: UnreservedControl) -> Result<(), AgentError> {
        let control = self.reserve_control(control)?;
        let permit = self.tx.reserve().await.map_err(|_| AgentError::RunEnded)?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(control);
        Ok(())
    }

    fn try_send(&self, control: UnreservedControl) -> Result<(), AgentError> {
        let control = self.reserve_control(control)?;
        let permit = self.tx.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => AgentError::ControlQueueFull,
            mpsc::error::TrySendError::Closed(_) => AgentError::RunEnded,
        })?;
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(control);
        Ok(())
    }

    /// Injects input into the conversation at the next model-turn boundary of
    /// the active run (persisted to the session when applied).
    pub async fn steer(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.send(UnreservedControl::Steer(input.into())).await
    }

    /// Reserves steering capacity synchronously and returns a submission plus
    /// its receipt before asynchronous sending starts. A frontend can retain
    /// its draft on admission failure. Send through this control (or a clone);
    /// dropping or recalling the prepared value immediately frees capacity.
    pub fn prepare_steer(
        &self,
        input: impl Into<UserInput>,
    ) -> Result<(PreparedSteering, SteeringReceipt), AgentError> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission || self.tx.is_closed() {
            return Err(AgentError::RunEnded);
        }
        let input = input.into();
        let reservation = self.reserve_input_capacity(&input)?;
        let (mut prepared, receipt) = PreparedSteering::new(input);
        prepared
            .receipt
            .payload
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
            .expect("new prepared input")
            .reservation = Some(reservation);
        prepared.owner = Some(self.pending_count.clone());
        Ok((prepared, receipt))
    }

    /// Submits a prepared, retractable steering input at the next safe boundary.
    ///
    /// The receipt can recall local intent, an in-flight send, or accepted
    /// pending input. A recalled submission completes successfully as a no-op;
    /// `Ok(())` is admission, not durable delivery. Normal control budgets and
    /// run-end admission fencing still apply. Cancelling this future before
    /// admission drops its input and releases its reservations. Submitting an
    /// input reserved by a different run returns [`AgentError::RunEnded`].
    pub async fn steer_retractable(&self, prepared: PreparedSteering) -> Result<(), AgentError> {
        if prepared
            .owner
            .as_ref()
            .is_some_and(|owner| !Arc::ptr_eq(owner, &self.pending_count))
        {
            return Err(AgentError::RunEnded);
        }
        if !prepared.receipt.is_pending() {
            return Ok(());
        }
        if self.tx.is_closed()
            || !*self
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        {
            return Err(AgentError::RunEnded);
        }
        {
            let mut payload = prepared
                .receipt
                .payload
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(payload) = payload.as_mut() else {
                return Ok(());
            };
            if payload.reservation.is_none() {
                payload.reservation = Some(self.reserve_input_capacity(&payload.input)?);
            }
        }
        let permit = tokio::select! {
            biased;
            _ = prepared.receipt.recalled.cancelled() => return Ok(()),
            permit = self.tx.reserve() => permit.map_err(|_| AgentError::RunEnded)?,
        };
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*admission {
            return Err(AgentError::RunEnded);
        }
        permit.send(Control::Steer(ReservedInput::Retractable(prepared)));
        Ok(())
    }

    /// Attempts to enqueue steering without allowing a producer to wait behind
    /// the run's bounded control queue.
    pub(crate) fn try_steer(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.try_send(UnreservedControl::Steer(input.into()))
    }

    /// Queues input for after the current run settles: when the model completes
    /// a turn without tool calls, the run continues with this input instead of
    /// finishing.
    pub async fn follow_up(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.send(UnreservedControl::FollowUp(input.into())).await
    }

    /// Requests a final answer at the next safe turn boundary. The supplied
    /// input is persisted like steering, but subsequent requests in this run
    /// expose no tools.
    pub async fn finish_now(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.send(UnreservedControl::FinishNow(input.into())).await
    }

    /// Attempts to enqueue a follow-up without allowing a producer to wait
    /// behind the run's bounded control queue.
    pub(crate) fn try_follow_up(&self, input: impl Into<UserInput>) -> Result<(), AgentError> {
        self.try_send(UnreservedControl::FollowUp(input.into()))
    }

    /// Changes how pending steering messages are delivered.
    pub async fn set_steering_mode(&self, mode: QueueDeliveryMode) -> Result<(), AgentError> {
        self.send(UnreservedControl::SetSteeringMode(mode)).await
    }

    /// Changes how pending follow-up messages are delivered.
    pub async fn set_follow_up_mode(&self, mode: QueueDeliveryMode) -> Result<(), AgentError> {
        self.send(UnreservedControl::SetFollowUpMode(mode)).await
    }

    /// Aborts the run at the next safe boundary: the in-flight model stream is
    /// dropped (cancelling the request) or the running tool is cancelled (child
    /// processes killed). All already-completed session entries are preserved
    /// and the run finishes with exactly one
    /// [`AgentEvent::RunFinished`]`{ reason: FinishReason::Aborted }`.
    pub fn abort(&self) {
        self.abort.set();
    }
}

/// Level-triggered abort signal: reliable regardless of channel capacity and
/// observable both by polling (`is_set`) and awaiting (`wait`).
#[derive(Default)]
struct AbortFlag {
    set: AtomicBool,
    notify: tokio::sync::Notify,
    cancellation: CancellationToken,
}

impl AbortFlag {
    fn set(&self) {
        self.set.store(true, Ordering::Release);
        self.cancellation.cancel();
        self.notify.notify_waiters();
    }

    fn is_set(&self) -> bool {
        self.set.load(Ordering::Acquire) || self.cancellation.is_cancelled()
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_set() {
                return;
            }
            tokio::select! {
                _ = notified => {},
                _ = self.cancellation.cancelled() => return,
            }
        }
    }
}

/// Fallback when the model has no declared image bounds. This is a host safety
/// ceiling, not a claim about what any particular provider accepts.
const FALLBACK_IMAGE_LIMITS: ImageInputLimits = ImageInputLimits {
    max_width: 4_000,
    max_height: 4_000,
    max_bytes: octet_ai::MAX_USER_IMAGE_BYTES,
};

// Also bounds aggregate decode work: each of at most eight images is subject
// to octet-ai's 16-million-pixel and bounded-resize limits.
const MAX_IMAGES_PER_INPUT: usize = 8;
const MAX_IMAGE_INPUT_BYTES: usize = 20 * 1024 * 1024;

/// Stop a detached blocking decoder at the next image when its async owner ends.
struct CancelBlockingImages(Arc<AtomicBool>);

impl Drop for CancelBlockingImages {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Check the entire batch before spawning any decode work or appending history.
fn image_preparation_limits(
    input: &UserInput,
    model: &Model,
) -> Result<ImageInputLimits, AgentError> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for part in &input.parts {
        if let InputPart::Media(Media::Image(image)) = part {
            count += 1;
            if let ImageSource::Inline(data) = &image.source {
                if data.len() > octet_ai::MAX_USER_IMAGE_BYTES {
                    return Err(ImageInputError::InputTooLarge.into());
                }
                bytes = bytes.saturating_add(data.len());
            }
            if count > MAX_IMAGES_PER_INPUT || bytes > MAX_IMAGE_INPUT_BYTES {
                return Err(AgentError::ImageInputBatchLimit);
            }
        }
    }
    if count > 0
        && !model
            .spec
            .effective_input_modalities()
            .contains(Modality::Image)
    {
        return Err(AiError::Unsupported(octet_ai::UnsupportedError::Image).into());
    }
    let limits = model
        .spec
        .preset
        .image_input_limits
        .unwrap_or(FALLBACK_IMAGE_LIMITS);
    if count > 0 {
        limits.validate()?;
    }
    Ok(limits)
}

/// Transform canonical input off the async worker, atomically before history
/// append. Cancellation stops between images and never commits a partial batch.
async fn prepare_user_images(
    mut input: UserInput,
    model: &Model,
    abort: Option<&AbortFlag>,
) -> Result<UserInput, AgentError> {
    let limits = image_preparation_limits(&input, model)?;
    if abort.is_some_and(AbortFlag::is_set) {
        return Err(AgentError::Cancelled);
    }
    if !input.parts.iter().any(|part| {
        matches!(part,
            InputPart::Media(Media::Image(image)) if matches!(image.source, ImageSource::Inline(_))
        )
    }) {
        return Ok(input);
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelBlockingImages(Arc::clone(&cancelled));
    let worker = tokio::task::spawn_blocking(move || {
        for part in &mut input.parts {
            if cancelled.load(Ordering::Acquire) {
                return Err(AgentError::Cancelled);
            }
            if let InputPart::Media(Media::Image(image)) = part {
                *image = octet_ai::prepare_user_image(image, limits)?;
            }
        }
        Ok(input)
    });
    let result = if let Some(abort) = abort {
        tokio::select! {
            biased;
            _ = abort.wait() => Err(AgentError::Cancelled),
            result = worker => result.map_err(|_| AgentError::ImagePreparationFailed)?,
        }
    } else {
        worker
            .await
            .map_err(|_| AgentError::ImagePreparationFailed)?
    };
    drop(guard);
    if abort.is_some_and(AbortFlag::is_set) {
        return Err(AgentError::Cancelled);
    }
    result
}

fn user_message(input: UserInput) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: input.into_user_parts(),
    }))
}

struct ObserverDispatch {
    observers: Vec<Arc<dyn EventObserver>>,
    resource_owner: String,
}

fn notify_observers(observers: &ObserverDispatch, event: &AgentEvent) {
    for observer in &observers.observers {
        observer.on_event_for_owner(event, &observers.resource_owner);
    }
}

/// Minimum context headroom retained for an ordinary coding turn. This is a
/// compaction policy, not the provider request's output ceiling.
const DEFAULT_COMPACTION_RESERVE_TOKENS: u64 = 16 * 1024;
/// Leave room for a visible answer after token-budget reasoning when the model
/// advertises enough output capacity.
const REASONING_ANSWER_RESERVE: u64 = 1024;
/// Bound actual tool executions emitted in one assistant turn. Every excess
/// call still receives a compact error result so provider pairing remains valid.
const MAX_TOOL_CALLS_PER_TURN: usize = 32;
/// Bound read fan-out independently of the Tokio worker pool. Consecutive
/// eligible calls are split into ordered waves of this width; every other
/// effect is a barrier.
const MAX_PARALLEL_READ_WAVE_WIDTH: usize = 4;
/// Number of recent identical calls retained for the generic no-progress hint.
const MAX_RECENT_TOOL_CALLS: usize = 16;
/// Do not distract the model for the first two legitimate repeated probes.
const REPEATED_TOOL_CALL_THRESHOLD: usize = 2;
const FAILED_TURN_CONTEXT_MARKER: &str = "The previous assistant turn failed before completion. Do not continue that request unless the user asks again.";
const TOOL_TRUNCATION_MARKER: &str = "\n[tool output truncated]\n";
/// Maximum retries for a transient provider failure. A replacement attempt is
/// safe even after deltas were received: streamed output is provisional, the
/// assistant message is persisted only after `Finished`, and tools are not
/// executed until that point.
const MAX_PROVIDER_RETRIES: usize = 3;
/// Total time one retry decision may spend in extension advisory hooks.
/// Hooks run only after the host has independently admitted the retry.
const PROVIDER_RETRY_HOOK_BUDGET: Duration = Duration::from_millis(200);
/// Total time a completed assistant turn may spend collecting extension-owned
/// metadata before its atomic durable append. Timeout or cancellation drops
/// only extension metadata, never the canonical assistant result.
const PERSISTENCE_METADATA_HOOK_BUDGET: Duration = Duration::from_millis(200);
/// Non-timeout network failures are usually short-lived connection loss. Five
/// visible, cancellable replacement attempts give the connection time to
/// recover without charging usage or consuming an autonomous model turn.
const MAX_NETWORK_RETRIES: usize = 5;
/// UTF-8 byte budget for the opt-in model-visible "available tools" section
/// rendered from the registered tools' `promptSnippet`/`promptGuidelines`
/// contributions. A section that would exceed it is truncated on a character
/// boundary, so no registered tool can enlarge a system prompt without bound.
const MAX_TOOL_PROMPT_SECTION_BYTES: usize = 8 * 1024;
/// Hard byte budget for the exact JSON array of provider-visible tool schemas.
///
/// This is intentionally independent from the prose prompt-section cap: schema
/// parameters can be arbitrarily large JSON values. A request over the budget
/// is refused rather than dropping or rewriting any registered tool.
pub const DEFAULT_TOOL_SCHEMA_BUDGET_BYTES: usize = 128 * 1024;
/// Maximum number of tools rendered into that section, in registration order.
const MAX_TOOL_PROMPT_SECTION_TOOLS: usize = 64;
const TERMINAL_GATE_SYSTEM: &str = r#"You gate control flow for a coding agent. Output R when the candidate is a valid response to return to the user now: a substantiated completion, an answer or plan based on supplied text or general knowledge, a necessary clarification, an honest blocker or uncertainty, or a justified refusal. Output C when autonomous work should continue: promised next action, unsupported claim about current state, or requested repository or external action not substantiated by relevant successful action evidence. Do not treat an irrelevant or failed action as evidence. Respect explicit requests not to use tools or to guess. Output exactly R or C."#;
const TERMINAL_GATE_CORRECTION: &str = "The candidate response was not returnable: requested current-state or action work is not supported by relevant successful tool evidence. Continue the work using the available tools; do not repeat the rejected candidate.";
const TERMINAL_GATE_ATTEMPTS: usize = 2;
const TERMINAL_GATE_TEXT_LIMIT: usize = 3_000;
const TERMINAL_GATE_RECEIPT_LIMIT: usize = 24;
const TERMINAL_GATE_ARGUMENT_LIMIT: usize = 400;
const TERMINAL_GATE_RESULT_LIMIT: usize = 600;
// Registered names fit unchanged; also bound unknown names emitted by a provider.
const TERMINAL_GATE_TOOL_NAME_LIMIT: usize = 256;
// Keep the initial request and a rolling suffix. Both count and UTF-8 bytes
// matter: empty controls must not grow the list, nor may multibyte text evade it.
// This byte budget always fits the initial and latest 3,000-character summaries.
const TERMINAL_GATE_REQUEST_LIMIT: usize = 8;
const TERMINAL_GATE_REQUEST_BYTES: usize = 32 * 1024;
// The field/count limits below fit even JSON's worst-case six bytes per char,
// plus keys, delimiters and omission counters. This is not a context-limit bypass.
const TERMINAL_GATE_CAPSULE_BYTES: usize = 384 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalGateDecision {
    Return,
    Continue,
}

#[derive(Debug, Serialize)]
struct TerminalActionReceipt {
    tool: String,
    arguments: String,
    status: &'static str,
    result: String,
}

/// Lossy gate-only evidence, never the authoritative input or tool result.
/// Natural runs have no instance and perform none of these projections.
#[derive(Default)]
struct TerminalGateEvidence {
    prior_context: String,
    requests: VecDeque<String>,
    request_bytes: usize,
    requests_omitted: usize,
    receipts: VecDeque<TerminalActionReceipt>,
    actions_omitted: usize,
}

impl TerminalGateEvidence {
    fn for_run(
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

    fn record_request(&mut self, summary: &str) {
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

    fn record_action(&mut self, tool: &str, arguments: &str, is_error: bool, result: &str) {
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

struct CompletedToolExecution {
    result: Result<ToolOutput, ToolError>,
    /// Host-owned effect admission result, absent only when the call never
    /// reached a registered tool's effect boundary.
    policy_decision: Option<ToolPolicyDecision>,
    /// Wall time taken for the call.
    duration: std::time::Duration,
    /// Unix milliseconds just before the tool's effects were admitted
    /// (`None` when the call never reached the effect gate).
    started_unix_ms: Option<u64>,
    /// Unix milliseconds when the call's outcome was finalized.
    finished_unix_ms: Option<u64>,
    progress_rx: mpsc::Receiver<ToolProgress>,
    progress_sink: ToolProgressSink,
    cancellation_won: bool,
}

/// Synthetic, secret-safe result for a normalized call rejected by the exact
/// request schema. Keep this static: provider arguments may contain secrets.
const SCHEMA_MISMATCH_TOOL_ERROR: &str =
    "tool call was not executed because its arguments do not satisfy the advertised schema; correct the arguments and try again";

fn rejected_argument_tool_error(error: ToolCallArgumentError) -> ToolError {
    let message = match error {
        ToolCallArgumentError::SchemaMismatch => SCHEMA_MISMATCH_TOOL_ERROR,
    };
    ToolError::new(message)
}

fn rejected_argument_tool_execution(
    error: ToolCallArgumentError,
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
) -> CompletedToolExecution {
    let (progress_tx, progress_rx) = mpsc::channel(PROGRESS_CHANNEL_CAPACITY);
    CompletedToolExecution {
        result: Err(rejected_argument_tool_error(error)),
        // The exact request schema rejected this call before tool effect
        // classification. Still expose the host-owned pre-admission denial so
        // policy diagnostics remain complete without exposing provider args.
        policy_decision: Some(policy_decision(
            sandbox,
            broker,
            None,
            None,
            Some(ToolPolicyDenialCode::InvalidToolArguments),
        )),
        duration: std::time::Duration::ZERO,
        started_unix_ms: None,
        finished_unix_ms: Some(crate::session::now_unix_millis()),
        progress_rx,
        progress_sink: ToolProgressSink::live(progress_tx),
        cancellation_won: false,
    }
}

struct ToolEffectAdmission {
    intent: EffectIntent,
    reservation: EffectReservation,
    effect: ToolEffect,
}

struct ToolEffectAdmissionError {
    error: ToolError,
    decision: ToolPolicyDecision,
}

fn policy_decision(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: Option<ToolEffect>,
    authorization: Option<crate::effect::EffectAuthorization>,
    denial_code: Option<ToolPolicyDenialCode>,
) -> ToolPolicyDecision {
    ToolPolicyDecision {
        effect,
        allowed: authorization.is_some(),
        authorization,
        denial_code,
        policy: sandbox.effective_tool_policy(broker.policy()),
    }
}

fn denied_tool_policy(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: Option<ToolEffect>,
    denial_code: ToolPolicyDenialCode,
    message: impl Into<String>,
) -> (ToolError, ToolPolicyDecision) {
    let decision = policy_decision(sandbox, broker, effect, None, Some(denial_code));
    let error = ToolError::policy_denied(denial_code, message);
    (error, decision)
}

/// Classify pre-admission argument parsing failures without retaining provider
/// argument text in an error result or diagnostic.
fn invalid_tool_arguments_denial(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
) -> (ToolError, ToolPolicyDecision) {
    denied_tool_policy(
        sandbox,
        broker,
        None,
        ToolPolicyDenialCode::InvalidToolArguments,
        "invalid tool arguments",
    )
}

/// Classify a trusted hook veto without exposing hook-provided text to the model.
fn secondary_hook_denial(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: Option<ToolEffect>,
) -> (ToolError, ToolPolicyDecision) {
    denied_tool_policy(
        sandbox,
        broker,
        effect,
        ToolPolicyDenialCode::SecondaryHookDenied,
        "tool call denied by host policy",
    )
}

/// Classify a reservation that was invalidated between admission and dispatch.
fn effect_reservation_commit_denial(
    sandbox: &SandboxConfig,
    broker: &EffectBroker,
    effect: ToolEffect,
    error: &crate::effect::EffectBrokerError,
) -> (ToolError, ToolPolicyDecision) {
    denied_tool_policy(
        sandbox,
        broker,
        Some(effect),
        error.policy_denial_code(),
        "effect reservation could not be committed",
    )
}

/// Replace an earlier broker admission with a later trusted tool-boundary
/// denial, such as a resolved symlink escaping workspace confinement.
fn apply_execution_policy_denial(
    policy_decision: &mut Option<ToolPolicyDecision>,
    result: &Result<ToolOutput, ToolError>,
) {
    let (Some(decision), Err(error)) = (policy_decision, result) else {
        return;
    };
    let Some(denial_code) = error.policy_denial_code() else {
        return;
    };
    decision.allowed = false;
    decision.authorization = None;
    decision.denial_code = Some(denial_code);
}

fn effect_is_repeatable_observation(effect: ToolEffect) -> bool {
    matches!(effect, ToolEffect::Pure | ToolEffect::WorkspaceRead)
}

/// Live read scheduling is broader than crash replay. Host reads are allowed
/// to overlap only after the exact host-owned classification and policy
/// admission; they remain ineligible for automatic recovery after a crash.
fn effect_is_parallel_observation(effect: ToolEffect) -> bool {
    matches!(
        effect,
        ToolEffect::Pure | ToolEffect::WorkspaceRead | ToolEffect::HostRead
    )
}

#[allow(clippy::too_many_arguments)]
async fn reserve_tool_effect(
    broker: &EffectBroker,
    tool: &dyn Tool,
    name: &str,
    arguments: &serde_json::Value,
    context: &ToolContext<'_>,
    principal: &str,
    run_id: &str,
    generation: u64,
    request_id: &octet_ai::ToolCallId,
    interactive: bool,
) -> Result<ToolEffectAdmission, ToolEffectAdmissionError> {
    let effect = tool.effect(arguments, context).map_err(|error| {
        let denial_code = error
            .policy_denial_code()
            .unwrap_or(ToolPolicyDenialCode::InvalidToolArguments);
        ToolEffectAdmissionError {
            decision: policy_decision(context.sandbox, broker, None, None, Some(denial_code)),
            error,
        }
    })?;
    let intent = EffectIntent::new(
        principal,
        run_id,
        generation,
        request_id.0.clone(),
        name,
        effect,
        arguments,
    )
    .map_err(|error| {
        let denial_code = error.policy_denial_code();
        ToolEffectAdmissionError {
            decision: policy_decision(
                context.sandbox,
                broker,
                Some(effect),
                None,
                Some(denial_code),
            ),
            error: ToolError::new(error.to_string()),
        }
    })?;
    let reservation = broker
        .reserve(&intent, interactive.then_some(&context.progress))
        .await
        .map_err(|error| {
            let denial_code = error.policy_denial_code();
            ToolEffectAdmissionError {
                decision: policy_decision(
                    context.sandbox,
                    broker,
                    Some(effect),
                    None,
                    Some(denial_code),
                ),
                error: ToolError::new(error.to_string()),
            }
        })?;
    Ok(ToolEffectAdmission {
        intent,
        reservation,
        effect,
    })
}

struct DeferredParallelAfterToolCall {
    name: String,
    arguments: serde_json::Value,
    progress_sink: ToolProgressSink,
}

struct ParallelReadWaveExecution {
    execution: CompletedToolExecution,
    after: Option<DeferredParallelAfterToolCall>,
}

/// Terminal status of one tool boundary: a hard error or a tool-reported
/// error result is an error span; a tool-reported success is not.
fn tool_execution_failed(result: &Result<ToolOutput, ToolError>) -> bool {
    match result {
        Ok(output) => output.is_error(),
        Err(_) => true,
    }
}

struct AdmittedParallelReadCall {
    tool: Arc<dyn Tool>,
    name: String,
    arguments: serde_json::Value,
    execute_arguments: serde_json::Value,
    progress_rx: mpsc::Receiver<ToolProgress>,
    progress_sink: ToolProgressSink,
    policy_decision: ToolPolicyDecision,
    start: std::time::Instant,
    started_unix_ms: u64,
}

enum ParallelReadPreparation {
    Admitted(Box<AdmittedParallelReadCall>),
    Completed(Box<ParallelReadWaveExecution>),
}

fn advertised_tool_definition(tool: &dyn Tool, model: &Model) -> ToolDef {
    let mut definition = tool.definition();
    // Static parallel capability permits scheduling hints, never effects.
    // Exact argument classification and broker admission still gate dispatch.
    if model.responses_features().async_tools && tool.concurrency() == ToolConcurrency::Parallel {
        definition.async_execution = true;
    }
    definition
}

fn parallel_read_candidate(
    call: &ToolCall,
    call_index: usize,
    answer_only: bool,
    output_truncated: bool,
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    context: &ToolContext<'_>,
) -> bool {
    call_index < MAX_TOOL_CALLS_PER_TURN
        && !answer_only
        && !output_truncated
        && call.argument_error.is_none()
        && call.arguments_value().is_ok_and(|arguments| {
            tool_map.get(&call.name).is_some_and(|tool| {
                tool.concurrency() == ToolConcurrency::Parallel
                    && tool
                        .effect(&arguments, context)
                        .is_ok_and(effect_is_parallel_observation)
            })
        })
}

fn completed_parallel_read_execution(
    result: Result<ToolOutput, ToolError>,
    policy_decision: Option<ToolPolicyDecision>,
    progress_rx: mpsc::Receiver<ToolProgress>,
    progress_sink: ToolProgressSink,
    start: std::time::Instant,
    cancellation_won: bool,
) -> Box<ParallelReadWaveExecution> {
    Box::new(ParallelReadWaveExecution {
        execution: CompletedToolExecution {
            result,
            policy_decision,
            duration: start.elapsed(),
            started_unix_ms: None,
            finished_unix_ms: Some(crate::session::now_unix_millis()),
            progress_rx,
            progress_sink,
            cancellation_won,
        },
        after: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn prepare_parallel_read_call(
    invocation: InvocationHandle,
    tool: Arc<dyn Tool>,
    hooks: &[Arc<dyn ToolCallHook>],
    broker: &EffectBroker,
    run_id: &str,
    generation: u64,
    request_id: &octet_ai::ToolCallId,
    name: &str,
    arguments: serde_json::Value,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> ParallelReadPreparation {
    let start = std::time::Instant::now();
    let (progress_tx, progress_rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let progress_sink = ToolProgressSink::live(progress_tx).with_invocation(invocation);
    let tool_ctx = ToolContext {
        workspace: &sandbox.workspace,
        sandbox,
        execution_scope: tool_scope,
        resource_owner,
        active_skills,
        registered_tools,
        progress: progress_sink.clone(),
        cancellation: cancellation.clone(),
    };

    let admission = reserve_tool_effect(
        broker,
        tool.as_ref(),
        name,
        &arguments,
        &tool_ctx,
        resource_owner,
        run_id,
        generation,
        request_id,
        false,
    )
    .await;
    let ToolEffectAdmission {
        intent,
        reservation: effect_reservation,
        effect,
    } = match admission {
        Ok(admission) => admission,
        Err(ToolEffectAdmissionError { error, decision }) => {
            let cancellation_won = cancellation.is_cancelled();
            let result = if cancellation_won {
                Err(cancelled_tool_error())
            } else {
                Err(error)
            };
            return ParallelReadPreparation::Completed(completed_parallel_read_execution(
                result,
                Some(decision),
                progress_rx,
                progress_sink,
                start,
                cancellation_won,
            ));
        }
    };

    let mut hook_denial = None;
    let mut cancellation_won = false;
    for hook in hooks {
        let hook_result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = hook.before_tool_call(name, &arguments, &tool_ctx) => Some(result),
        };
        let Some(hook_result) = hook_result else {
            cancellation_won = true;
            break;
        };
        // A hook can synchronously cause cancellation while returning. The
        // level-triggered check keeps cancellation ahead of a same-poll denial.
        if cancellation.is_cancelled() {
            cancellation_won = true;
            break;
        }
        if hook_result.is_err() {
            hook_denial = Some(());
            break;
        }
    }
    if cancellation_won || cancellation.is_cancelled() {
        return ParallelReadPreparation::Completed(completed_parallel_read_execution(
            Err(cancelled_tool_error()),
            None,
            progress_rx,
            progress_sink,
            start,
            true,
        ));
    }
    if hook_denial.is_some() {
        let (error, decision) = secondary_hook_denial(sandbox, broker, Some(effect));
        return ParallelReadPreparation::Completed(completed_parallel_read_execution(
            Err(error),
            Some(decision),
            progress_rx,
            progress_sink,
            start,
            false,
        ));
    }
    if cancellation.is_cancelled() {
        return ParallelReadPreparation::Completed(completed_parallel_read_execution(
            Err(cancelled_tool_error()),
            None,
            progress_rx,
            progress_sink,
            start,
            true,
        ));
    }

    // Preserve the original hook arguments while completing the potentially
    // large execution allocation before the reservation is consumed.
    let execute_arguments = arguments.clone();
    let receipt = match effect_reservation.commit(&intent) {
        Ok(receipt) => receipt,
        Err(error) => {
            let (error, decision) =
                effect_reservation_commit_denial(sandbox, broker, effect, &error);
            return ParallelReadPreparation::Completed(completed_parallel_read_execution(
                Err(error),
                Some(decision),
                progress_rx,
                progress_sink,
                start,
                false,
            ));
        }
    };
    let policy_decision = policy_decision(
        sandbox,
        broker,
        Some(effect),
        Some(receipt.authorization()),
        None,
    );
    let started_unix_ms = crate::session::now_unix_millis();
    ParallelReadPreparation::Admitted(Box::new(AdmittedParallelReadCall {
        tool,
        name: name.to_owned(),
        arguments,
        execute_arguments,
        progress_rx,
        progress_sink,
        policy_decision,
        start,
        started_unix_ms,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn execute_admitted_parallel_read(
    admitted: AdmittedParallelReadCall,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> ParallelReadWaveExecution {
    let AdmittedParallelReadCall {
        tool,
        name,
        arguments,
        execute_arguments,
        progress_rx,
        progress_sink,
        policy_decision,
        start,
        started_unix_ms,
    } = admitted;
    let tool_ctx = ToolContext {
        workspace: &sandbox.workspace,
        sandbox,
        execution_scope: tool_scope,
        resource_owner,
        active_skills,
        registered_tools,
        progress: progress_sink.clone(),
        cancellation: cancellation.clone(),
    };
    let execute = tool.execute(execute_arguments, &tool_ctx);
    tokio::pin!(execute);
    let mut cancellation_won = false;
    let execution_result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            cancellation_won = true;
            Err(cancelled_tool_error())
        }
        result = &mut execute => result,
    };
    let result = if cancellation_won || cancellation.is_cancelled() {
        cancellation_won = true;
        Err(cancelled_tool_error())
    } else {
        execution_result
    };
    ParallelReadWaveExecution {
        execution: CompletedToolExecution {
            result,
            policy_decision: Some(policy_decision),
            duration: start.elapsed(),
            started_unix_ms: Some(started_unix_ms),
            finished_unix_ms: Some(crate::session::now_unix_millis()),
            progress_rx,
            progress_sink: progress_sink.clone(),
            cancellation_won,
        },
        after: Some(DeferredParallelAfterToolCall {
            name,
            arguments,
            progress_sink,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_parallel_read_wave(
    calls: &[ToolCall],
    invocations: &[InvocationHandle],
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    hooks: &[Arc<dyn ToolCallHook>],
    broker: &EffectBroker,
    run_id: &str,
    generation: u64,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> Vec<ParallelReadWaveExecution> {
    let mut results: Vec<Option<ParallelReadWaveExecution>> =
        (0..calls.len()).map(|_| None).collect();
    let mut executions = futures_util::stream::FuturesUnordered::new();

    for (index, call) in calls.iter().enumerate() {
        let arguments = call
            .arguments_value()
            .expect("parallel read wave validates arguments before admission");
        let prepared = prepare_parallel_read_call(
            invocations[index].clone(),
            Arc::clone(
                tool_map
                    .get(&call.name)
                    .expect("parallel read wave validates registered tools"),
            ),
            hooks,
            broker,
            run_id,
            generation,
            &call.id,
            &call.name,
            arguments,
            sandbox,
            tool_scope,
            resource_owner,
            active_skills,
            registered_tools,
            cancellation.clone(),
        )
        .await;
        match prepared {
            ParallelReadPreparation::Completed(execution) => results[index] = Some(*execution),
            ParallelReadPreparation::Admitted(admitted) => {
                let execution_cancellation = cancellation.clone();
                executions.push(async move {
                    (
                        index,
                        execute_admitted_parallel_read(
                            *admitted,
                            sandbox,
                            tool_scope,
                            resource_owner,
                            active_skills,
                            registered_tools,
                            execution_cancellation,
                        )
                        .await,
                    )
                });
                // Poll once after each commit so dispatch is not deferred until
                // all reservations in this wave have been consumed. This is a
                // deterministic executor handoff, not a timing-based delay.
                let _ = futures_util::future::poll_fn(|cx| {
                    match Pin::new(&mut executions).poll_next(cx) {
                        std::task::Poll::Ready(Some((index, execution))) => {
                            results[index] = Some(execution);
                            std::task::Poll::Ready(())
                        }
                        _ => std::task::Poll::Ready(()),
                    }
                })
                .await;
            }
        }
    }
    while let Some((index, execution)) = executions.next().await {
        results[index] = Some(execution);
    }
    results
        .into_iter()
        .map(|execution| execution.expect("parallel read wave produces one result per call"))
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn run_parallel_after_tool_hooks(
    after: DeferredParallelAfterToolCall,
    hooks: &[Arc<dyn ToolCallHook>],
    result: &Result<ToolOutput, ToolError>,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) {
    let DeferredParallelAfterToolCall {
        name,
        arguments,
        progress_sink,
    } = after;
    let (output, is_error) = match result {
        Ok(output) => (output.text.as_str(), output.is_error()),
        Err(error) => (error.message.as_str(), true),
    };
    let tool_ctx = ToolContext {
        workspace: &sandbox.workspace,
        sandbox,
        execution_scope: tool_scope,
        resource_owner,
        active_skills,
        registered_tools,
        progress: progress_sink,
        cancellation,
    };
    for hook in hooks {
        hook.after_tool_call(&name, &arguments, output, is_error, &tool_ctx)
            .await;
    }
}

#[cfg(test)]
thread_local! {
    static TERMINAL_GATE_TEXT_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TERMINAL_GATE_INITIAL_SUMMARIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn bounded_gate_text(text: &str, max_chars: usize) -> String {
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

fn message_visible_text(message: &Message) -> Option<String> {
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

fn recent_conversational_context(messages: &[Message]) -> String {
    let mut selected = messages
        .iter()
        .rev()
        .filter_map(message_visible_text)
        .take(2)
        .collect::<Vec<_>>();
    selected.reverse();
    bounded_gate_text(&selected.join("\n---\n"), TERMINAL_GATE_TEXT_LIMIT)
}

fn terminal_gate_capsule(evidence: &TerminalGateEvidence, candidate: &AssistantMessage) -> String {
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

fn parse_terminal_gate(response: &octet_ai::Response) -> Option<TerminalGateDecision> {
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

fn continuation_instruction(stop_reason: &StopReason) -> &'static str {
    match stop_reason {
        StopReason::MaxTokens => {
            "The previous response was truncated at the token limit. Continue the task from the persisted state; do not claim completion until the work is finished and verified."
        }
        StopReason::Other(reason) if reason == "tool_output_locked" => {
            "The previous response emitted an internal locked-output placeholder instead of the intended structured call. Re-issue that tool call now using the provider's required tool-call format; do not print any control placeholder."
        }
        _ => {
            "The provider paused the turn. Continue the task from the persisted state and do not claim completion until the work is finished and verified."
        }
    }
}

fn next_tool_scope() -> String {
    static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
    format!(
        "agent-{}-{}",
        std::process::id(),
        NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
    )
}

fn reasoning_token_budget(model: &Model, reasoning: &ReasoningConfig) -> u64 {
    match reasoning {
        ReasoningConfig::Budget(budget) => *budget,
        ReasoningConfig::Effort(effort) => model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .filter(|capability| capability.control == octet_ai::ReasoningControl::TokenBudget)
            .and_then(|capability| {
                let budgets = capability.effort_budgets?;
                let effort = (*effort).min(capability.max_effort);
                Some(match effort {
                    octet_ai::ReasoningEffort::Minimal => budgets.minimal,
                    octet_ai::ReasoningEffort::Low => budgets.low,
                    octet_ai::ReasoningEffort::Medium => budgets.medium,
                    octet_ai::ReasoningEffort::High => budgets.high,
                    octet_ai::ReasoningEffort::Xhigh => budgets.xhigh,
                    octet_ai::ReasoningEffort::Max | octet_ai::ReasoningEffort::Ultra => {
                        budgets.max
                    }
                })
            })
            .unwrap_or_default(),
        ReasoningConfig::Off | ReasoningConfig::On => 0,
    }
}

fn agent_compaction_reserve_tokens(model: &Model, reasoning: &ReasoningConfig) -> u64 {
    let model_max = model.spec.limits.max_output_tokens.max(1);
    let reasoning_floor = reasoning_token_budget(model, reasoning)
        .saturating_add(REASONING_ANSWER_RESERVE)
        .min(model_max);
    DEFAULT_COMPACTION_RESERVE_TOKENS
        .max(reasoning_floor)
        .min(model_max)
}

/// Headroom reserved between the per-request input estimate and the provider's
/// own input count.
///
/// The estimate is bytes/4 plus structural overhead, while a provider counts
/// with its own tokenizer and chat template. Sizing the request as exactly
/// `window - estimate` therefore sits on the boundary, where a one-token
/// difference is a hard rejection: a real vLLM deployment answered
/// "maximum context length is 131072 tokens ... you requested 30896 output
/// tokens and your prompt contains at least 100177 input tokens, for a total of
/// at least 131073 tokens". Reserving bounded slack keeps `input + output`
/// inside the window without meaningfully shrinking a decoded answer.
const REQUEST_OUTPUT_HEADROOM_PERCENT: u64 = 1;
const REQUEST_OUTPUT_HEADROOM_DIVISOR: u64 = 100;
const REQUEST_OUTPUT_HEADROOM_MINIMUM: u64 = 256;
const REQUEST_OUTPUT_HEADROOM_MAXIMUM: u64 = 4096;

fn request_output_headroom(context_window: u64) -> u64 {
    ((context_window / REQUEST_OUTPUT_HEADROOM_DIVISOR) * REQUEST_OUTPUT_HEADROOM_PERCENT).clamp(
        REQUEST_OUTPUT_HEADROOM_MINIMUM,
        REQUEST_OUTPUT_HEADROOM_MAXIMUM,
    )
}

fn resolve_request_max_output_tokens(
    context_window: u64,
    input_tokens: u64,
    provider_output_ceiling: u64,
) -> u64 {
    provider_output_ceiling.min(
        context_window
            .saturating_sub(input_tokens)
            .saturating_sub(request_output_headroom(context_window)),
    )
}

fn add_usage(total: &mut Usage, turn: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(turn.input_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(turn.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(turn.cache_write_tokens);
    total.cache_write_1h_tokens = total
        .cache_write_1h_tokens
        .saturating_add(turn.cache_write_1h_tokens);
    total.output_tokens = total.output_tokens.saturating_add(turn.output_tokens);
    total.reasoning_tokens = total.reasoning_tokens.saturating_add(turn.reasoning_tokens);
    total.total_tokens = total.total_tokens.saturating_add(turn.total_tokens);
}

fn usage_since(after: Usage, before: Usage) -> Usage {
    Usage {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        cache_read_tokens: after
            .cache_read_tokens
            .saturating_sub(before.cache_read_tokens),
        cache_write_tokens: after
            .cache_write_tokens
            .saturating_sub(before.cache_write_tokens),
        cache_write_1h_tokens: after
            .cache_write_1h_tokens
            .saturating_sub(before.cache_write_1h_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        reasoning_tokens: after
            .reasoning_tokens
            .saturating_sub(before.reasoning_tokens),
        total_tokens: after.total_tokens.saturating_sub(before.total_tokens),
    }
}

#[derive(Default)]
struct CostAccumulator {
    microdollars: u64,
    picodollars_remainder: u32,
    unpriced_operations: u64,
}

impl CostAccumulator {
    /// Aggregate a request after its usage record durably updates the session.
    /// Missing prices remain unpriced; the numeric amount is a known subtotal.
    fn add(&mut self, cost: Option<Cost>) {
        let Some(cost) = cost else {
            self.unpriced_operations = self.unpriced_operations.saturating_add(1);
            return;
        };
        let remainder = u64::from(self.picodollars_remainder)
            .saturating_add(u64::from(cost.total_picodollars_remainder));
        let carry = remainder / u64::from(PICODOLLARS_PER_MICRODOLLAR);
        self.microdollars = self
            .microdollars
            .saturating_add(cost.total)
            .saturating_add(carry);
        self.picodollars_remainder = (remainder % u64::from(PICODOLLARS_PER_MICRODOLLAR)) as u32;
    }
}

fn active_branch_entries(session: &Session) -> Vec<&crate::session::Entry> {
    let mut reverse = Vec::new();
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        cursor = entry.parent.clone();
        reverse.push(entry);
    }
    reverse.reverse();
    reverse
}

fn resolve_tool_delivery_after_persistence(
    result: &Result<ToolOutput, ToolError>,
    text_limit: usize,
) {
    if let Ok(output) = result {
        output.resolve_delivery(output.text.len() <= text_limit);
    }
}

fn cancelled_tool_error() -> ToolError {
    ToolError::new(
        "tool execution cancelled by user; state may be partially changed and must not be replayed automatically",
    )
}

fn pending_tool_state(session: &Session) -> Option<(Vec<ToolCall>, HashSet<octet_ai::ToolCallId>)> {
    let mut persisted = HashSet::new();
    let mut calls = Vec::new();
    let mut latest_assistant = true;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id)?;
        match &entry.value {
            EntryValue::Message(Message::Assistant(assistant)) => {
                calls.extend(assistant.content.iter().filter_map(|part| match part {
                    AssistantPart::ToolCall(call) if latest_assistant || call.async_execution => {
                        Some(call.clone())
                    }
                    _ => None,
                }));
                latest_assistant = false;
            }
            EntryValue::Message(Message::User(user)) => {
                for part in &user.content {
                    if let UserPart::ToolResult(result) = part {
                        persisted.insert(result.tool_call_id.clone());
                    }
                }
            }
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    (!calls.is_empty()).then_some((calls, persisted))
}

fn tool_call_arguments_fingerprint(name: &str, args: &serde_json::Value) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    if serde_json::to_writer(&mut bytes, args).is_err() {
        bytes.extend_from_slice(b"<invalid-json>");
    }
    content_hash(&bytes)
}

fn repeated_tool_annotation(repeated_recently: usize) -> String {
    format!(
        "\n[agent diagnostic: exact call repeated {}x recently; if no progress, change approach or verify state.]",
        repeated_recently.saturating_add(1)
    )
}

fn annotate_repeated_tool_result(
    result: Result<ToolOutput, ToolError>,
    repeated_recently: usize,
) -> Result<ToolOutput, ToolError> {
    if repeated_recently < REPEATED_TOOL_CALL_THRESHOLD {
        return result;
    }
    let annotation = repeated_tool_annotation(repeated_recently);
    match result {
        Ok(output) if serde_json::from_str::<serde_json::Value>(&output.text).is_ok() => {
            // Keep machine-readable tool contracts valid. A trailing hint
            // would turn an otherwise valid JSON result into invalid JSON;
            // the next model turn can still be diagnosed from telemetry.
            Ok(output)
        }
        Ok(output) => Ok(output.with_model_annotation(&annotation)),
        Err(error) if serde_json::from_str::<serde_json::Value>(&error.message).is_ok() => {
            Err(error)
        }
        Err(error) => {
            let message = format!("{}{}", error.message, annotation);
            match error.policy_denial_code() {
                Some(code) => Err(ToolError::policy_denied(code, message)),
                None => Err(ToolError::new(message)),
            }
        }
    }
}

fn assistant_has_terminal_content(assistant: &AssistantMessage) -> bool {
    assistant.content.iter().any(|part| match part {
        AssistantPart::Text(text) => !text.trim().is_empty(),
        AssistantPart::ToolCall(_) | AssistantPart::Media(_) => true,
        AssistantPart::Reasoning(_) | AssistantPart::ProviderMetadata(_) => false,
    })
}

/// Content-free evidence for a normally ended turn with no terminal content.
/// Only allowlisted diagnostic codes are read, never their messages or IDs.
fn incomplete_terminal_response_reason(
    assistant: &AssistantMessage,
    stop_reason: &StopReason,
    usage: &Usage,
    diagnostics: &[octet_ai::Diagnostic],
    request_max_output_tokens: u64,
) -> String {
    let base = if assistant
        .content
        .iter()
        .any(|part| matches!(part, AssistantPart::Reasoning(_)))
    {
        "provider returned reasoning but no answer text"
    } else {
        "provider returned no user-visible content"
    };
    let mut chat_stop_defaulted = false;
    let mut usage_missing = false;
    for diagnostic in diagnostics {
        match diagnostic.code.as_str() {
            "chat_defaulted_stop_reason" => chat_stop_defaulted = true,
            "chat_usage_missing" => usage_missing = true,
            _ => {}
        }
    }
    let stop = match stop_reason {
        StopReason::Other(_) => "other",
        reason => reason.as_canonical(),
    };
    let usage = if usage_missing {
        "usage=not_reported".to_owned()
    } else {
        format!(
            "usage=canonical; output_tokens={}; reasoning_tokens={}",
            usage.output_tokens, usage.reasoning_tokens
        )
    };
    format!(
        "{base} (stop={stop}; chat_stop_defaulted={chat_stop_defaulted}; {usage}; request_max_output_tokens={request_max_output_tokens}; not automatically retried)"
    )
}

fn truncate_tool_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    if limit <= TOOL_TRUNCATION_MARKER.len() {
        return TOOL_TRUNCATION_MARKER[..limit].to_owned();
    }
    let available = limit - TOOL_TRUNCATION_MARKER.len();
    let head = available / 2;
    let tail = available - head;
    let mut result = String::with_capacity(limit);
    let mut head_end = head.min(text.len());
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    result.push_str(&text[..head_end]);
    result.push_str(TOOL_TRUNCATION_MARKER);
    let mut tail_start = text.len().saturating_sub(tail);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    result.push_str(&text[tail_start..]);
    result
}

fn truncate_ordered_tool_text(
    content_parts: &[ToolOutputContentPart],
    limit: usize,
) -> Vec<Option<String>> {
    let mut lowered = vec![None; content_parts.len()];
    let text_indices = content_parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| matches!(part, ToolOutputContentPart::Text(_)).then_some(index))
        .collect::<Vec<_>>();
    let Some(&first_text_index) = text_indices.first() else {
        return lowered;
    };
    let total_text_bytes = text_indices.iter().fold(0usize, |total, &index| {
        let ToolOutputContentPart::Text(text) = &content_parts[index] else {
            unreachable!("text_indices contains only text parts");
        };
        total.saturating_add(text.len())
    });
    if total_text_bytes <= limit {
        for index in text_indices {
            let ToolOutputContentPart::Text(text) = &content_parts[index] else {
                unreachable!("text_indices contains only text parts");
            };
            lowered[index] = Some(text.clone());
        }
        return lowered;
    }
    if limit == 0 {
        lowered[first_text_index] = Some(String::new());
        return lowered;
    }
    if limit <= TOOL_TRUNCATION_MARKER.len() {
        lowered[first_text_index] = Some(TOOL_TRUNCATION_MARKER[..limit].to_owned());
        return lowered;
    }

    let available = limit - TOOL_TRUNCATION_MARKER.len();
    let mut head_remaining = available / 2;
    let mut tail_remaining = available - head_remaining;
    let mut prefixes = vec![String::new(); content_parts.len()];
    let mut suffixes = vec![String::new(); content_parts.len()];

    for &index in &text_indices {
        if head_remaining == 0 {
            break;
        }
        let ToolOutputContentPart::Text(text) = &content_parts[index] else {
            unreachable!("text_indices contains only text parts");
        };
        let mut end = head_remaining.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        prefixes[index].push_str(&text[..end]);
        head_remaining -= end;
    }
    for &index in text_indices.iter().rev() {
        if tail_remaining == 0 {
            break;
        }
        let ToolOutputContentPart::Text(text) = &content_parts[index] else {
            unreachable!("text_indices contains only text parts");
        };
        let mut start = text.len().saturating_sub(tail_remaining);
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        suffixes[index].push_str(&text[start..]);
        tail_remaining -= text.len() - start;
    }

    let marker_index = text_indices
        .iter()
        .rev()
        .copied()
        .find(|&index| !prefixes[index].is_empty())
        .unwrap_or(first_text_index);
    for index in text_indices {
        let mut text = std::mem::take(&mut prefixes[index]);
        if index == marker_index {
            text.push_str(TOOL_TRUNCATION_MARKER);
        }
        text.push_str(&suffixes[index]);
        if !text.is_empty() {
            lowered[index] = Some(text);
        }
    }
    lowered
}

fn lower_tool_media_part(
    media: &Media,
    model: &Model,
    inline_media: bool,
    result_parts: &mut Vec<ToolResultPart>,
    adjacent_media: &mut Vec<Media>,
    accepted_kinds: &mut Vec<ToolOutputMediaKind>,
    omissions: &mut Vec<String>,
) {
    match media {
        Media::Image(_) => {
            if !model
                .spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Image)
            {
                omissions
                    .push("[image omitted: the active model does not accept image input]".into());
            } else {
                accepted_kinds.push(ToolOutputMediaKind::Image);
                if inline_media {
                    result_parts.push(ToolResultPart::Media(media.clone()));
                } else {
                    adjacent_media.push(media.clone());
                }
            }
        }
        Media::Audio(audio) => {
            if !model
                .spec
                .capabilities
                .input_modalities
                .contains(octet_ai::Modality::Audio)
            {
                omissions
                    .push("[audio omitted: the active model does not accept audio input]".into());
            } else if model.spec.protocol != Protocol::OpenAiChat {
                omissions
                    .push("[audio omitted: this protocol cannot replay audio tool output]".into());
            } else if !matches!(
                audio.format,
                octet_ai::AudioFormat::Wav | octet_ai::AudioFormat::Mp3
            ) {
                omissions.push(format!(
                    "[audio omitted: OpenAI Chat accepts WAV or MP3 input, got {:?}]",
                    audio.format
                ));
            } else {
                accepted_kinds.push(ToolOutputMediaKind::Audio);
                adjacent_media.push(media.clone());
            }
        }
    }
}

fn lower_tool_result(
    call_id: octet_ai::ToolCallId,
    result: &Result<ToolOutput, ToolError>,
    model: &Model,
    text_limit: usize,
    added_tool_names: Vec<String>,
) -> (
    UserMessage,
    Vec<ToolOutputMediaKind>,
    String,
    bool,
    Option<ToolOutputDetails>,
) {
    let (raw_text, is_error) = match result {
        Ok(output) => (output.text.as_str(), output.is_error()),
        Err(error) => (error.message.as_str(), true),
    };
    let persisted_text = truncate_tool_text(raw_text, text_limit);
    let mut result_parts = Vec::new();
    let mut adjacent_media = Vec::new();
    let mut accepted_kinds = Vec::new();
    let mut omissions = Vec::new();

    match result {
        Err(_) => result_parts.push(ToolResultPart::Text(persisted_text.clone())),
        Ok(output)
            if matches!(
                model.spec.protocol,
                Protocol::OpenAiResponses | Protocol::AnthropicMessages
            ) =>
        {
            let bounded_text = truncate_ordered_tool_text(output.content_parts(), text_limit);
            for (index, part) in output.content_parts().iter().enumerate() {
                match part {
                    ToolOutputContentPart::Text(_) => {
                        if let Some(text) = bounded_text[index].as_ref() {
                            result_parts.push(ToolResultPart::Text(text.clone()));
                        }
                    }
                    ToolOutputContentPart::Media(media) => lower_tool_media_part(
                        media,
                        model,
                        true,
                        &mut result_parts,
                        &mut adjacent_media,
                        &mut accepted_kinds,
                        &mut omissions,
                    ),
                }
            }
        }
        Ok(output) => {
            result_parts.push(ToolResultPart::Text(persisted_text.clone()));
            for media in output.media() {
                lower_tool_media_part(
                    media,
                    model,
                    false,
                    &mut result_parts,
                    &mut adjacent_media,
                    &mut accepted_kinds,
                    &mut omissions,
                );
            }
        }
    }
    result_parts.extend(omissions.iter().cloned().map(ToolResultPart::Text));
    let effective_is_error = is_error
        || result
            .as_ref()
            .is_ok_and(|output| !output.media().is_empty() && accepted_kinds.is_empty());
    let presented_text = if omissions.is_empty() {
        persisted_text.clone()
    } else if persisted_text.is_empty() {
        omissions.join("\n")
    } else {
        format!("{persisted_text}\n{}", omissions.join("\n"))
    };

    let mut content = Vec::with_capacity(1 + adjacent_media.len());
    content.push(UserPart::ToolResult(ToolResult {
        tool_call_id: call_id,
        content: result_parts,
        is_error: effective_is_error,
        added_tool_names: (!added_tool_names.is_empty()).then_some(added_tool_names),
    }));
    content.extend(adjacent_media.into_iter().map(UserPart::Media));
    (
        UserMessage { content },
        accepted_kinds,
        presented_text,
        effective_is_error,
        result.as_ref().ok().and_then(ToolOutput::details).cloned(),
    )
}

/// The lowerer emits exactly one paired result followed by protocol-adjacent
/// media. Use that authoritative message, not the tool's unaccepted raw output.
fn lowered_tool_result_media(message: &UserMessage) -> impl Iterator<Item = &Media> {
    message.content.iter().flat_map(|part| {
        let (nested, adjacent) = match part {
            UserPart::ToolResult(result) => (result.content.as_slice(), None),
            UserPart::Media(media) => (&[][..], Some(media)),
            UserPart::Text(_) => (&[][..], None),
        };
        nested
            .iter()
            .filter_map(|part| match part {
                ToolResultPart::Media(media) => Some(media),
                ToolResultPart::Text(_) => None,
            })
            .chain(adjacent)
    })
}

fn persist_pending_cancellations(session: &mut Session) -> Result<(), AgentError> {
    let Some((calls, persisted)) = pending_tool_state(session) else {
        return Ok(());
    };
    let unresolved = calls
        .into_iter()
        .filter(|call| !persisted.contains(&call.id));
    for call in unresolved {
        let text = match call.argument_error {
            Some(argument_error) => rejected_argument_tool_error(argument_error).message,
            None => cancelled_tool_error().message,
        };
        session.append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: call.id,
                content: vec![ToolResultPart::Text(text)],
                is_error: true,
                added_tool_names: None,
            })],
        })))?;
    }
    Ok(())
}

fn close_failed_turn(session: &mut Session, model: &Model) -> Result<(), AgentError> {
    let ends_with_user = {
        let context = session.context_ref()?;
        matches!(context.last(), Some(Message::User(_)))
    };
    if ends_with_user {
        session.append_with_metadata(
            EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text(FAILED_TURN_CONTEXT_MARKER.to_owned())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            })),
            Some(EntryMetadata {
                native_steering: None,
                local_synthetic_assistant: true,
                ..EntryMetadata::default()
            }),
        )?;
    }
    Ok(())
}

fn retryable_before_generation(error: &AiError) -> bool {
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

fn is_replayable_network_failure(error: &AiError) -> bool {
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

fn looks_like_context_error(error: &AiError) -> bool {
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

fn provider_requests_connection_refresh(error: &octet_ai::ProviderError) -> bool {
    let Some(code) = error.code.as_deref() else {
        return false;
    };
    let code = code.to_ascii_lowercase();
    code == "websocket_connection_limit_reached"
        || (code.contains("websocket") && code.contains("connection") && code.contains("limit"))
}

fn permanent_provider_error(error: &octet_ai::ProviderError) -> bool {
    error.is_permanent()
}

fn retryable_provider_error(error: &octet_ai::ProviderError) -> bool {
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

fn retryable_stream_start(error: &AiError) -> bool {
    if let AiError::StreamFailure { inner, .. } = error {
        return retryable_stream_start(inner);
    }
    // No visible output is not proof of nonacceptance. Only a safe connection
    // failure or an explicit provider rejection can authorize another request.
    retryable_before_generation(error)
        || matches!(error, AiError::Provider(provider) | AiError::ResponsesFailed(provider) if retryable_provider_error(provider))
}

fn provider_retry_limit(error: &AiError) -> usize {
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
fn qualified_inference_replacement(model: &Model, request: &Request) -> bool {
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

fn interrupted_inference_error(error: &AiError) -> bool {
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
const MAX_INFERENCE_REPLACEMENTS: usize = 11;
// Five physical HTTP sends for each of six outer HTTP attempts. These are
// independent of streamed-generation replacements, with a shared finite cap
// covering six WS attempts plus thirty HTTP attempts at the Codex defaults.
const MAX_OPENING_ADMISSION_REPLACEMENTS: usize = 29;
const MAX_CUMULATIVE_PROVIDER_REPLACEMENTS: usize = 35;

fn opening_transport_failure(error: &AiError) -> bool {
    match error {
        AiError::StreamFailure { inner, .. } => opening_transport_failure(inner),
        AiError::Transport(error) => matches!(
            error.phase,
            octet_ai::TransportPhase::Connect | octet_ai::TransportPhase::ResponseHeaders
        ),
        _ => false,
    }
}

fn http_server_failure(error: &AiError) -> bool {
    match error {
        AiError::StreamFailure { inner, .. } => http_server_failure(inner),
        AiError::Http(error) => error.is_transient_server_error(),
        _ => false,
    }
}

fn http_usage_unknown(error: &AiError) -> bool {
    match error {
        AiError::StreamFailure { inner, .. } => http_usage_unknown(inner),
        AiError::Http(error) => error.status.is_server_error() || error.status.as_u16() == 408,
        _ => false,
    }
}

#[derive(Default)]
struct ProviderRecoveryBudget {
    admission: usize,
    stream: usize,
}

impl ProviderRecoveryBudget {
    fn limit(&self, total: usize, recovery: &PendingProviderRecovery) -> usize {
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

    fn admit(&mut self, recovery: &PendingProviderRecovery) {
        if recovery.opening_admission() {
            self.admission += 1;
        } else {
            self.stream += 1;
        }
    }
}

struct PendingProviderRecovery {
    error: AiError,
    qualified: bool,
    saw_generation: bool,
    opened: bool,
}

impl PendingProviderRecovery {
    fn waiting_for_network(&self) -> bool {
        self.qualified
            && !self.opened
            && !self.saw_generation
            && matches!(
                &self.error,
                AiError::Auth(octet_ai::AuthError::Unavailable) | AiError::NetworkUnavailable(_)
            )
    }

    fn opening_admission(&self) -> bool {
        self.qualified
            && !self.saw_generation
            && (opening_transport_failure(&self.error) || http_server_failure(&self.error))
    }

    fn replacement_limit(&self) -> usize {
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

    fn usage_unknown(&self) -> bool {
        self.saw_generation
            // A replay-authorized gateway 5xx is not proof of zero billing.
            || http_usage_unknown(&self.error)
            || (self.opened && !retryable_before_generation(&self.error))
            || interrupted_inference_error(&self.error)
    }
}

async fn wait_network_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn network_wait_delay(run_id: &str, attempt: usize) -> Duration {
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

fn retry_after(error: &AiError, attempt: usize) -> Duration {
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

fn provider_rate_limit_delay(message: &str) -> Option<Duration> {
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

type AuxiliaryDispatch = Arc<Mutex<Option<AiClient>>>;

struct AuxiliaryRecovery<'a> {
    dispatch: AuxiliaryDispatch,
    session: &'a mut Session,
    run_id: &'a str,
    resource_owner: &'a str,
    retry_hooks: &'a [Arc<dyn ProviderRetryHook>],
    max_network_wait: Option<Duration>,
    model: &'a Model,
    qualified: bool,
    enabled: bool,
    hard_budget: bool,
    abort: &'a AbortFlag,
    events: &'a mpsc::UnboundedSender<AgentEvent>,
    operation: crate::events::ProviderOperation,
    session_id: &'a str,
}

impl AuxiliaryRecovery<'_> {
    fn disarm(&mut self) {
        self.dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    fn record_uncertainty(&mut self) -> Result<(), AgentError> {
        let first = !self.session.has_uncertain_usage();
        let recorded = self.session.record_usage_uncertainty(
            self.model.endpoint.id.clone(),
            self.model.spec.id.clone(),
            match self.operation {
                crate::events::ProviderOperation::LocalCompaction => "local_compaction",
                crate::events::ProviderOperation::BranchSummary => "branch_summary",
                crate::events::ProviderOperation::NativeCompaction => "native_compaction",
                crate::events::ProviderOperation::TerminalGate => "terminal_gate",
            },
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
async fn recover_auxiliary<T, F, Fut, S>(
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
            || (context.hard_budget && usage_unknown)
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
async fn auxiliary_complete(
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

async fn auxiliary_compact(
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
fn incomplete_responses_error(model: &Model, response: &octet_ai::Response) -> Option<AiError> {
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

struct ProviderRetryDecision {
    proceed: bool,
    additional_delay: Duration,
}

struct ProviderRetryRequest<'a> {
    hooks: &'a [Arc<dyn ProviderRetryHook>],
    context: ProviderRetryContext,
    abort: &'a AbortFlag,
}

async fn provider_retry_decision(request: ProviderRetryRequest<'_>) -> ProviderRetryDecision {
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

fn assistant_persistence_context(
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

async fn collect_persistence_metadata(
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

fn provider_retry_diagnostic(model: &Model, error: &AiError) -> String {
    let diagnostic = public_ai_error_diagnostic(error, &model.endpoint.id.0, &model.spec.id.0);
    if is_replayable_network_failure(error) {
        format!("Network connection lost. Are you connected to the internet? {diagnostic}")
    } else {
        diagnostic
    }
}

fn provider_failure(error: AiError, retries: usize) -> AgentError {
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
async fn execute_recovery_call(
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
            let progress_sink = ToolProgressSink::live(progress_tx);
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
            let (output, is_error) = match &result {
                Ok(output) => (output.text.as_str(), output.is_error()),
                Err(error) => (error.message.as_str(), true),
            };
            for hook in hooks {
                hook.after_tool_call(&call.name, &hook_arguments, output, is_error, &context)
                    .await;
            }
            result
        }
    };
    Ok(result)
}

fn model_visible_branch_entries(session: &Session) -> Vec<&crate::session::Entry> {
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

fn previous_message_is_user(session: &Session, entry: &crate::session::Entry) -> bool {
    let mut cursor = entry.parent.clone();
    while let Some(id) = cursor {
        let Some(previous) = session.entry(&id) else {
            return false;
        };
        match &previous.value {
            EntryValue::Message(Message::User(user)) => return !user.content.is_empty(),
            EntryValue::Message(Message::Assistant(_)) => return false,
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

fn turn_starts(session: &Session) -> Vec<EntryId> {
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
struct CountingWriter(u64);

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const ESTIMATED_IMAGE_TOKENS: u64 = 1_600;
const ESTIMATED_AUDIO_TOKENS: u64 = 8_000;

fn inline_media_payload_bytes(media: &Media) -> u64 {
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

fn media_tokens(media: &Media) -> u64 {
    match media {
        Media::Image(_) => ESTIMATED_IMAGE_TOKENS,
        Media::Audio(_) => ESTIMATED_AUDIO_TOKENS,
    }
}

fn request_media_adjustment(messages: &[Message]) -> (u64, u64) {
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

fn responses_replay_media_adjustment(replay: &[ResponsesReplayItem]) -> (u64, u64) {
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

fn tool_schema_bytes(tools: &[ToolDef]) -> usize {
    let mut bytes = CountingWriter::default();
    // ToolDef is internally constructed from serializable strings and JSON
    // values, so serialization failure would violate the provider-request
    // invariant rather than being a recoverable user boundary.
    serde_json::to_writer(&mut bytes, tools).expect("ToolDef serializes");
    usize::try_from(bytes.0).unwrap_or(usize::MAX)
}

fn require_tool_schema_budget(tools: &[ToolDef], max_bytes: usize) -> Result<(), AgentError> {
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

fn validate_compaction_summary_part(summary: &str) -> Result<(), AgentError> {
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

fn append_compaction_turn_prefix(
    summary: &mut String,
    prefix_summary: &str,
) -> Result<(), AgentError> {
    validate_compaction_summary_part(prefix_summary)?;
    summary.push_str("\n\n---\n\n**Turn Context (split turn):**\n\n");
    summary.push_str(prefix_summary);
    Ok(())
}

fn finish_validated_compaction_handoff(
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

fn estimate_request_tokens(system: &str, messages: &[Message], tools: &[ToolDef]) -> u64 {
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

struct ExactResponsesReplay {
    input: ResponsesInput,
    replay: Arc<Vec<ResponsesReplayItem>>,
    instructions: Option<String>,
}

fn exact_responses_replay(
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

fn current_head_is_native_checkpoint(session: &Session, model: &Model) -> bool {
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

fn validate_native_compact_output(output: &octet_ai::ResponsesOutput) -> Result<(), AgentError> {
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

fn validate_reasoning_update(model: &Model, reasoning: &ReasoningConfig) -> Result<(), AgentError> {
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

fn require_ultra_observation(
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

fn persist_reasoning_selection(
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

fn durable_responses_options(
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

fn request_reasoning_for_replay(
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
fn resolve_service_tier(
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

fn native_responses_options(
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

fn estimate_responses_request_tokens(
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

fn estimate_compact_request_tokens(
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

fn estimate_messages_tokens(messages: &[Message]) -> u64 {
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

fn usage_context_tokens(usage: &Usage) -> u64 {
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

#[cfg(test)]
thread_local! {
    static PROVIDER_CONTEXT_ENTRY_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PROVIDER_CONTEXT_USAGE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Provider usage is the best available tokenizer measurement of the prefix
/// through its assistant response. Add structural estimates only for messages
/// persisted after that response. Usage from before the latest compaction or
/// from a different route/model is stale and must not retrigger compaction.
fn provider_context_estimate(session: &Session, model: &Model) -> Option<u64> {
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
            EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. } => break,
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

fn reconcile_context_estimate(
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

fn serialized_tokens<T: Serialize>(value: &T) -> u64 {
    let mut bytes = CountingWriter::default();
    if serde_json::to_writer(&mut bytes, value).is_err() {
        return 0;
    }
    bytes.0.div_ceil(4)
}

fn visible_compaction_summary(session: &Session) -> Option<String> {
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

fn context_breakdown(
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

fn observe_context_tracker(
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

/// Which queue a batch of control inputs came from; only the announced event
/// differs between steering and follow-up delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlDeliveryKind {
    Steering,
    FollowUp,
}

impl ControlDeliveryKind {
    fn delivered_event(self, messages: Vec<String>) -> AgentEvent {
        match self {
            Self::Steering => AgentEvent::SteeringDelivered { messages },
            Self::FollowUp => AgentEvent::FollowUpDelivered { messages },
        }
    }
}

/// Outcome of appending one queued control-input batch (steering or
/// follow-up) to the session.
enum ControlDelivery {
    /// Every input was appended; announce them with this event when present.
    Completed { event: Option<AgentEvent> },
    /// Persistence failed. Any prefix that did reach the session is announced
    /// by `event`; the run must then end with `finish`.
    Interrupted {
        event: Option<AgentEvent>,
        finish: FinishReason,
    },
}

/// Snapshot of everything `observe_context_tracker` needs to re-observe the
/// tracker after control inputs change the session.
struct ContextObservation<'a> {
    tracker: &'a ContextTracker,
    model: &'a Model,
    system: &'a str,
    tools: &'a [ToolDef],
}

impl ContextObservation<'_> {
    fn observe(&self, session: &Session) -> Result<ContextBreakdown, SessionError> {
        observe_context_tracker(self.tracker, session, self.model, self.system, self.tools)
    }
}

async fn next_delegation_snapshot(
    receiver: &mut watch::Receiver<Option<DelegationTelemetrySnapshot>>,
) -> Option<DelegationTelemetrySnapshot> {
    receiver.changed().await.ok()?;
    receiver.borrow_and_update().clone()
}

/// Bounded pacing for the live panel's *replaceable* publications (row 4.8).
///
/// The panel feed is `ToolProgress`: append-only `Output`/`Status` chunks are
/// load-bearing (dropping one breaks the `complete_<stream>=true` contract) and
/// stay verbatim, while a [`ToolProgressDecoration`] replaces the previous
/// annotation, so an intermediate one carries nothing the latest does not.
/// This is the run-path consumer of [`AdaptivePreviewCoalescer`]:
///
/// * the first decoration of a call is published immediately,
/// * later ones are paced to the coalescer's interval/rate policy and collapsed
///   to the latest state,
/// * [`LivePreviewPacer::settle`] forces the held state at the call's terminal
///   boundary, so a finished call can never leave the panel on stale state.
struct LivePreviewPacer {
    coalescer: AdaptivePreviewCoalescer,
    pending: Option<ToolProgressDecoration>,
    /// Publications observed; read by [`Self::stats`] in tests only.
    #[cfg_attr(not(test), allow(dead_code))]
    published: u64,
    /// Intermediate states collapsed away; read by [`Self::stats`] in tests.
    #[cfg_attr(not(test), allow(dead_code))]
    coalesced: u64,
}

impl LivePreviewPacer {
    fn new() -> Self {
        Self {
            coalescer: AdaptivePreviewCoalescer::new(),
            pending: None,
            published: 0,
            coalesced: 0,
        }
    }

    /// Routes one replaceable update, returning the publication to forward now.
    fn observe(
        &mut self,
        decoration: ToolProgressDecoration,
        now: std::time::Instant,
    ) -> Option<ToolProgressDecoration> {
        let encoded_bytes = decoration.label().len() + decoration.detail().map_or(0, str::len);
        match self.coalescer.record(encoded_bytes, now) {
            PreviewPublication::Immediate => {
                self.pending = None;
                self.published = self.published.saturating_add(1);
                Some(decoration)
            }
            PreviewPublication::Scheduled(_) => {
                if self.pending.replace(decoration).is_some() {
                    self.coalesced = self.coalesced.saturating_add(1);
                }
                None
            }
        }
    }

    /// The instant at which held state becomes publishable, if any is held.
    fn flush_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.pending.as_ref()?;
        self.coalescer
            .deadline_in(now)
            .map(|remaining| now + remaining)
    }

    /// Publishes held state if its pace deadline has passed.
    fn take_due(&mut self, now: std::time::Instant) -> Option<ToolProgressDecoration> {
        self.coalescer.take_due(now)?;
        self.published = self.published.saturating_add(1);
        self.pending.take()
    }

    /// Publishes held state unconditionally (completion, error, cancellation).
    fn settle(&mut self, now: std::time::Instant) -> Option<ToolProgressDecoration> {
        let pending = self.pending.take()?;
        self.coalescer.force(now, 0);
        self.published = self.published.saturating_add(1);
        Some(pending)
    }

    /// Decorations published and intermediate states collapsed away.
    ///
    /// Test-only observability: the live path forwards the surviving
    /// decoration itself, so production builds never read the counters.
    #[cfg(test)]
    fn stats(&self) -> (u64, u64) {
        (self.published, self.coalesced)
    }
}

/// Routes one accepted progress item to the live panel.
///
/// Replaceable decorations go through `pacer`; every append-only flavor is
/// forwarded unchanged.
fn forward_tool_progress(
    progress: ToolProgress,
    pacer: &mut LivePreviewPacer,
    now: std::time::Instant,
) -> Option<ToolProgress> {
    match progress {
        ToolProgress::Decoration(decoration) => {
            pacer.observe(decoration, now).map(ToolProgress::Decoration)
        }
        verbatim => Some(verbatim),
    }
}

/// Opt-in durable partial-output checkpointing for one tool's live calls (row
/// 4.7).
///
/// The host names the tool, supplies the durable replacement sink, and chooses
/// the cadence; the run path owns publishing. A name that matches no registered
/// tool costs checkpoints and nothing else — it can never change a result.
#[cfg(any(unix, windows))]
#[derive(Clone)]
struct PartialOutputCheckpointConfig {
    tool: String,
    // None binds the current session invocation at dispatch, never a global
    // sink shared by different provider calls.
    sink: Option<Arc<dyn PartialOutputCheckpointSink>>,
    interval: Duration,
    totals: Arc<PartialOutputCheckpointTotals>,
}

/// Observable counters for the run path's checkpoint publications.
///
/// `published` is the number of snapshots the sink accepted, `paced` the
/// observations the publisher collapsed away (interval or duplicate), and
/// `failures` the storage faults. A failure is bookkeeping only: it never
/// changes a tool result, and it never becomes durable state.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartialOutputCheckpointStats {
    /// Bounded snapshots handed to the sink.
    pub published: u64,
    /// Observations suppressed by the interval or by duplicate suppression.
    pub paced: u64,
    /// Sink refusals (storage faults).
    pub failures: u64,
}

#[cfg(any(unix, windows))]
#[derive(Default)]
struct PartialOutputCheckpointTotals {
    published: AtomicU64,
    paced: AtomicU64,
    failures: AtomicU64,
}

#[cfg(any(unix, windows))]
impl PartialOutputCheckpointTotals {
    fn stats(&self) -> PartialOutputCheckpointStats {
        PartialOutputCheckpointStats {
            published: self.published.load(Ordering::Relaxed),
            paced: self.paced.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

/// Run-path consumer of [`BashCheckpointPublisher`] for one live invocation.
///
/// Pi's durability contract makes the *harness* replace
/// `pendingToolOutput(operationId, invocationId)` while a call is live, and the
/// tool own the cadence and the "this update is a complete bounded snapshot"
/// claim. The cadence, the byte bound and duplicate suppression here are exactly
/// the publisher's; the durable replacement is the host's
/// [`PartialOutputCheckpointSink`]. The values live only as long as the call
/// does, so a settled invocation has nothing left to republish, and only
/// bounded state is retained: at most [`BASH_CHECKPOINT_MAX_BYTES`] per stream,
/// which is also the cap applied to the published snapshot.
#[cfg(any(unix, windows))]
struct LivePartialOutput {
    sink: Arc<dyn PartialOutputCheckpointSink>,
    publisher: BashCheckpointPublisher,
    streams: [PartialStream; 2],
    totals: Arc<PartialOutputCheckpointTotals>,
}

/// Bytes of one stream the run-path tracker retains.
///
/// Half of the published cap minus a header reserve, so the rendered snapshot
/// (both stream headers plus both retained tails) always fits
/// [`BASH_CHECKPOINT_MAX_BYTES`] with its `stdout: N bytes seen` header intact
/// even after the publisher's final bounding fence.
#[cfg(any(unix, windows))]
const PARTIAL_STREAM_CAP: usize = BASH_CHECKPOINT_MAX_BYTES / 2 - PARTIAL_HEADER_RESERVE;
/// Per-stream budget reserved for the snapshot header.
#[cfg(any(unix, windows))]
const PARTIAL_HEADER_RESERVE: usize = 512;

/// Bounded, newest-bytes-retaining accumulation of one stream.
#[cfg(any(unix, windows))]
#[derive(Default)]
struct PartialStream {
    seen: u64,
    tail: Vec<u8>,
    elided: bool,
}

#[cfg(any(unix, windows))]
impl PartialStream {
    /// Appends live bytes, keeping at most [`PARTIAL_STREAM_CAP`] of the newest
    /// output on a UTF-8 boundary.
    fn push(&mut self, bytes: &[u8]) {
        self.seen = self.seen.saturating_add(bytes.len() as u64);
        self.tail.extend_from_slice(bytes);
        if self.tail.len() <= PARTIAL_STREAM_CAP {
            return;
        }
        self.elided = true;
        let mut start = self.tail.len() - PARTIAL_STREAM_CAP;
        while start < self.tail.len() && !is_utf8_boundary(&self.tail, start) {
            start += 1;
        }
        self.tail.drain(..start);
    }

    fn render(&self, name: &str) -> String {
        if self.seen == 0 {
            return format!("{name}: 0 bytes seen");
        }
        let text = String::from_utf8_lossy(&self.tail);
        let text = text.trim_end_matches('\n');
        let elided = if self.elided {
            " (earlier bytes elided)"
        } else {
            ""
        };
        format!(
            "{name}: {} bytes seen{elided}, showing the newest {} bytes\n{}",
            self.seen,
            self.tail.len(),
            text
        )
    }
}

/// Whether `index` starts a UTF-8 code point.
#[cfg(any(unix, windows))]
fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index >= bytes.len() || (bytes[index] & 0xC0) != 0x80
}

/// Renders the complete replaceable snapshot for one invocation.
///
/// It shares the tool layer's `"{stream}: {n} bytes seen"` header so recovery
/// reads one shape from either mechanism, keeps the newest bytes of each stream
/// on a code-point boundary, and deliberately never emits
/// `complete_<stream>=true`: a checkpoint must not be readable as proof that the
/// command finished.
#[cfg(any(unix, windows))]
fn render_partial_output(streams: &[PartialStream; 2]) -> String {
    format!(
        "{}\n{}",
        streams[0].render("stdout"),
        streams[1].render("stderr")
    )
}

#[cfg(any(unix, windows))]
impl LivePartialOutput {
    /// Creates the tracker for `call` when the host's opt-in names that tool.
    fn for_call(config: &PartialOutputCheckpointConfig, call: &str) -> Option<Self> {
        let sink = config.sink.as_ref()?;
        (config.tool == call).then(|| Self {
            sink: Arc::clone(sink),
            publisher: BashCheckpointPublisher::new(config.interval),
            streams: [PartialStream::default(), PartialStream::default()],
            totals: Arc::clone(&config.totals),
        })
    }

    /// Records one drained progress item, publishing only replaceable,
    /// bounded, complete output snapshots.
    fn observe_progress(&mut self, progress: &ToolProgress, now: std::time::Instant) {
        if let ToolProgress::Output { stream, bytes } = progress {
            self.observe_output(*stream, bytes, now);
        }
    }

    /// Records live output and publishes the snapshot the publisher admits.
    ///
    /// Returns the published snapshot for observability/tests. A sink refusal is
    /// counted and dropped: the command's own result is unaffected.
    fn observe_output(
        &mut self,
        stream: OutputStream,
        bytes: &[u8],
        now: std::time::Instant,
    ) -> Option<String> {
        let slot = match stream {
            OutputStream::Stdout => &mut self.streams[0],
            OutputStream::Stderr => &mut self.streams[1],
        };
        slot.push(bytes);
        if !self.publisher.is_due(now) {
            self.publisher.note_before_interval();
            self.totals.paced.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let snapshot =
            BashCheckpointPublisher::bound_snapshot(&render_partial_output(&self.streams));
        let Some(published) = self.publisher.observe(&snapshot, now) else {
            self.totals.paced.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        match self.sink.checkpoint_partial_output(&published) {
            Ok(()) => {
                self.totals.published.fetch_add(1, Ordering::Relaxed);
                Some(published)
            }
            Err(_) => {
                self.publisher.note_failure();
                self.totals.failures.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}

/// Result of applying one drained tool-progress item to the run.
enum ProgressSettlement {
    /// Cancellation took precedence before the item was accepted; semantic
    /// state was discarded and the caller must stop accepting progress.
    Cancelled,
    /// Consumed internally as a durable session event (persisted, or its
    /// reply resolved with the persistence error).
    Settled,
    /// Pure progress; surface it to observers as a `ToolProgress` event.
    Emit(ToolProgress),
}

/// Apply one drained tool-progress item.
///
/// When `cancelled` won, any queued session event is rejected through its
/// reply channel without touching the session. Otherwise a session event is
/// appended durably and acknowledged; every other progress flavor is returned
/// for the caller to emit.
fn settle_tool_progress(
    p: ToolProgress,
    cancelled: bool,
    session: &mut Session,
) -> ProgressSettlement {
    if cancelled {
        // The biased select deliberately gives cancellation
        // precedence. Events already accepted in the loop
        // remain durable, but a queued semantic event must
        // not take effect after the tool was reported as
        // cancelled (notably, it must not activate a skill).
        if let ToolProgress::SessionEvent(_, reply_tx_mutex) = p {
            if let Ok(mut opt) = reply_tx_mutex.lock() {
                if let Some(reply_tx) = opt.take() {
                    let _ = reply_tx.send(Err(
                        "session event discarded because cancellation won".to_string()
                    ));
                }
            }
        }
        return ProgressSettlement::Cancelled;
    }
    if let ToolProgress::SessionEvent(event, reply_tx_mutex) = p {
        let res = session.append(*event);
        if let Ok(mut opt) = reply_tx_mutex.lock() {
            if let Some(reply_tx) = opt.take() {
                let _ = reply_tx.send(res.map_err(|e| e.to_string()));
            }
        }
        ProgressSettlement::Settled
    } else {
        ProgressSettlement::Emit(p)
    }
}

/// Append a batch of already-queued control inputs as durable user messages
/// and report what was delivered.
///
/// When enabled, bounded evidence is recorded for the terminal gate before
/// its append is attempted. Frontend delivery summaries remain complete.
/// On append failure the context tracker is still observed (its error ignored)
/// so observers see the partial delivery before the run ends.
async fn deliver_control_inputs(
    queued: Vec<ReservedInput>,
    kind: ControlDeliveryKind,
    session: &mut Session,
    metadata: &EntryMetadata,
    terminal_gate_evidence: &mut Option<TerminalGateEvidence>,
    observation: &ContextObservation<'_>,
    abort: Option<&AbortFlag>,
) -> ControlDelivery {
    let mut delivered = Vec::with_capacity(queued.len());
    for queued in queued {
        // Linearize recall against delivery BEFORE any evidence or durable
        // write. The payload and permits leave receipt ownership together;
        // recall cannot succeed after this claim, including during fsync.
        let Some(ReservedPayload { input, reservation }) = queued.claim() else {
            continue;
        };
        let input = match prepare_user_images(input, observation.model, abort).await {
            Ok(input) => input,
            Err(error) => {
                let event = (!delivered.is_empty())
                    .then(|| kind.delivered_event(std::mem::take(&mut delivered)));
                return ControlDelivery::Interrupted {
                    event,
                    finish: if matches!(error, AgentError::Cancelled) {
                        FinishReason::Aborted
                    } else {
                        FinishReason::Failed(error)
                    },
                };
            }
        };
        let summary = input.text_summary();
        if let Some(evidence) = terminal_gate_evidence {
            evidence.record_request(&summary);
        }
        if let Err(e) = session.append_with_metadata(user_message(input), Some(metadata.clone())) {
            let event = (!delivered.is_empty())
                .then(|| kind.delivered_event(std::mem::take(&mut delivered)));
            let _ = observation.observe(session);
            return ControlDelivery::Interrupted {
                event,
                finish: FinishReason::Failed(e.into()),
            };
        }
        delivered.push(summary);
        // Never free admission capacity merely because ingress was drained.
        // Both permits remain live through the successful durable append.
        drop(reservation);
    }
    if !delivered.is_empty() {
        if let Err(error) = observation.observe(session) {
            return ControlDelivery::Interrupted {
                event: Some(kind.delivered_event(delivered)),
                finish: FinishReason::Failed(error.into()),
            };
        }
        return ControlDelivery::Completed {
            event: Some(kind.delivered_event(delivered)),
        };
    }
    ControlDelivery::Completed { event: None }
}

fn worst_case_request_cost(
    model: &Model,
    input_tokens: u64,
    output_tokens: u64,
    service_tier: Option<ServiceTier>,
) -> Option<u64> {
    let pricing = model.spec.pricing.as_ref()?;
    let cache_write_1h_rate = match pricing.cache_write_1h {
        Some(rate) => rate.0,
        None => pricing.input.0.checked_mul(2)?,
    };
    let mut input_rate = pricing
        .input
        .0
        .max(pricing.cache_read.0)
        .max(pricing.cache_write_5m.0)
        .max(cache_write_1h_rate);
    let mut output_rate = pricing
        .output
        .0
        .max(pricing.reasoning.map(|rate| rate.0).unwrap_or_default());
    if model.spec.protocol == Protocol::AnthropicMessages {
        for fallback in model
            .spec
            .preset
            .anthropic_compat
            .as_ref()
            .into_iter()
            .flat_map(|compat| &compat.allowed_fallback_models)
        {
            // The server may select any declared fallback, including one more
            // expensive than the requested model. An unpriced target cannot be
            // admitted under a hard cost ceiling.
            let pricing = fallback.cost?.pricing()?;
            input_rate = input_rate
                .max(pricing.input.0)
                .max(pricing.cache_read.0)
                .max(pricing.cache_write_5m.0)
                .max(pricing.input.0.checked_mul(2)?);
            output_rate = output_rate.max(pricing.output.0);
        }
    }
    for tier in &pricing.tiers {
        // The implicit one-hour write price follows the active input tier,
        // not the base catalog input rate. Never reserve below that bucket.
        if pricing.cache_write_1h.is_none() {
            if let Some(rate) = tier.input {
                input_rate = input_rate.max(rate.0.checked_mul(2)?);
            }
        }
        for rate in [
            tier.input,
            tier.cache_read,
            tier.cache_write_5m,
            tier.cache_write_1h,
        ]
        .into_iter()
        .flatten()
        {
            input_rate = input_rate.max(rate.0);
        }
        for rate in [tier.output, tier.reasoning].into_iter().flatten() {
            output_rate = output_rate.max(rate.0);
        }
    }
    let conservative = octet_ai::Pricing {
        input: octet_ai::TokenRate(input_rate),
        output: octet_ai::TokenRate(output_rate),
        cache_read: octet_ai::TokenRate(input_rate),
        cache_write_5m: octet_ai::TokenRate(input_rate),
        cache_write_1h: Some(octet_ai::TokenRate(input_rate)),
        reasoning: Some(octet_ai::TokenRate(output_rate)),
        tiers: Vec::new(),
    };
    let usage = Usage {
        input_tokens,
        output_tokens,
        ..Usage::default()
    };
    let cost = octet_ai::responses_cost_of(
        &conservative,
        &usage,
        model.endpoint.runtime.responses_profile,
        &model.spec.api_name,
        service_tier,
        None,
    )
    .ok()??;
    cost.total
        .checked_add(u64::from(cost.total_picodollars_remainder > 0))
}

fn priced_session_subtotal(session: &Session, model: &Model) -> Option<u64> {
    (!session.has_unpriced_usage()
        && (session.total_cost_microdollars() > 0 || model.spec.pricing.is_some()))
    .then(|| session.total_cost_microdollars())
}

fn usage_total_tokens(usage: &Usage) -> u64 {
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

fn session_total_tokens_for_own_context(session: &Session) -> u64 {
    session
        .usage_records()
        .iter()
        .filter(|record| !matches!(&record.kind, UsageRecordKind::DelegatedAgent { .. }))
        .fold(0u64, |total, record| {
            total.saturating_add(usage_total_tokens(&record.usage))
        })
}

fn record_delegated_usage_once(
    session: &mut Session,
    mut delegated: DelegatedUsage,
) -> Result<(), SessionError> {
    use crate::delegation::{
        add_delegated_cost, add_delegated_usage, subtract_cost, subtract_usage,
    };

    // The root's committed ledger, not a process-local or fleet-file watermark,
    // is authoritative across failed appends, repeated snapshots and restarts.
    let mut mirrored_usage = Usage::default();
    let mut mirrored_cost = Cost::default();
    let mut mirrored_turns = 0;
    let mut mirrored_tools = 0;
    for record in session.usage_records() {
        if let UsageRecordKind::DelegatedAgent {
            agent_id,
            turn_count,
            tool_call_count,
        } = &record.kind
        {
            if *agent_id != delegated.agent_id {
                continue;
            }
            add_delegated_usage(&mut mirrored_usage, &record.usage);
            if let Some(cost) = record.cost {
                add_delegated_cost(&mut mirrored_cost, cost);
            }
            mirrored_turns = mirrored_turns.max(*turn_count);
            mirrored_tools = mirrored_tools.max(*tool_call_count);
        }
    }
    delegated.usage = subtract_usage(delegated.usage, mirrored_usage);
    delegated.cost = delegated
        .cost
        .map(|cost| subtract_cost(cost, mirrored_cost));
    if delegated.usage == Usage::default()
        && delegated.cost.unwrap_or_default() == Cost::default()
        && delegated.turn_count <= mirrored_turns
        && delegated.tool_call_count <= mirrored_tools
    {
        return Ok(());
    }
    session.record_delegated_agent_usage(delegated)
}

// Child records are cumulative snapshots. Root uncertainty is a sticky aggregate
// flag, not another physical failed attempt each time the same child is mirrored.
fn mirror_delegated_uncertainty(
    session: &mut Session,
    model: &Model,
    uncertain: bool,
) -> Result<bool, SessionError> {
    if !uncertain || session.has_uncertain_usage() {
        return Ok(false);
    }
    session.record_usage_uncertainty(
        model.endpoint.id.clone(),
        model.spec.id.clone(),
        "delegated_agent",
    )?;
    Ok(true)
}

fn require_enforceable_output_cap(
    session: &Session,
    output_cap: Option<u64>,
    token_limit: Option<u64>,
    cost_limit: Option<u64>,
) -> Result<(), AgentError> {
    if token_limit.is_none() && cost_limit.is_none() {
        return Ok(());
    }
    // Existing exposure is the more specific reason a ceiling cannot work.
    if session.has_uncertain_usage() {
        return Err(AgentError::UsageUncertain);
    }
    if let Some(limit) = cost_limit {
        if session.has_unpriced_usage() {
            return Err(AgentError::CostUnavailable { limit });
        }
    }
    output_cap.ok_or(AgentError::OutputLimitUnavailable)?;
    Ok(())
}

fn reservation_output_tokens(
    session: &Session,
    model: &Model,
    requested: u64,
    token_limit: Option<u64>,
    cost_limit: Option<u64>,
) -> Result<u64, AgentError> {
    let cap = octet_ai::effective_output_token_cap(model, Some(requested));
    require_enforceable_output_cap(session, cap, token_limit, cost_limit)?;
    // Only operations without hard ceilings may use an unenforced planning
    // estimate. It is never a fabricated bound for a hard reservation.
    Ok(cap.unwrap_or(requested))
}

fn reserve_request_tokens(
    session: &Session,
    input_tokens: u64,
    output_tokens: u64,
    limit: Option<u64>,
) -> Result<(), AgentError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    if session.has_uncertain_usage() {
        return Err(AgentError::UsageUncertain);
    }
    let current = session_total_tokens_for_own_context(session);
    let reserved = input_tokens.saturating_add(output_tokens);
    if current >= limit || current.saturating_add(reserved) > limit {
        return Err(AgentError::TokenLimit {
            current,
            reserved,
            limit,
        });
    }
    Ok(())
}

fn reserve_request_cost(
    session: &Session,
    model: &Model,
    input_tokens: u64,
    output_tokens: u64,
    limit: Option<u64>,
    retention: CacheRetention,
) -> Result<(), AgentError> {
    reserve_request_cost_with_tier(
        session,
        model,
        input_tokens,
        output_tokens,
        limit,
        None,
        retention,
    )
}

fn reserve_request_cost_with_tier(
    session: &Session,
    model: &Model,
    input_tokens: u64,
    output_tokens: u64,
    limit: Option<u64>,
    service_tier: Option<ServiceTier>,
    retention: CacheRetention,
) -> Result<(), AgentError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    if session.has_uncertain_usage() {
        return Err(AgentError::UsageUncertain);
    }
    let current = session.total_cost_microdollars();
    if session.has_unpriced_usage() {
        return Err(AgentError::CostUnavailable { limit });
    }
    // Fallback quotes have no one-hour cache-write tariff. A route that can
    // request one-hour writes cannot enforce a hard ceiling if the server
    // chooses a fallback; do not invent a price for that bucket.
    if retention == CacheRetention::Long
        && model.spec.protocol == Protocol::AnthropicMessages
        && model.spec.cache.supports_long_retention
        && model
            .spec
            .preset
            .anthropic_compat
            .as_ref()
            .is_some_and(|compat| !compat.allowed_fallback_models.is_empty())
    {
        return Err(AgentError::CostUnavailable { limit });
    }
    let reserved = worst_case_request_cost(model, input_tokens, output_tokens, service_tier)
        .ok_or(AgentError::CostUnavailable { limit })?;
    if current >= limit || current.saturating_add(reserved) > limit {
        return Err(AgentError::CostLimit {
            current,
            reserved,
            limit,
        });
    }
    Ok(())
}

fn assistant_text(response: &octet_ai::Response) -> Option<String> {
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
fn snapcompact_source(preparation: &HandoffPreparation) -> String {
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

struct CompactionContext<'a> {
    run_id: &'a str,
    resource_owner: &'a str,
    retry_hooks: &'a [Arc<dyn ProviderRetryHook>],
    compaction_strategy: Option<&'a Arc<dyn CompactionStrategy>>,
    max_network_wait: Option<Duration>,
    provider_retries_enabled: bool,
    client: &'a AiClient,
    /// Active model, used for context-window sizing and the normal request.
    model: &'a Model,
    /// Optional configured route used for the summary request itself.
    compaction_model: &'a Model,
    summary_operation: crate::events::ProviderOperation,
    session: &'a mut Session,
    usage: &'a mut Usage,
    run_cost: &'a mut CostAccumulator,
    cache_retention: CacheRetention,
    reasoning: &'a ReasoningConfig,
    reasoning_mode: ReasoningMode,
    session_id: &'a str,
    max_session_tokens: Option<u64>,
    max_session_cost_microdollars: Option<u64>,
    abort: &'a AbortFlag,
    mode: AgentCompactionMode,
    threshold_fraction: f64,
    keep_recent_tokens: u64,
    events: &'a mpsc::UnboundedSender<AgentEvent>,
    context: &'a ContextTracker,
    tool_generation: u64,
    capacity: &'a mut ContextCapacityCache,
    /// Explicit span observer copied from the owning agent for this compaction.
    telemetry: TelemetryContext,
}

struct CapacityEstimate {
    input_tokens: u64,
    max_output_tokens: u64,
    active_system: String,
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
struct ContextCapacityCache {
    head: Option<EntryId>,
    tool_generation: u64,
    structural_tokens: u64,
    provider_tokens: Option<u64>,
    responses_items: Option<usize>,
    valid: bool,
    #[cfg(test)]
    full_rebuilds: usize,
}

impl ContextCapacityCache {
    fn seeded(session: &Session, tool_generation: u64, context: &ContextBreakdown) -> Self {
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

    fn invalidate(&mut self) {
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
    fn advance_messages(&mut self, session: &Session) -> bool {
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
            if matches!(entry.value, EntryValue::Compaction { .. }) {
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

    fn advance_for_model(&mut self, session: &Session, model: &Model) -> bool {
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
                EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. }
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
    fn rebuild(
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

    fn estimate(
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
    fn observe_assistant_response(&mut self, session: &Session, model: &Model, usage: &Usage) {
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
    fn full_rebuilds(&self) -> usize {
        self.full_rebuilds
    }
}

/// An immutable request snapshot assembled only after context capacity has
/// been established. Compaction and dynamic tool publication can both cross
/// an await before the provider request is opened, so the snapshot carries the
/// durable cursor and the exact prompt/tool generations it was built from.
struct PreparedTurn {
    durable_head: Option<EntryId>,
    active_system: String,
    tool_generation: u64,
    request: Request,
    input_tokens: u64,
}

impl PreparedTurn {
    fn new(
        durable_head: Option<EntryId>,
        active_system: String,
        tool_generation: u64,
        request: Request,
        input_tokens: u64,
    ) -> Self {
        Self {
            durable_head,
            active_system,
            tool_generation,
            request,
            input_tokens,
        }
    }

    /// The request may be sent only while all inputs used to prepare it still
    /// describe the authoritative session. A mismatch is retried through the
    /// normal turn-boundary path, which rebuilds context and tool maps instead
    /// of mixing generations in one provider call.
    fn is_current(&self, session: &Session, active_system: &str, tool_generation: u64) -> bool {
        self.durable_head == session.head()
            && self.active_system == active_system
            && self.tool_generation == tool_generation
            && self.request.system.as_deref()
                == (!active_system.is_empty()).then_some(active_system)
    }
}

impl CompactionContext<'_> {
    async fn call(
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
        let text = assistant_text(&response).ok_or_else(|| AgentError::IncompleteResponse {
            stop_reason: "compaction summary was empty or whitespace-only".to_owned(),
        })?;
        // This is shared by autonomous compaction and explicit callers such as
        // `/compact`; reject bad provider output before either path can merge
        // it into a durable handoff.
        validate_compaction_summary_part(&text)?;
        CompletionAttributes::usage(&response.usage)
            .with_uncertainty(self.session.has_uncertain_usage())
            .record(&summary_request_guard.span);
        summary_request_guard.finish(false);
        summary_guard.finish(false);
        Ok(Some(text))
    }

    /// Generate a Pi-compatible structured handoff, including a dedicated
    /// summary when the retained boundary splits the current turn.
    async fn summarize(
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
    async fn render_snapcompact(
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

    fn preferred_boundary(&self) -> Result<Option<EntryId>, AgentError> {
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

    fn oldest_reducible_boundary(&self) -> Option<EntryId> {
        turn_starts(self.session).get(1).cloned()
    }

    fn begin_compaction(
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

    fn finish_compaction(
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

    async fn compact_native_responses(
        &mut self,
        system: &str,
        tools: &[ToolDef],
        reason: CompactionReason,
    ) -> Result<CompactionInfo, AgentError> {
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
        operation
    }

    async fn compact_boundary(
        &mut self,
        first_kept: EntryId,
        system: &str,
        tools: &[ToolDef],
        reason: CompactionReason,
    ) -> Result<CompactionInfo, AgentError> {
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
        }

        self.telemetry = turn_scope;
        self.finish_compaction(id, system, tools, reason, &operation, self.compaction_model);
        compaction_guard.finish(operation.is_err());
        operation
    }

    async fn ensure_capacity(
        &mut self,
        system: &str,
        tools: &[ToolDef],
        compaction_reserve_tokens: u64,
        provider_output_ceiling: u64,
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
        let resolve = |input_tokens, active_system| CapacityEstimate {
            input_tokens,
            max_output_tokens: resolve_request_max_output_tokens(
                context_window,
                input_tokens,
                provider_output_ceiling,
            ),
            active_system,
        };
        let mut native_attempted = false;
        loop {
            let active_system = system.to_owned();
            let estimate = self
                .capacity
                .estimate(
                    self.session,
                    self.model,
                    &active_system,
                    tools,
                    self.tool_generation,
                )?
                .input_tokens;
            let over_capacity = estimate > budget;
            let over_threshold = estimate.saturating_add(compaction_reserve_tokens) > threshold;
            if !over_capacity && (self.mode == AgentCompactionMode::Disabled || !over_threshold) {
                return Ok(resolve(estimate, active_system));
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
                    return Ok(resolve(estimate, active_system));
                }
                self.compact_native_responses(&active_system, tools, reason)
                    .await?;
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
                self.compact_boundary(first_kept, &active_system, tools, reason)
                    .await?;
                continue;
            }
            if estimate <= budget {
                return Ok(resolve(estimate, active_system));
            }
            return Err(AgentError::ContextExceeded { estimate, budget });
        }
    }

    async fn force_one_boundary(
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

struct TerminalGateContext<'a> {
    run_id: &'a str,
    resource_owner: &'a str,
    retry_hooks: &'a [Arc<dyn ProviderRetryHook>],
    max_network_wait: Option<Duration>,
    provider_retries_enabled: bool,
    events: &'a mpsc::UnboundedSender<AgentEvent>,
    client: &'a AiClient,
    model: &'a Model,
    session: &'a mut Session,
    usage: &'a mut Usage,
    run_cost: &'a mut CostAccumulator,
    cache_retention: CacheRetention,
    session_id: &'a str,
    max_session_tokens: Option<u64>,
    max_session_cost_microdollars: Option<u64>,
    abort: &'a AbortFlag,
}

impl TerminalGateContext<'_> {
    async fn decide(&mut self, capsule: String) -> Result<TerminalGateDecision, AgentError> {
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

async fn open_provider_stream(
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

/// One provider deferred-poll attempt requested from a [`DeferredPollSource`].
#[derive(Debug)]
pub enum DeferredPollReply {
    /// The provider is still working; the returned handle parks the run again.
    StillDeferred(DeferredHandle),
    /// The provider finished the request.
    Settled(Box<octet_ai::Response>),
    /// The provider failed the poll. The diagnostic must be bounded and must
    /// not contain provider payloads, credentials, or request content. The
    /// admitted attempt may have been dispatched, so its usage is unknown.
    Failed(String),
    /// The poll was refused **before dispatch** (for example a transport permit
    /// that was stale or already consumed). Nothing was billed, no exposure is
    /// created, and the durable effect-pending leaf stays replaceable.
    Refused(String),
}

/// Provider transport for the deferred-fetch half of a suspended run.
///
/// The agent owns the durable lifecycle (permit, effect-pending intent,
/// generation fence, recovery, cancel); the source owns the provider request
/// for one admitted poll. It is called only after the poll's effect-pending
/// intent is durable and never more than once per permit. `permit` is the
/// codec's one-shot transport permit, minted by the agent for this pass and
/// consumed by the provider call; a source must not mint or reuse one.
#[async_trait::async_trait]
pub trait DeferredPollSource: Send + Sync {
    /// Performs exactly one deferred poll against the provider.
    async fn poll_deferred(
        &self,
        handle: &DeferredHandle,
        permit: octet_ai::deferred::DeferredPollPermit,
        leaf_generation: u64,
    ) -> DeferredPollReply;
}

/// A [`DeferredPollSource`] that polls one configured [`AiClient`] route.
///
/// The client owns the transport permit fence: a stale, replayed, or missing
/// permit is refused before any request is dispatched, which this source maps
/// to [`DeferredPollReply::Refused`] so a purely local refusal never creates
/// billing exposure. A transport that cannot park or poll fails closed for the
/// same reason. Once a request is dispatched, any failure is reported as
/// [`DeferredPollReply::Failed`] because its usage cannot be known.
#[derive(Clone)]
pub struct AiDeferredPollSource {
    client: AiClient,
    model: Model,
    wait_ms: Option<u64>,
}

impl std::fmt::Debug for AiDeferredPollSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiDeferredPollSource")
            .field("endpoint", &self.model.endpoint.id.0)
            .field("model", &self.model.spec.id.0)
            .field("wait_ms", &self.wait_ms)
            .finish()
    }
}

impl AiDeferredPollSource {
    /// Builds a poll source for one client/model route.
    pub fn new(client: AiClient, model: Model) -> Self {
        Self {
            client,
            model,
            wait_ms: None,
        }
    }

    /// Bounds the provider long-poll; `Some(0)` performs one status check.
    pub fn with_wait_ms(mut self, wait_ms: Option<u64>) -> Self {
        self.wait_ms = wait_ms;
        self
    }

    fn refusal_or_failure(&self, error: AiError) -> DeferredPollReply {
        match &error {
            AiError::Deferred(refusal) => DeferredPollReply::Refused(refusal.to_string()),
            AiError::Unsupported(octet_ai::UnsupportedError::Deferred) => {
                DeferredPollReply::Refused(
                    "deferred provider responses are unsupported on this transport".to_owned(),
                )
            }
            _ => DeferredPollReply::Failed(public_error_diagnostic(
                &AgentError::Ai(error),
                &self.model.endpoint.id.0,
                &self.model.spec.id.0,
            )),
        }
    }
}

#[async_trait::async_trait]
impl DeferredPollSource for AiDeferredPollSource {
    async fn poll_deferred(
        &self,
        handle: &DeferredHandle,
        permit: octet_ai::deferred::DeferredPollPermit,
        leaf_generation: u64,
    ) -> DeferredPollReply {
        let codec_handle = octet_ai::deferred::DeferredHandle::from(handle.clone());
        let mut stream = match self
            .client
            .fetch_deferred(
                &self.model,
                codec_handle,
                permit,
                leaf_generation,
                self.wait_ms,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => return self.refusal_or_failure(error),
        };
        loop {
            match stream.next().await {
                Some(Ok(StreamEvent::Finished(response))) => {
                    if response.stop_reason == StopReason::Deferred {
                        return match response.deferred.clone() {
                            Some(handle) => {
                                DeferredPollReply::StillDeferred(DeferredHandle::from(handle))
                            }
                            None => DeferredPollReply::Failed(
                                "provider parked the poll without a deferred handle".to_owned(),
                            ),
                        };
                    }
                    return DeferredPollReply::Settled(Box::new(response));
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => return self.refusal_or_failure(error),
                None => {
                    return DeferredPollReply::Failed(
                        "deferred poll stream ended without a terminal response".to_owned(),
                    );
                }
            }
        }
    }
}

/// Result of one deferred resume pass.
#[derive(Debug)]
pub enum DeferredRunOutcome {
    /// The durable record is terminal (settled, cancelled, or failed); nothing
    /// may poll it again.
    Finished {
        /// Durable operation identity.
        operation_id: String,
        /// Terminal state label (`settled`, `cancelled`, or `failed`).
        state: &'static str,
    },
    /// Observe-only pass: nothing was written and no provider work started.
    Waiting(SuspendedRunObservation),
    /// Fail closed: nothing was written and no provider work started.
    Refused(Box<DeferredPollRefusal>),
    /// The provider is still working; the run is parked again at
    /// `deferred.suspended` with a bumped generation.
    Suspended(SuspendedRunObservation),
    /// The poll settled; these reserved durable ids name the commit slots for
    /// the response entry and its usage record. The caller commits the response
    /// exactly once; the durable tombstone already blocks a re-poll.
    Settled {
        /// The complete provider response.
        response: Box<octet_ai::Response>,
        /// Reserved durable response entry id.
        response_id: String,
        /// Reserved durable usage id.
        usage_id: String,
    },
    /// Terminal failure; the run must not poll again.
    Failed(Box<DeferredSuspendFailure>),
    /// The admitted poll was refused before any provider work (for example a
    /// transport permit that was already consumed). Nothing was billed and the
    /// durable leaf stays replaceable for a later permitted pass.
    PollRefused(String),
}

/// The durable model identity used to validate deferred provider handles.
///
/// Octet has no separate provider registry identity in a model spec, so the
/// endpoint id is the durable provider and the model id is the provider-local
/// model identity. A handle whose provider, model id, or api does not match the
/// run's durable configuration or the response that carried it is a terminal
/// failure, never a suspension.
pub fn deferred_model_identity(model: &Model) -> ModelIdentity {
    ModelIdentity::new(model.endpoint.id.0.clone(), model.spec.id.0.clone())
}

fn stop_reason_label(reason: DeferredStopReason) -> &'static str {
    match reason {
        DeferredStopReason::Deferred => "deferred",
        DeferredStopReason::Settled => "settled",
        DeferredStopReason::Failed => "failed",
        DeferredStopReason::Aborted => "aborted",
    }
}

fn refusal_stop_reason(refusal: &DeferredPollRefusal) -> &'static str {
    match refusal.kind {
        crate::tools::deferred::DeferredPollRefusalKind::ExpiredHandle { .. } => "aborted",
        // An unknown outcome is refused, not terminal: the run stays parked at
        // its effect-pending leaf until an explicit replacement resume.
        crate::tools::deferred::DeferredPollRefusalKind::StalePermit { .. }
        | crate::tools::deferred::DeferredPollRefusalKind::AlreadyConsumed
        | crate::tools::deferred::DeferredPollRefusalKind::UnknownPollOutcome { .. }
        | crate::tools::deferred::DeferredPollRefusalKind::ForeignHandle(_) => "refused",
    }
}

fn bounded_deferred_label(value: &str) -> String {
    const LIMIT: usize = 256;
    if value.len() <= LIMIT {
        return value.to_owned();
    }
    let mut end = LIMIT;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Whether one provider turn reported any billable token bucket.
///
/// A parked request reports no billed work; recording a zero-token unpriced
/// operation would falsely block hard cost ceilings.
fn usage_is_billed(usage: &Usage) -> bool {
    usage.input_tokens > 0
        || usage.cache_read_tokens > 0
        || usage.cache_write_tokens > 0
        || usage.output_tokens > 0
        || usage.reasoning_tokens > 0
        || usage.total_tokens > 0
}

impl Agent {
    /// Creates a new agent: canonicalizes the sandbox workspace and validates
    /// the registered extensions (duplicate tool names are rejected).
    pub fn new(mut config: AgentConfig) -> Result<Self, AgentError> {
        if config.extensions.duplicate_compaction_strategy {
            return Err(AgentError::InvalidCompactionPolicy(
                "multiple enabled extensions declared compaction_strategy".into(),
            ));
        }
        if let Some(duplicate) = config.extensions.duplicate_tools.first() {
            return Err(AgentError::DuplicateTool(duplicate.clone()));
        }
        if let Some(namespace) = config.extensions.invalid_metadata_namespaces.first() {
            return Err(AgentError::ExtensionMetadataNamespace(namespace.clone()));
        }
        let workspace = config.sandbox.workspace.canonicalize().map_err(|e| {
            AgentError::Workspace(format!("{}: {e}", config.sandbox.workspace.display()))
        })?;
        if !workspace.is_dir() {
            return Err(AgentError::Workspace(format!(
                "{}: not a directory",
                workspace.display()
            )));
        }
        config.sandbox.workspace = workspace;
        if config.model.responses_features().reasoning_effort_updates {
            if let Some((_, effective)) = config
                .session
                .responses_reasoning(&config.model.endpoint.id, &config.model.spec.id)?
            {
                config.reasoning = effective;
            }
        }
        let resource_owner = config.session.resource_owner_key();
        let session_id = config.session_id.unwrap_or_else(|| resource_owner.clone());
        let max_output_tokens = config.model.spec.limits.max_output_tokens;
        let tool_scope = next_tool_scope();
        let bash_owner = BashOwnerLease::acquire(&resource_owner);
        Ok(Self {
            client: config.client,
            model: config.model,
            session: config.session,
            extensions: config.extensions,
            sandbox: config.sandbox,
            effect_broker: config.effect_broker,
            system: config.system,
            max_turns: config.max_turns,
            reasoning: config.reasoning,
            reasoning_mode: config.reasoning_mode,
            cache_retention: config.cache_retention,
            tool_schema_budget_bytes: DEFAULT_TOOL_SCHEMA_BUDGET_BYTES,
            compaction_model: None,
            auto_compaction_mode: AgentCompactionMode::Local,
            compaction_threshold_fraction: 1.0,
            compaction_keep_recent_tokens: DEFAULT_KEEP_RECENT_TOKENS,
            session_id,
            resource_owner,
            bash_owner,
            tool_scope,
            completion_policy: CompletionPolicy::Natural,
            output_modalities: OutputModalities::Text,
            max_output_tokens,
            service_tier: None,
            prompt_model_source: None,
            tool_prompt_section: false,
            #[cfg(any(unix, windows))]
            partial_output_checkpoints: None,
            prompt_color: None,
            prompt_display_text: None,
            max_session_tokens: None,
            max_session_cost_microdollars: None,
            provider_retries_enabled: true,
            max_network_wait: None,
            owner_tool_images_enabled: false,
            ultra_observation_managed: false,
            delegation: None,
            delegation_model_resolver: None,
            last_run_lifecycle: None,
            telemetry: TelemetryContext::default(),
        })
    }

    /// Installs the explicit span observer used by runs of this agent.
    ///
    /// The context is inert by default. Spans observe boundaries only: they
    /// never write durable session accounting, so an installed observer (or a
    /// missing one) cannot change usage, cost or uncertainty outcomes. A
    /// delegated child agent receives the parent's delegation-span context when
    /// its run is driven, so child spans nest under `octet.agent.delegation`.
    pub fn set_telemetry_context(&mut self, telemetry: TelemetryContext) {
        if let Some(delegation) = &self.delegation {
            // Child runs are driven outside this agent's own stream, so the
            // bound delegation runtime observes with the same explicit context.
            delegation.set_span_context(telemetry.clone());
        }
        self.telemetry = telemetry;
    }

    /// Returns the span observer currently installed for this agent.
    pub fn telemetry_context(&self) -> &TelemetryContext {
        &self.telemetry
    }

    /// Builds an owned startup request that can establish the Responses
    /// WebSocket while the frontend is idle. The request contains only the
    /// current durable context and uses `generate=false` in `AiClient`; it is
    /// never part of agent accounting or persistence.
    pub fn responses_prewarm_request(
        &self,
    ) -> Result<Option<(AiClient, Model, Request)>, AgentError> {
        if !matches!(
            self.model.endpoint.transport,
            octet_ai::EndpointTransport::WebSocketPreferred
        ) || self.model.spec.protocol != Protocol::OpenAiResponses
        {
            return Ok(None);
        }
        if self.session.has_unsettled_native_steering() {
            return Ok(None);
        }
        let responses = match self.auto_compaction_mode {
            AgentCompactionMode::NativeResponses
                if !self.model.responses_features().reasoning_effort_updates =>
            {
                Some(native_responses_options(
                    &self.session,
                    &self.model,
                    &self.system,
                    self.service_tier,
                )?)
            }
            AgentCompactionMode::NativeResponses
            | AgentCompactionMode::Local
            | AgentCompactionMode::Disabled => durable_responses_options(
                &self.session,
                &self.model,
                &self.system,
                self.service_tier,
            )?,
        };
        let tools: Vec<_> = self
            .extensions
            .tool_snapshot()
            .1
            .iter()
            .map(|tool| advertised_tool_definition(tool.as_ref(), &self.model))
            .collect();
        require_tool_schema_budget(&tools, self.tool_schema_budget_bytes)?;
        let request = Request {
            system: (!self.system.is_empty()).then(|| self.system.clone()),
            messages: self.session.context()?,
            tools,
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(self.max_output_tokens),
            temperature: None,
            stop: Vec::new(),
            reasoning: request_reasoning_for_replay(
                &self.session,
                &self.model,
                responses.as_ref(),
                &self.reasoning,
            )?,
            reasoning_mode: self.reasoning_mode,
            responses,
            output_format: OutputFormat::Text,
            output_modalities: self.output_modalities.clone(),
            compatibility: CompatibilityMode::Strict,
            cache_retention: self.cache_retention,
            session_id: Some(self.session_id.clone()),
        };
        Ok(Some((self.client.clone(), self.model.clone(), request)))
    }

    /// Attempt one opt-in, cost-reserved Anthropic prompt-cache keepalive at a
    /// settled idle boundary. No model output or synthetic prompt is persisted.
    /// `Off` and unsupported/early/over-budget requests make no network call.
    /// This API is an unscheduled library building block, not an idle timer;
    /// the coding-agent host must opt in at an actual idle boundary.
    pub async fn warm_prompt_cache(
        &mut self,
        mode: crate::cache_warmer::CacheWarmMode,
        policy: crate::cache_warmer::CacheWarmPolicy,
    ) -> Result<crate::cache_warmer::CacheWarmOutcome, AgentError> {
        use crate::cache_warmer::{reservation, CacheWarmOutcome};
        if mode != crate::cache_warmer::CacheWarmMode::Idle
            || !crate::cache_warmer::is_direct_anthropic(&self.model)
            || self.cache_retention != CacheRetention::Short
            || self.reasoning != ReasoningConfig::Off
        {
            return Ok(CacheWarmOutcome::Skipped);
        }
        // Existing context estimates include system/tool schema framing and
        // provider-reconciled prefix usage. Reserve room for the synthetic
        // suffix independently; the suffix never enters the session.
        let input_tokens = self
            .request_context_estimate()?
            .input_tokens
            .saturating_add(256);
        let reserved = worst_case_request_cost(&self.model, input_tokens, 1, None);
        if reservation(
            &self.model,
            &self.session,
            self.cache_retention,
            mode,
            policy,
            input_tokens,
            now_unix_millis(),
            reserved,
        )
        .is_none()
        {
            return Ok(CacheWarmOutcome::Skipped);
        }
        self.ensure_request_cost_capacity(&self.model, input_tokens, 1)?;
        reserve_request_tokens(&self.session, input_tokens, 1, self.max_session_tokens)?;
        let (_, tools) = self.extensions.tool_snapshot();
        let tools: Vec<_> = tools
            .iter()
            .map(|tool| advertised_tool_definition(tool.as_ref(), &self.model))
            .collect();
        require_tool_schema_budget(&tools, self.tool_schema_budget_bytes)?;
        let mut messages = self.session.context()?;
        messages.push(Message::User(UserMessage {
            content: vec![UserPart::Text("Reply with a single period.".into())],
        }));
        let system = self.model_visible_system(true);
        let request = Request {
            system: (!system.is_empty()).then_some(system),
            messages,
            tools,
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(1),
            temperature: None,
            stop: Vec::new(),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: self.reasoning_mode,
            responses: None,
            output_format: OutputFormat::Text,
            output_modalities: OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: CacheRetention::WarmShort,
            session_id: Some(self.session_id.clone()),
        };
        crate::cache_warmer::dispatch(
            &self.client,
            &self.model,
            &mut self.session,
            request,
            policy.deadline,
        )
        .await
        .map_err(AgentError::from)
    }

    /// Read-only access to the agent's session (its entries and head).
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub(crate) fn resource_owner_id(&self) -> &str {
        &self.resource_owner
    }

    /// Persist a non-model-visible terminal marker for a frontend-owned run.
    ///
    /// Callers should record this only after the [`Run`] has been dropped, so
    /// the run no longer holds the authoritative mutable session borrow.
    pub fn record_run_outcome(
        &mut self,
        outcome: SessionRunOutcome,
    ) -> Result<EntryId, AgentError> {
        self.session.append_run_outcome(outcome).map_err(Into::into)
    }

    /// Read-only access to the selected model.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Requested output modalities for subsequent model turns.
    ///
    /// Text is the default. Generated audio is currently delivered as a
    /// complete [`AgentEvent::OutputMedia`] event and retained in
    /// [`RunOutput::media`]; unsupported requests fail through `octet-ai`'s
    /// normal capability validation.
    pub fn output_modalities(&self) -> &OutputModalities {
        &self.output_modalities
    }

    /// Configure output modalities for subsequent runs.
    pub fn set_output_modalities(&mut self, output_modalities: OutputModalities) {
        self.output_modalities = output_modalities;
        self.sync_delegation_runtime_settings();
    }

    /// Replace the system prompt at an idle boundary. Product frontends use
    /// this to apply typed extension context without exposing private agent
    /// state; the value is cloned into the next run when [`prompt`](Self::prompt)
    /// starts.
    pub fn set_system_prompt(&mut self, system: impl Into<String>) {
        let system = system.into();
        if let Some(binding) = &self.delegation {
            binding.update_base_system(system.clone());
        }
        self.system = system;
        let delegation_instructions = self
            .delegation
            .as_ref()
            .map(|binding| binding.system_instructions().to_owned())
            .filter(|instructions| !instructions.is_empty());
        if let Some(instructions) = delegation_instructions {
            self.append_system_instructions(instructions);
        }
    }

    /// Returns the complete system prompt used by subsequent runs.
    pub fn system_prompt(&self) -> &str {
        &self.system
    }

    /// Enables the bounded host-side V2 collaboration runtime.
    ///
    /// Child agents inherit the resolved model, sandbox, approved extension
    /// tools, reasoning, compaction, completion, and cost settings present at
    /// this idle boundary. Each child receives an isolated durable session.
    pub fn enable_v2_delegation(
        &mut self,
        config: DelegationConfig,
    ) -> Result<std::path::PathBuf, DelegationError> {
        self.enable_v2_delegation_with_surface(config, true)
    }

    /// Enables the bounded V2 runtime without exposing native root
    /// collaboration tools. Product layers use this when an extension owns the
    /// user-facing orchestration and observation surface.
    pub fn enable_v2_delegation_extension_only(
        &mut self,
        config: DelegationConfig,
    ) -> Result<std::path::PathBuf, DelegationError> {
        self.enable_v2_delegation_with_surface(config, false)
    }

    /// Installs or refreshes the host-owned configured-model routing service.
    pub fn set_delegation_model_resolver(
        &mut self,
        resolver: Arc<dyn crate::delegation::AgentModelResolver>,
    ) {
        if let Some(binding) = &self.delegation {
            binding.set_model_resolver(resolver.clone());
        }
        self.delegation_model_resolver = Some(resolver);
    }

    fn enable_v2_delegation_with_surface(
        &mut self,
        config: DelegationConfig,
        root_tools: bool,
    ) -> Result<std::path::PathBuf, DelegationError> {
        if self.delegation.is_some() {
            return Err(DelegationError::AlreadyEnabled);
        }
        let template = DelegationTemplate {
            model_resolver: std::sync::RwLock::new(self.delegation_model_resolver.clone()),
            client: self.client.clone(),
            model: self.model.clone(),
            base_system: std::sync::RwLock::new(self.system.clone()),
            sandbox: self.sandbox.clone(),
            effect_broker: self.effect_broker.clone(),
            extensions: self.extensions.clone(),
            max_turns: self.max_turns,
            reasoning: std::sync::RwLock::new(self.reasoning.clone()),
            reasoning_mode: self.reasoning_mode,
            cache_retention: self.cache_retention,
            runtime: std::sync::RwLock::new(self.delegation_runtime_settings()),
        };
        let binding = enable_root_delegation(self, config, template, root_tools)?;
        let team_directory = binding.team_directory().to_path_buf();
        self.delegation = Some(binding);
        Ok(team_directory)
    }

    /// Returns the authoritative host count of active delegated workers.
    ///
    /// Returns zero when delegation is not enabled; frontend roster snapshots
    /// are not consulted.
    pub fn active_delegated_worker_count(&self) -> usize {
        self.delegation
            .as_ref()
            .map_or(0, DelegationBinding::active_worker_count)
    }

    /// Returns the private team directory when V2 delegation is enabled.
    pub fn delegation_team_directory(&self) -> Option<&std::path::Path> {
        self.delegation
            .as_ref()
            .map(DelegationBinding::team_directory)
    }

    /// Opens one exact child transcript from this agent's current delegation
    /// team as a read-only session. Opaque references from another parent are
    /// never resolved.
    pub fn open_delegated_session_reference(
        &self,
        extension_principal: &str,
        reference: &str,
    ) -> Result<Option<Session>, AgentError> {
        let Some(binding) = self.delegation.as_ref() else {
            return Ok(None);
        };
        binding.open_session_reference(extension_principal, reference)
    }

    /// Binds an executable extension's negotiated child-session service to
    /// this root agent's V2 delegation manager.
    pub fn bind_extension_agent_sessions(
        &self,
        process: &ExtensionProcess,
    ) -> Result<bool, AgentError> {
        if !process.supports_feature(EXTENSION_FEATURE_AGENT_SESSIONS) {
            return Ok(false);
        }
        let binding = self.delegation.as_ref().ok_or_else(|| {
            AgentError::Delegation(
                "an extension negotiated agent_sessions before V2 delegation was enabled".into(),
            )
        })?;
        let service = binding
            .extension_service(
                process.agent_session_principal(),
                self.session_id.clone(),
                self.resource_owner.clone(),
            )
            .map_err(AgentError::Delegation)?;
        process
            .bind_agent_session_service(service)
            .map_err(|error| AgentError::Delegation(error.to_string()))?;
        Ok(true)
    }

    fn delegation_runtime_settings(&self) -> DelegationRuntimeSettings {
        DelegationRuntimeSettings {
            compaction_model: self.compaction_model.clone(),
            auto_compaction_mode: self.auto_compaction_mode,
            auto_compaction_threshold: self.compaction_threshold_fraction,
            compaction_keep_recent_tokens: self.compaction_keep_recent_tokens,
            completion_policy: self.completion_policy,
            output_modalities: self.output_modalities.clone(),
            max_output_tokens: self.max_output_tokens,
            max_session_tokens: self.max_session_tokens,
            max_session_cost_microdollars: self.max_session_cost_microdollars,
            provider_retries_enabled: self.provider_retries_enabled,
            max_network_wait: self.max_network_wait,
            tool_schema_budget_bytes: self.tool_schema_budget_bytes,
        }
    }

    fn sync_delegation_runtime_settings(&self) {
        if let Some(binding) = &self.delegation {
            binding.update_runtime_settings(self.delegation_runtime_settings());
        }
    }

    pub(crate) fn append_system_instructions(&mut self, instructions: String) {
        if !self.system.is_empty() {
            self.system.push_str("\n\n");
        }
        self.system.push_str(&instructions);
    }

    pub(crate) fn install_delegation_tools(&mut self, tools: Vec<Arc<dyn Tool>>) {
        for tool in tools {
            self.extensions.tool_arc(tool);
        }
    }

    pub(crate) fn set_delegation_binding(
        &mut self,
        binding: DelegationBinding,
    ) -> Result<(), DelegationError> {
        if self.delegation.is_some() {
            return Err(DelegationError::AlreadyEnabled);
        }
        self.delegation = Some(binding);
        Ok(())
    }

    pub(crate) fn mark_ultra_observation_managed(&mut self) {
        self.ultra_observation_managed = true;
    }

    /// Set the stable semantic creator/source key persisted with future user
    /// prompts (for example `openai` or `deepseek`). This is presentation
    /// metadata only and never enters provider-visible message content.
    pub fn set_prompt_model_source(&mut self, source: Option<String>) {
        self.prompt_model_source = source.filter(|source| !source.trim().is_empty());
    }

    /// Set the exact inert sRGB highlight persisted with future user prompts.
    /// Validation and normalization happen at the durable session boundary.
    pub fn set_prompt_color(&mut self, color: Option<String>) {
        self.prompt_color = color.filter(|color| !color.trim().is_empty());
    }

    /// Set the transcript text for the next submitted prompt. It is consumed
    /// exactly once by `prompt`; model-visible text remains in the durable
    /// message payload for replay. An explicitly empty string is retained so
    /// media-only turns do not expose synthetic model instructions as caller text.
    pub fn set_prompt_display_text(&mut self, text: Option<String>) {
        self.prompt_display_text = text;
    }

    fn prompt_entry_metadata(&mut self) -> EntryMetadata {
        EntryMetadata {
            prompt_model: Some(self.model.spec.id.clone()),
            prompt_model_source: self.prompt_model_source.clone(),
            prompt_color: self.prompt_color.clone(),
            display_text: self.prompt_display_text.take(),
            run_outcome: None,
            tool_output: None,
            tool_started_unix_ms: None,
            tool_finished_unix_ms: None,
            native_steering: None,
            local_synthetic_assistant: false,
            extension_metadata: Default::default(),
        }
    }

    /// Check a prospective provider request against this agent's configured
    /// conservative cost reservation. Product-level manual subrequests use
    /// this same gate as autonomous turns.
    pub fn ensure_request_cost_capacity(
        &self,
        model: &Model,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(), AgentError> {
        let output_tokens = reservation_output_tokens(
            &self.session,
            model,
            output_tokens,
            self.max_session_tokens,
            self.max_session_cost_microdollars,
        )?;
        reserve_request_tokens(
            &self.session,
            input_tokens,
            output_tokens,
            self.max_session_tokens,
        )?;
        reserve_request_cost(
            &self.session,
            model,
            input_tokens,
            output_tokens,
            self.max_session_cost_microdollars,
            self.cache_retention,
        )
    }

    /// Configure a conservative hard ceiling for total provider-reported
    /// tokens across this session. Every provider operation reserves its
    /// estimated input plus maximum output before network I/O.
    pub(crate) fn set_max_session_tokens(&mut self, limit: Option<u64>) {
        self.max_session_tokens = limit;
        self.sync_delegation_runtime_settings();
    }

    /// Configure a conservative hard ceiling for billable session requests.
    /// Before every normal or compaction request, priced models reserve their
    /// worst-case input/output cost; a request that could cross the ceiling is
    /// rejected before network I/O.
    pub fn set_max_session_cost_microdollars(&mut self, limit: Option<u64>) {
        self.max_session_cost_microdollars = limit;
        self.sync_delegation_runtime_settings();
    }

    /// Opt into bounded accepted inline tool images for the owning run consumer.
    /// General observers remain payload-free. Disabled by default; interactive
    /// hosts should enable this immediately before invoking their common prompt
    /// path, including after an Agent rebuild. This is not a persisted setting.
    pub fn set_owner_tool_images_enabled(&mut self, enabled: bool) {
        self.owner_tool_images_enabled = enabled;
    }

    /// Enable or disable transient provider retries for subsequent runs.
    pub fn set_provider_retries_enabled(&mut self, enabled: bool) {
        self.provider_retries_enabled = enabled;
        self.sync_delegation_runtime_settings();
    }

    /// Limits elapsed recovery for each definitely-pre-send outage, including
    /// subsequent request opening. Successful opening ends the outage. `None` waits until
    /// connectivity returns or cancellation; zero disables outage waiting.
    /// This never extends external job deadlines or provider body deadlines.
    pub fn set_max_network_wait(&mut self, limit: Option<Duration>) {
        self.max_network_wait = limit;
        self.sync_delegation_runtime_settings();
    }

    #[cfg(test)]
    pub(crate) fn max_network_wait(&self) -> Option<Duration> {
        self.max_network_wait
    }

    /// Sets the maximum serialized JSON bytes for provider-visible tool schemas.
    ///
    /// The default is [`DEFAULT_TOOL_SCHEMA_BUDGET_BYTES`]. A request that
    /// would exceed this hard limit is refused; octet never truncates, drops,
    /// or rewrites tool definitions to make it fit. Zero permits only an empty
    /// tool list.
    pub fn set_tool_schema_budget_bytes(&mut self, max_bytes: usize) {
        self.tool_schema_budget_bytes = max_bytes;
        self.sync_delegation_runtime_settings();
    }

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
    const LEGACY_COMPACTION_TOKENS_PER_TURN: u64 = 1_000;

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

    /// Provider-advertised output ceiling for the active model.
    pub fn max_output_tokens(&self) -> u64 {
        self.max_output_tokens
    }

    /// Minimum output headroom used by autonomous capacity checks.
    pub fn compaction_reserve_tokens(&self) -> u64 {
        agent_compaction_reserve_tokens(&self.model, &self.reasoning)
    }

    #[cfg(test)]
    pub(crate) fn max_session_tokens(&self) -> Option<u64> {
        self.max_session_tokens
    }

    /// Apply the root agent's provider output ceiling to a delegated child.
    pub(crate) fn inherit_max_output_tokens(&mut self, max_output_tokens: u64) {
        self.max_output_tokens = max_output_tokens;
        self.sync_delegation_runtime_settings();
    }

    /// Estimate the next request using the same provider-reconciled baseline
    /// as autonomous capacity checks, without mutating the session.
    pub fn request_context_estimate(&self) -> Result<RequestContextEstimate, SessionError> {
        let messages = self.session.context_ref()?;
        let system = self.model_visible_system(true);
        let tools = self.extensions.tool_definitions();
        Ok(reconcile_context_estimate(
            &self.session,
            &self.model,
            &system,
            &messages,
            &tools,
        ))
    }

    /// Build the detailed, provider-reconciled context categories on demand.
    pub fn request_context_breakdown(&self) -> Result<ContextBreakdown, SessionError> {
        let messages = self.session.context_ref()?;
        let system = self.model_visible_system(true);
        let tools = self.extensions.tool_definitions();
        Ok(context_breakdown(
            &self.session,
            &self.model,
            &system,
            &messages,
            &tools,
        ))
    }

    /// Complete route-affine Responses replay input for the active branch.
    ///
    /// `None` means the active route is not Responses or a legacy/crash gap or
    /// different route makes exact opaque replay unavailable. Ordinary requests
    /// use canonical context in that case; native mode remains fail-closed.
    pub fn responses_replay_input(&self) -> Result<Option<ResponsesInput>, SessionError> {
        if self.model.spec.protocol != Protocol::OpenAiResponses {
            return Ok(None);
        }
        let Some(replay) = self
            .session
            .responses_replay_snapshot(&self.model.endpoint.id, &self.model.spec.id)?
        else {
            return Ok(None);
        };
        let system = self.system.clone();
        Ok(Some(
            octet_ai::responses::encode_responses_replay(
                &self.model,
                (!system.is_empty()).then_some(system.as_str()),
                &replay,
            )
            .map_err(|_| SessionError::Limit("invalid durable Responses replay".into()))?,
        ))
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
        let model = self
            .compaction_model
            .clone()
            .unwrap_or_else(|| self.model.clone());
        let summary = self
            .summary_call(
                &model,
                SUMMARIZATION_SYSTEM_PROMPT,
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
    async fn summary_call(
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
        let tools = self.extensions.tool_definitions();
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

    /// Selects the provider service tier for this agent's Responses requests.
    ///
    /// `None` clears the selection and sends no tier, which is the default and
    /// the only thing an undeclared route can carry. `Some(tier)` is accepted
    /// only on a route whose declared endpoint profile accepts the Responses
    /// `service_tier` field ([`octet_ai::ResponsesRuntimeProfile::accepts_service_tier`],
    /// the Codex subscription runtime today); every other route returns the
    /// codec's typed unsupported error instead of silently dropping a control
    /// that changes provider routing and billing. The same gate is re-applied
    /// when a request is built, so a later route change can never leak the field
    /// onto an endpoint that does not declare it.
    pub fn set_service_tier(&mut self, tier: Option<ServiceTier>) -> Result<(), AgentError> {
        resolve_service_tier(&self.model, tier)?;
        self.service_tier = tier;
        Ok(())
    }

    /// Returns the selected provider service tier, if any.
    pub fn service_tier(&self) -> Option<ServiceTier> {
        self.service_tier
    }

    /// The durable model identity used to validate deferred provider handles.
    ///
    /// Octet has no separate provider registry identity in a model spec, so the
    /// endpoint id is the durable provider and the model id is the provider-local
    /// model identity. A handle whose provider, model id, or api does not match
    /// the run's durable configuration is a terminal failure, never a
    /// suspension.
    pub fn deferred_model_identity(&self) -> ModelIdentity {
        deferred_model_identity(&self.model)
    }

    /// Every durable deferred-run record in this session, including terminal
    /// tombstones. These are auxiliary lifecycle records: never model-visible
    /// context and never usage accounting.
    pub fn deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.session.deferred_runs()
    }

    /// Every non-terminal deferred run that may still be resumed.
    pub fn parked_deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.session.parked_deferred_runs()
    }

    /// Durable state of one deferred run, if any.
    pub fn deferred_run(&self, operation_id: &str) -> Option<DeferredRunRecord> {
        self.session.deferred_run(operation_id)
    }

    /// Parks one deferred provider response at the `deferred.suspended` leaf.
    ///
    /// Row 4.12: the decision core validates the handle against the run's
    /// durable identity and the api of the response that carried it. A valid
    /// handle becomes one durable, replaceable session record and emits
    /// `run_suspend` to registered observers; anything else is a terminal
    /// [`DeferredSuspendDecision::Failed`] with **no durable write** and no
    /// claim that the request settled.
    pub fn suspend_deferred_run(
        &mut self,
        operation_id: &str,
        source_entry_id: &str,
        declaration: DeferredResponseDeclaration,
    ) -> Result<DeferredSuspendDecision, AgentError> {
        let identity = self.deferred_model_identity();
        let stop_reason = stop_reason_label(declaration.stop_reason);
        let store = self.session.deferred_run_store();
        let decision = store.suspend(&identity, operation_id, source_entry_id, declaration)?;
        match &decision {
            DeferredSuspendDecision::Suspended(leaf) => {
                self.observe_deferred_boundary(
                    &leaf.operation_id,
                    "deferred",
                    "suspended",
                    leaf.poll,
                    leaf.generation,
                    false,
                );
                self.emit_run_suspend(leaf);
            }
            DeferredSuspendDecision::Settled => {
                self.observe_deferred_boundary(operation_id, "settled", "settled", 0, 0, false);
            }
            DeferredSuspendDecision::Failed(_) => {
                self.observe_deferred_boundary(operation_id, stop_reason, "failed", 0, 0, false);
            }
        }
        Ok(decision)
    }

    /// Cancels one parked deferred run, fenced on its current generation.
    ///
    /// A cancelled leaf can never be polled, including after a restart. When
    /// the cancelled leaf had an admitted poll whose outcome is unknown, the
    /// abandoned attempt is recorded as sticky session exposure; no usage or
    /// cost is invented and no second poll is made.
    pub fn cancel_deferred_run(
        &mut self,
        operation_id: &str,
        expected_generation: u64,
    ) -> Result<DeferredRunCancellation, AgentError> {
        let store = self.session.deferred_run_store();
        let cancellation = store.cancel(operation_id, expected_generation)?;
        self.observe_deferred_boundary(
            operation_id,
            "aborted",
            "cancelled",
            cancellation
                .previous
                .leaf()
                .map(|leaf| leaf.poll)
                .unwrap_or_default(),
            cancellation.cancelled.generation,
            false,
        );
        if cancellation.abandoned_unknown_poll() {
            self.record_deferred_exposure()?;
        }
        Ok(cancellation)
    }

    /// Resumes one parked deferred run with exactly one poll permit.
    ///
    /// `intent` selects whether this pass owns a permit ([`DeferredResumeIntent::Poll`])
    /// or merely observes. A leaf whose admitted poll outcome is unknown
    /// (`deferred.effect_pending`) is refused by [`DeferredResumeIntent::Poll`];
    /// only [`DeferredResumeIntent::ReplaceUnknownPoll`], an explicit user
    /// resume decision, may replace it with a new billable poll under fresh
    /// reserved ids. An admitted poll's `deferred.effect_pending` intent
    /// is durable before the provider is called and `run_resume` is emitted
    /// before the poll; the permit is consumed exactly once and a second call
    /// with the same pass or an older generation is refused without provider
    /// work. An admitted poll that fails records the accepted attempt as
    /// exposure, because its usage cannot be known and re-polling would be a
    /// second billable request for the same effect.
    pub async fn resume_deferred_run(
        &mut self,
        operation_id: &str,
        pass_id: impl Into<String>,
        intent: DeferredResumeIntent,
        source: &dyn DeferredPollSource,
    ) -> Result<DeferredRunOutcome, AgentError> {
        let pass_id = pass_id.into();
        let store = self.session.deferred_run_store();
        let now_ms = i64::try_from(now_unix_millis()).unwrap_or(i64::MAX);
        let start = store.begin_pass(operation_id, pass_id.clone(), intent, now_ms)?;
        match start {
            DeferredResumeStart::Unknown => {
                Err(DeferredRunError::UnknownOperation(operation_id.to_owned()).into())
            }
            DeferredResumeStart::Finished(record) => Ok(DeferredRunOutcome::Finished {
                operation_id: record.operation_id.clone(),
                state: record.state_label(),
            }),
            DeferredResumeStart::Waiting(observation) => {
                self.observe_deferred_boundary(
                    operation_id,
                    "deferred",
                    "suspended",
                    observation.poll,
                    0,
                    false,
                );
                Ok(DeferredRunOutcome::Waiting(*observation))
            }
            DeferredResumeStart::Refused(refusal) => {
                let stop_reason = refusal_stop_reason(&refusal);
                self.observe_deferred_boundary(operation_id, stop_reason, "failed", 0, 0, false);
                Ok(DeferredRunOutcome::Refused(refusal))
            }
            DeferredResumeStart::Admitted(poll) => {
                self.drive_admitted_deferred_poll(pass_id, *poll, source)
                    .await
            }
        }
    }

    async fn drive_admitted_deferred_poll(
        &mut self,
        pass_id: String,
        poll: AdmittedDeferredPoll,
        source: &dyn DeferredPollSource,
    ) -> Result<DeferredRunOutcome, AgentError> {
        let operation_id = poll.effect_pending.operation_id.clone();
        let generation = poll.intent.generation;
        let poll_number = poll.intent.poll;
        let recovery = poll.intent.discard_unknown_poll.is_some();
        // An abandoned unknown-outcome poll may already be accepted and billed;
        // its exposure is sticky and is never cleared by a later success.
        if recovery {
            self.record_deferred_exposure()?;
        }
        let resume = DeferredRunResumed {
            operation_id: operation_id.clone(),
            pass_id: pass_id.clone(),
            poll: poll_number,
            generation,
            recovery,
        };
        for observer in &self.extensions.observers {
            observer.on_run_resume(&resume);
        }
        self.observe_deferred_boundary(
            &operation_id,
            "deferred",
            "effect_pending",
            poll_number,
            generation,
            recovery,
        );
        // The transport permit is minted for the same unique pass and leaf
        // generation as the durable permit, and is consumed by the provider
        // call before any request is dispatched.
        let transport_permit = octet_ai::deferred::DeferredPollPermit::one(pass_id, generation);
        let reply = source
            .poll_deferred(&poll.intent.handle, transport_permit, generation)
            .await;
        let (outcome, settled) = match reply {
            DeferredPollReply::StillDeferred(handle) => {
                (DeferredPollOutcome::StillDeferred(handle), None)
            }
            DeferredPollReply::Settled(response) => (DeferredPollOutcome::Settled, Some(response)),
            DeferredPollReply::Failed(message) => (
                DeferredPollOutcome::Failed {
                    message: bounded_deferred_label(&message),
                },
                None,
            ),
            DeferredPollReply::Refused(message) => {
                // A refusal before dispatch is not a provider failure: nothing
                // was billed, no exposure is created, and the effect-pending
                // leaf stays replaceable for a later permitted pass.
                self.observe_deferred_boundary(
                    &operation_id,
                    "refused",
                    "effect_pending",
                    poll_number,
                    generation,
                    recovery,
                );
                return Ok(DeferredRunOutcome::PollRefused(bounded_deferred_label(
                    &message,
                )));
            }
        };
        let completion = self
            .session
            .deferred_run_store()
            .complete_pass(&poll, outcome)?;
        match completion {
            DeferredPollCompletion::Suspended(observation) => {
                self.observe_deferred_boundary(
                    &operation_id,
                    "deferred",
                    "suspended",
                    observation.poll,
                    0,
                    recovery,
                );
                Ok(DeferredRunOutcome::Suspended(observation))
            }
            DeferredPollCompletion::Settled {
                response_id,
                usage_id,
            } => {
                let Some(response) = settled else {
                    return Err(DeferredRunError::Corrupt(
                        "a settled poll requires the provider response".into(),
                    )
                    .into());
                };
                self.observe_deferred_boundary(
                    &operation_id,
                    "settled",
                    "settled",
                    poll_number,
                    generation,
                    recovery,
                );
                Ok(DeferredRunOutcome::Settled {
                    response,
                    response_id,
                    usage_id,
                })
            }
            DeferredPollCompletion::Failed(failure) => {
                // The admitted poll may have been accepted and billed; its
                // usage is unknown, so record exposure rather than a fabricated
                // cost or a second poll.
                self.record_deferred_exposure()?;
                self.observe_deferred_boundary(
                    &operation_id,
                    "failed",
                    "failed",
                    poll_number,
                    generation,
                    recovery,
                );
                Ok(DeferredRunOutcome::Failed(failure))
            }
        }
    }

    fn emit_run_suspend(&self, leaf: &crate::tools::deferred::DeferredSuspended) {
        let suspension = DeferredRunSuspended {
            operation_id: leaf.operation_id.clone(),
            source_entry_id: leaf.source_entry_id.clone(),
            stop_reason: crate::tools::deferred::DeferredStopReason::Deferred,
            handle: leaf.handle.clone(),
            poll: leaf.poll,
            generation: leaf.generation,
        };
        for observer in &self.extensions.observers {
            observer.on_run_suspend(&suspension);
        }
    }

    /// One sticky exposure record for an accepted deferred attempt whose usage
    /// cannot be known. Never invents usage or cost, and never clears earlier
    /// exposure.
    fn record_deferred_exposure(&mut self) -> Result<(), AgentError> {
        if self.session.has_uncertain_usage() {
            return Ok(());
        }
        self.session.record_usage_uncertainty(
            self.model.endpoint.id.clone(),
            self.model.spec.id.clone(),
            "deferred_poll",
        )?;
        Ok(())
    }

    fn observe_deferred_boundary(
        &self,
        operation_id: &str,
        stop_reason: &str,
        phase: &str,
        poll: u64,
        generation: u64,
        recovery: bool,
    ) {
        let _guard = self
            .telemetry
            .begin_typed::<DeferredRunSpan>(DeferredRunAttributes {
                operation_id: bounded_deferred_label(operation_id),
                stop_reason: stop_reason.to_owned(),
                phase: phase.to_owned(),
                poll,
                generation,
                recovery,
                diagnostics: 0,
            });
    }

    /// Enables durable partial-output checkpoints for live calls of `tool`.
    ///
    /// Row 4.7's harness half: while an invocation of the named tool is live, the
    /// run path republishes a bounded, complete, replaceable snapshot of the
    /// output it has streamed so far to `sink`, at `interval` cadence
    /// ([`BashCheckpointPublisher`] policy: first observation immediate, at most
    /// one publication per interval, identical snapshots suppressed, snapshot
    /// capped at [`BASH_CHECKPOINT_MAX_BYTES`] with the newest bytes kept on a
    /// code-point boundary). The name is matched exactly against the executing
    /// tool's name; an unmatched name costs checkpoints and nothing else.
    ///
    /// A checkpoint is auxiliary observation data: this external-sink variant
    /// does not itself persist it in the session or turn it into a result, and
    /// never claims the command finished (no `complete_<stream>=true`). Nothing
    /// is published before the call starts or after its result is committed, so a
    /// settled invocation is never republished as progress. A host that instead
    /// constructs its own checkpointing tool (for example
    /// [`CheckpointedBashTool`](crate::tools::bash::CheckpointedBashTool))
    /// should not also enable this, so no invocation is published twice.
    ///
    /// Disabled by default: an unopted host publishes no progress snapshots.
    /// Invocation intent and memo records are independent of this opt-in.
    #[cfg(any(unix, windows))]
    pub fn enable_partial_output_checkpoints(
        &mut self,
        tool: impl Into<String>,
        sink: Arc<dyn PartialOutputCheckpointSink>,
        interval: Duration,
    ) {
        self.partial_output_checkpoints = Some(PartialOutputCheckpointConfig {
            tool: tool.into(),
            sink: Some(sink),
            interval,
            totals: Arc::new(PartialOutputCheckpointTotals::default()),
        });
    }

    /// Enables per-invocation checkpoints backed by this agent's private
    /// synced session log. No external sink or process-local fixture is used.
    /// Replay preserves the last bounded snapshot only as auxiliary data;
    /// paired-result persistence atomically fences and clears its live value.
    #[cfg(any(unix, windows))]
    pub fn enable_session_partial_output_checkpoints(
        &mut self,
        tool: impl Into<String>,
        interval: Duration,
    ) {
        self.partial_output_checkpoints = Some(PartialOutputCheckpointConfig {
            tool: tool.into(),
            sink: None,
            interval,
            totals: Arc::new(PartialOutputCheckpointTotals::default()),
        });
    }

    /// Enables checkpoints for `tool` at Pi's bash cadence.
    #[cfg(any(unix, windows))]
    pub fn enable_default_partial_output_checkpoints(
        &mut self,
        tool: impl Into<String>,
        sink: Arc<dyn PartialOutputCheckpointSink>,
    ) {
        self.enable_partial_output_checkpoints(tool, sink, BASH_CHECKPOINT_INTERVAL);
    }

    /// Disables partial-output checkpoints and drops their counters.
    #[cfg(any(unix, windows))]
    pub fn disable_partial_output_checkpoints(&mut self) {
        self.partial_output_checkpoints = None;
    }

    /// Publication counters of the enabled checkpoint consumer, if any.
    #[cfg(any(unix, windows))]
    pub fn partial_output_checkpoint_stats(&self) -> Option<PartialOutputCheckpointStats> {
        self.partial_output_checkpoints
            .as_ref()
            .map(|config| config.totals.stats())
    }

    /// Enables or disables the model-visible "available tools" section.
    ///
    /// The section is rendered from [`collect_tool_prompt_contributions`] over
    /// the tools that are registered when a run starts, so the snippet and
    /// guidelines a host reads are exactly the ones the executing tool
    /// declares. It is appended to the run's system prompt before the first
    /// request and stays fixed for that run — the provider prefix never changes
    /// mid-run — and is bounded in bytes and in tool count, so no registration
    /// can widen a prompt without limit.
    ///
    /// Disabled by default: a host that owns its own prompt assembly keeps a
    /// byte-identical system prompt until it opts in. A run that exposes no
    /// tools (for example [`Agent::prompt_without_tools`]) never carries the
    /// section, so a tool-free run cannot advertise tools. An answer-only turn
    /// inside a tool-bearing run withholds the tool schemas from that request
    /// while the run prefix stays stable; that is the same contract a host
    /// system prompt that names its tools already has.
    pub fn set_tool_prompt_section_enabled(&mut self, enabled: bool) {
        self.tool_prompt_section = enabled;
    }

    /// Whether the model-visible tool section is enabled.
    pub fn tool_prompt_section_enabled(&self) -> bool {
        self.tool_prompt_section
    }

    /// Prompt contributions of the currently registered tools, in wire order.
    ///
    /// Presentation only: it is the same collection the run uses to build the
    /// opt-in model-visible tool section, exposed so a host can render its own
    /// prompt from the tools that will actually execute.
    pub fn tool_prompt_contributions(&self) -> Vec<ToolPromptContribution> {
        let (_, tools) = self.extensions.tool_snapshot();
        collect_tool_prompt_contributions(tools.iter().map(|tool| tool.as_ref()))
    }

    /// The system prompt this agent's next model request begins from.
    ///
    /// When the tool section is enabled and `tools_enabled` holds, the
    /// registered tools' bounded snippet/guideline section is appended. The
    /// result is deterministic for a given registration and system prompt, and
    /// is what both the live run and the idle context estimates report.
    fn model_visible_system(&self, tools_enabled: bool) -> String {
        if !self.tool_prompt_section || !tools_enabled {
            return self.system.clone();
        }
        let (_, tools) = self.extensions.tool_snapshot();
        let section = render_tool_prompt_section(tools.iter().map(|tool| tool.as_ref()));
        match section {
            None => self.system.clone(),
            Some(section) if self.system.is_empty() => section,
            Some(section) => format!("{}\n\n{section}", self.system),
        }
    }

    /// Replaces the durable active session at an idle boundary.
    ///
    /// Active V2 delegation owns session-scoped child resources, so callers
    /// must rebuild that runtime instead of retargeting it in place.
    pub fn replace_session_at_idle(&mut self, session: Session) -> Result<(), AgentError> {
        if self.delegation.is_some() {
            return Err(AgentError::Delegation(
                "active session changes require a rebuild while V2 delegation is enabled".into(),
            ));
        }
        if self
            .last_run_lifecycle
            .as_ref()
            .is_some_and(|lifecycle| lifecycle.dropped.load(Ordering::Acquire))
        {
            // Match `Drop`: preserve an interrupted old session before its
            // owner is replaced.
            persist_pending_cancellations(&mut self.session)?;
        }
        let resource_owner = session.resource_owner_key();
        self.bash_owner = BashOwnerLease::acquire(&resource_owner);
        self.session_id = resource_owner.clone();
        self.resource_owner = resource_owner;
        self.session = session;
        self.last_run_lifecycle = None;
        // This is a one-shot transcript projection for a future prompt in the
        // old session, not a durable cross-session setting.
        self.prompt_display_text = None;
        Ok(())
    }

    /// Mutable access to the session for history operations between runs
    /// (checkout, manual compaction, config entries).
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Selects completion behavior for subsequent runs.
    pub fn set_completion_policy(&mut self, policy: CompletionPolicy) {
        self.completion_policy = policy;
        self.sync_delegation_runtime_settings();
    }

    /// Returns the selected completion policy.
    pub fn completion_policy(&self) -> CompletionPolicy {
        self.completion_policy
    }

    /// Provider schemas for all currently executable tools, in wire order.
    pub fn registered_tool_definitions(&self) -> Vec<ToolDef> {
        self.extensions.tool_definitions()
    }

    /// Exact host-policed registered tool names, sorted.
    ///
    /// The result lists every name registered after the frontend has applied
    /// all policy filters and extension registration. It still includes names
    /// that [`set_active_tool_names`](Self::set_active_tool_names) has
    /// deactivated, so it is the stable validation surface for active-tool
    /// requests; use
    /// [`registered_tool_definitions`](Self::registered_tool_definitions) for
    /// the exact schemas the next provider request would advertise.
    pub fn registered_tool_names(&self) -> Vec<String> {
        self.extensions.policed_tool_names()
    }

    /// Narrows the host-policed tool surface this agent advertises and
    /// executes to exactly `names`.
    ///
    /// `Some(set)` activates only the requested registered names. Unknown or
    /// already policy-excluded names fail with
    /// [`AgentError::UnknownActiveTools`] and change nothing; `None` restores
    /// the full host-policed surface; `Some(empty)` is valid and leaves the
    /// agent with no callable tools.
    ///
    /// Activation strictly narrows: it can never add a tool, re-admit a name
    /// the sandbox, effect broker, or frontend policy excluded, or widen what
    /// an admitted tool may do. Every accepted call bumps the host
    /// tool-snapshot revision, so an in-flight run drops deactivated schemas
    /// and refuses calls to deactivated tools with the existing `unknown tool`
    /// result at its next turn boundary.
    pub fn set_active_tool_names(
        &mut self,
        names: Option<BTreeSet<String>>,
    ) -> Result<(), AgentError> {
        if let Some(names) = names.as_ref() {
            let registered = self.registered_tool_names();
            let mut refused = names
                .iter()
                .filter(|name| !registered.iter().any(|registered| registered == *name))
                .cloned()
                .collect::<Vec<_>>();
            if !refused.is_empty() {
                refused.truncate(MAX_REFUSED_ACTIVE_TOOL_NAMES);
                return Err(AgentError::UnknownActiveTools(refused));
            }
        }
        self.extensions
            .set_active_tools(names.as_ref())
            .map_err(AgentError::ActiveToolSetRefused)
    }

    /// Reconciles unresolved calls from the latest persisted assistant turn.
    ///
    /// Only tools explicitly marked [`ReplaySafety::Safe`] execute again.
    /// Every other call receives a durable indeterminate error, preserving
    /// provider call/result pairing without silently duplicating an external
    /// mutation after a process crash.
    async fn recover_pending_tools(
        &mut self,
        previous_run_was_dropped: bool,
    ) -> Result<(), AgentError> {
        let Some((calls, persisted)) = pending_tool_state(&self.session) else {
            return Ok(());
        };
        // Keep each call's original assistant-turn index. Filtering first
        // would renumber unresolved calls and let crash recovery execute calls
        // that the live path would have skipped after the per-turn limit.
        let unresolved: Vec<(usize, ToolCall)> = calls
            .into_iter()
            .enumerate()
            .filter(|(_, call)| !persisted.contains(&call.id))
            .collect();
        if unresolved.is_empty() {
            return Ok(());
        }

        if previous_run_was_dropped {
            persist_pending_cancellations(&mut self.session)?;
            return Ok(());
        }

        let (tool_generation, tools) = self.extensions.tool_snapshot();
        let mut tool_map: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        for tool in &tools {
            let definition = tool.definition();
            tool_map.insert(definition.name, Arc::clone(tool));
        }
        let mut registered_tools = tool_map.keys().cloned().collect::<Vec<_>>();
        registered_tools.sort();
        let sandbox = self.sandbox.clone();
        let tool_scope = self.tool_scope.clone();
        let resource_owner = self.resource_owner.clone();
        let recovery_run_id = format!("{tool_scope}:recovery");
        let effect_broker = self.effect_broker.clone();
        let tool_call_hooks = self.extensions.tool_call_hooks.clone();
        for (call_index, call) in unresolved {
            if let Some((message, metadata)) =
                self.session.persisted_invocation_result(call_index)?
            {
                self.session
                    .append_with_metadata(EntryValue::Message(Message::User(message)), metadata)?;
                continue;
            }
            let result = if let Some(argument_error) = call.argument_error {
                // A schema-rejected call was never admitted for execution in
                // the live path; retain that fact across a restart as well.
                Err(rejected_argument_tool_error(argument_error))
            } else if call.async_execution {
                Err(ToolError::new(
                    "indeterminate background call after restart; not automatically replayed",
                ))
            } else if call_index >= MAX_TOOL_CALLS_PER_TURN {
                Err(ToolError::new(
                    "tool call skipped: per-turn tool-call limit reached",
                ))
            } else {
                let partial_output = self.session.invocation_partial_output(call_index)?;
                match tool_map.get(&call.name) {
                    None => Err(ToolError::new(format!(
                        "unknown tool: {}\n{}",
                        call.name,
                        synthesize_interruption(partial_output.as_deref()).text
                    ))),
                    Some(tool)
                        if !call.async_execution && tool.replay_safety() == ReplaySafety::Safe =>
                    {
                        execute_recovery_call(
                            call_index,
                            Arc::clone(tool),
                            &tool_call_hooks,
                            &effect_broker,
                            tool_generation,
                            &recovery_run_id,
                            &call,
                            &sandbox,
                            &tool_scope,
                            &resource_owner,
                            &registered_tools,
                            &mut self.session,
                        )
                        .await?
                    }
                    Some(_) => Err(ToolError::new(format!(
                        "indeterminate after restart: `{}` was not replayed.\n{}",
                        call.name,
                        synthesize_interruption(partial_output.as_deref()).text
                    ))),
                }
            };
            let (message, _, _, _, details) = lower_tool_result(
                call.id,
                &result,
                &self.model,
                sandbox.max_output_bytes,
                Vec::new(),
            );
            self.session.append_with_metadata(
                EntryValue::Message(Message::User(message)),
                details.map(|tool_output| EntryMetadata {
                    tool_output: Some(tool_output),
                    ..EntryMetadata::default()
                }),
            )?;
            resolve_tool_delivery_after_persistence(&result, sandbox.max_output_bytes);
        }
        Ok(())
    }

    /// Effective host-selected reasoning, distinct from a pinned request baseline.
    pub fn reasoning(&self) -> &ReasoningConfig {
        &self.reasoning
    }

    /// Changes reasoning on an idle agent, preserving qualified Responses caches.
    pub fn set_reasoning(&mut self, reasoning: ReasoningConfig) -> Result<(), AgentError> {
        require_ultra_observation(
            &reasoning,
            self.delegation.is_some() || self.ultra_observation_managed,
        )?;
        if self.model.responses_features().reasoning_effort_updates {
            persist_reasoning_selection(&mut self.session, &self.model, &reasoning)?;
        } else {
            octet_ai::responses::validate_responses_input(
                &self.model,
                &ResponsesInput::default(),
                &reasoning,
                false,
            )?;
        }
        self.reasoning = reasoning;
        if let Some(binding) = &self.delegation {
            binding.update_reasoning(self.reasoning.clone());
        }
        Ok(())
    }

    /// Begins a run: appends the user message to the session and returns the
    /// caller-driven event stream plus its control handle.
    ///
    /// Pre-flight failures (e.g. the session append) are returned here; once
    /// the run has started every terminal outcome — completed, aborted,
    /// failed, or max-turns — is reported by exactly one
    /// [`AgentEvent::RunFinished`].
    pub async fn prompt(&mut self, input: impl Into<UserInput>) -> Result<Run<'_>, AgentError> {
        self.prompt_with_tools(input.into(), true).await
    }

    /// Begins a run whose provider requests expose no tools. This is used for
    /// explicit answer-now flows that must synthesize from existing evidence.
    pub async fn prompt_without_tools(
        &mut self,
        input: impl Into<UserInput>,
    ) -> Result<Run<'_>, AgentError> {
        self.prompt_with_tools(input.into(), false).await
    }

    async fn prompt_with_tools(
        &mut self,
        input: UserInput,
        tools_enabled: bool,
    ) -> Result<Run<'_>, AgentError> {
        if self.reasoning == octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)
            && self.delegation.is_none()
            && !self.ultra_observation_managed
        {
            return Err(AgentError::Delegation(
                "Ultra requires an enabled child-session observation runtime".into(),
            ));
        }
        if self.session.has_unsettled_native_steering() {
            return Err(AiError::Config(octet_ai::ConfigError::Parse("unresolved native steering intent; automatic replay is prohibited; use a new session".into())).into());
        }
        // Direct library callers may not have an explicit construction
        // boundary. Keep this idempotent fallback so their first owning run
        // cannot leave dynamic publishers waiting forever.
        self.extensions.finalize_tool_surface();
        // A previous process may have died after persisting an assistant tool
        // call but before persisting its result. Repair that semantic boundary
        // before appending a new user message; otherwise strict provider
        // validation would reject the resumed conversation as malformed.
        let previous_run_was_dropped = self
            .last_run_lifecycle
            .take()
            .is_some_and(|lifecycle| lifecycle.dropped.load(Ordering::Acquire));
        self.recover_pending_tools(previous_run_was_dropped).await?;
        // This snapshot is both the preflight boundary and the first provider
        // request's frozen tool surface. Refusing it before the prompt append
        // leaves a frontend free to revise and retry the same draft.
        let (initial_tool_revision, initial_tools) = self.extensions.tool_snapshot();
        let initial_tool_defs: Vec<ToolDef> = if tools_enabled {
            initial_tools
                .iter()
                .map(|tool| advertised_tool_definition(tool.as_ref(), &self.model))
                .collect()
        } else {
            Vec::new()
        };
        require_tool_schema_budget(&initial_tool_defs, self.tool_schema_budget_bytes)?;
        let completion_policy = self.completion_policy;
        let mut terminal_gate_evidence =
            TerminalGateEvidence::for_run(completion_policy, &self.session, &input)?;
        let input = prepare_user_images(input, &self.model, None).await?;
        let prompt_metadata = self.prompt_entry_metadata();
        // `display_text` belongs only to the draft that started this run.
        // Steering and follow-up inputs are independent user submissions and
        // must render their own durable message bodies after replay.
        let control_prompt_metadata = EntryMetadata {
            display_text: None,
            ..prompt_metadata.clone()
        };
        let observer_input = (!self.extensions.observers.is_empty()).then(|| input.clone());
        if self.model.responses_features().reasoning_effort_updates {
            persist_reasoning_selection(&mut self.session, &self.model, &self.reasoning)?;
        }
        let first_entry = self
            .session
            .append_with_metadata(user_message(input), Some(prompt_metadata.clone()))?;
        if let Some(input) = observer_input.as_ref() {
            for observer in &self.extensions.observers {
                observer.on_run_started_for_owner(
                    &first_entry.0,
                    input,
                    &self.model,
                    &self.resource_owner,
                );
            }
        }
        let lifecycle = Arc::new(RunLifecycle {
            finished: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
        });
        self.last_run_lifecycle = Some(lifecycle.clone());
        let context = Arc::new(ContextTracker::default());
        let stream_context = context.clone();

        let (control_tx, mut control_rx) = mpsc::channel::<Control>(8);
        let abort = Arc::new(AbortFlag::default());
        let control_admission = Arc::new(std::sync::Mutex::new(true));
        let control = RunControl {
            reasoning_model: self
                .model
                .responses_features()
                .reasoning_effort_updates
                .then(|| self.model.clone()),
            ultra_observed: self.delegation.is_some() || self.ultra_observation_managed,
            admission: control_admission.clone(),
            tx: control_tx,
            pending_count: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONTROL_INPUTS)),
            pending_bytes: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONTROL_BYTES)),
            abort: abort.clone(),
        };

        // Disjoint borrows: the run stream owns clones of everything except
        // the session, which it borrows mutably for the run's lifetime —
        // preserving one authoritative head.
        let client = self.client.clone();
        let model = self.model.clone();
        let compaction_model = self
            .compaction_model
            .clone()
            .unwrap_or_else(|| model.clone());
        let system = self.model_visible_system(tools_enabled);
        let sandbox = self.sandbox.clone();
        let extension_host = self.extensions.clone();
        let initial_context =
            observe_context_tracker(&context, &self.session, &model, &system, &initial_tool_defs)?;
        let initial_capacity =
            ContextCapacityCache::seeded(&self.session, initial_tool_revision, &initial_context);
        if let Some(delegation) = &self.delegation {
            delegation.prepare_owning_run()?;
        }
        let observers = ObserverDispatch {
            observers: self.extensions.observers.clone(),
            resource_owner: self.resource_owner.clone(),
        };
        let tool_call_hooks = self.extensions.tool_call_hooks.clone();
        let provider_retry_hooks = self.extensions.provider_retry_hooks.clone();
        let persistence_metadata_hooks = self.extensions.persistence_metadata_hooks.clone();
        let max_turns = self.max_turns;
        let mut reasoning = self.reasoning.clone();
        let reasoning_mode = self.reasoning_mode;
        let cache_retention = self.cache_retention;
        let session_id = self.session_id.clone();
        let resource_owner = self.resource_owner.clone();
        let tool_scope = self.tool_scope.clone();
        let effect_broker = self.effect_broker.clone();
        let effect_run_id = format!("run:{}", first_entry.0);
        let output_modalities = self.output_modalities.clone();
        let provider_output_ceiling = self.max_output_tokens;
        let compaction_reserve_tokens = self.compaction_reserve_tokens();
        let effective_reasoning = &mut self.reasoning;
        let max_session_tokens = self.max_session_tokens;
        let max_session_cost_microdollars = self.max_session_cost_microdollars;
        let tool_schema_budget_bytes = self.tool_schema_budget_bytes;
        let auto_compaction_mode = if self.model.responses_features().reasoning_effort_updates
            && self.auto_compaction_mode == AgentCompactionMode::NativeResponses
        {
            AgentCompactionMode::Local
        } else {
            self.auto_compaction_mode
        };
        // The caller-selected provider service tier rides on every Responses
        // request this run builds; the builder re-checks the route capability.
        let service_tier = self.service_tier;
        let compaction_threshold_fraction = self.compaction_threshold_fraction;
        let compaction_keep_recent_tokens = self.compaction_keep_recent_tokens;
        let provider_retries_enabled = self.provider_retries_enabled;
        let max_network_wait = self.max_network_wait;
        let owner_tool_images_enabled = self.owner_tool_images_enabled;
        let stream_delegation = self.delegation.clone();
        // Row 4.7: the host's opt-in, captured before the session borrow, so the
        // run can publish live partial-output checkpoints without touching the
        // session log.
        #[cfg(any(unix, windows))]
        let partial_output_checkpoints = self.partial_output_checkpoints.clone();
        let run_delegation = self.delegation.clone();
        let mut delegation_telemetry = self
            .delegation
            .as_ref()
            .and_then(DelegationBinding::telemetry_receiver);
        let stream_lifecycle = lifecycle.clone();
        let telemetry = self.telemetry.clone();
        let session = &mut self.session;

        let stream = async_stream::stream! {
            // This guard owns the mutable session borrow for exactly as long as
            // the generated stream. If the caller drops the stream at any
            // suspension point, its Drop implementation durably pairs pending
            // tool calls before `Run::drop` returns.
            let mut session_guard = RunSessionGuard {
                session,
                lifecycle: stream_lifecycle.clone(),
            };
            let session = &mut *session_guard;
            let mut context_capacity = initial_capacity;
            // Parity 1e.2 durability half: republish the partial assistant
            // turn a killed stream left behind. The frame journal is consumed
            // exactly once and only its user-visible text/reasoning progress is
            // re-emitted; a partial tool call is never a result. A journal
            // removed at terminal settlement yields nothing here, so a
            // completed turn is never replayed as progress.
            match session.take_partial_assistant() {
                Ok(Some(partial)) => {
                    for part in partial.content {
                        let (channel, text) = match part {
                            AssistantPart::Text(text) => (OutputChannel::Text, text),
                            AssistantPart::Reasoning(reasoning) => (
                                OutputChannel::Reasoning,
                                reasoning.text.unwrap_or_default(),
                            ),
                            AssistantPart::ToolCall(_)
                            | AssistantPart::Media(_)
                            | AssistantPart::ProviderMetadata(_) => continue,
                        };
                        if text.is_empty() {
                            continue;
                        }
                        let ev = AgentEvent::RecoveredOutput { channel, text };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }
                }
                Ok(None) => {}
                // A recovery aid must never fail the run it observes.
                Err(_) => {}
            }
            // Row 3.5: one run span owns the generated stream's lifetime. Its
            // children (turns) are derived only from this explicit context, and
            // the guard is settled explicitly at the durable run boundary.
            let run_guard = telemetry.begin_typed::<RunSpan>(EmptyAttributes {});
            let run_context = run_guard.context();

            let mut tool_revision = initial_tool_revision;
            let tools = initial_tools;
            let mut tool_defs = initial_tool_defs;
            let mut tool_map: HashMap<String, Arc<dyn Tool>> =
                HashMap::with_capacity(if tools_enabled { tools.len() } else { 0 });
            if tools_enabled {
                for tool in &tools {
                    let definition = tool.definition();
                    tool_map.insert(definition.name, Arc::clone(tool));
                }
            }
            let mut registered_tools = tool_map.keys().cloned().collect::<Vec<_>>();
            registered_tools.sort();
            // Tool names already visible to the provider either as static
            // schemas or via an earlier `added_tool_names` announcement.
            let mut announced_tools: std::collections::HashSet<String> =
                registered_tools.iter().cloned().collect();

            let mut native = native_steering::NativeState::default();
            let (native_updates_tx, mut native_updates_rx) = mpsc::channel(128);
            let native_enabled = model.responses_features().steering
                && model.endpoint.transport == octet_ai::EndpointTransport::WebSocketPreferred
                && max_session_tokens.is_none() && max_session_cost_microdollars.is_none();
            let mut background_tools = background_tools::BackgroundTools::default();
            let background_cancellation = abort.cancellation.clone();
            let mut pending_reasoning = None;
            let mut pending_steer: Vec<ReservedInput> = Vec::new();
            let mut followups: VecDeque<ReservedInput> = VecDeque::new();
            // Preserve octet's historical defaults; frontends that expose queue
            // modes can update either mode through RunControl.
            let mut steering_mode = QueueDeliveryMode::All;
            let mut follow_up_mode = QueueDeliveryMode::OneAtATime;
            let mut control_open = true;
            let mut answer_only = !tools_enabled;
            let mut finish_pending = false;
            let mut completed_turns: u64 = 0;
            let mut context_retries = 0usize;
            // Shared by open/body retries, re-preparation and transport fallback.
            // Reset only on a complete successful assistant response.
            let mut stream_retries = 0usize;
            let mut recovery_budget = ProviderRecoveryBudget::default();
            let mut network_retries = 0usize;
            let mut network_deadline = None;
            let mut failed_usage_unknown = session.has_uncertain_usage();
            if failed_usage_unknown {
                let event = AgentEvent::ProviderUsageUncertain;
                notify_observers(&observers, &event);
                yield event;
            }
            let mut pending_recovery: Option<PendingProviderRecovery> = None;
            let mut run_usage = Usage::default();
            let mut run_cost = CostAccumulator::default();
            let mut recent_tool_calls: VecDeque<(String, String)> =
                VecDeque::with_capacity(MAX_RECENT_TOOL_CALLS);

            // Row 3.5 boundary state: the live turn guard and its derived child
            // context, plus the per-attempt outcome that decides whether the
            // previous turn settled as a completed or a failed attempt.
            let mut previous_turn: Option<SpanGuard> = None;
            let mut turn_attempt_opened = false;
            let mut turn_attempt_succeeded = false;

            let mut reason: FinishReason = 'run: loop {
                // Row 3.5: an iteration is one turn boundary. The previous
                // turn is settled here (a `continue 'run` continuation is a
                // completed turn, not an error) and the current one begins.
                // A turn that opened a provider attempt without a finished
                // response is reported as an error attempt.
                if let Some(settled) = previous_turn.take() {
                    settled.finish(turn_attempt_opened && !turn_attempt_succeeded);
                }
                turn_attempt_opened = false;
                turn_attempt_succeeded = false;
                let turn_guard = run_context.begin_typed::<TurnSpan>(EmptyAttributes {});
                let turn_context = turn_guard.context();
                previous_turn = Some(turn_guard);
                if let Some(recovery) = pending_recovery.take() {
                    if recovery.usage_unknown() {
                        let first = !session.has_uncertain_usage();
                        if let Err(error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "assistant_turn") {
                            if first {
                                let event = AgentEvent::ProviderUsageUncertain;
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            break 'run FinishReason::Failed(error.into());
                        }
                        if first {
                            let event = AgentEvent::ProviderUsageUncertain;
                            notify_observers(&observers, &event);
                            yield event;
                        }
                    }
                    failed_usage_unknown |= recovery.usage_unknown();
                    let waiting_for_network = recovery.waiting_for_network();
                    if waiting_for_network && network_deadline.is_none() {
                        network_deadline = max_network_wait.and_then(|limit| tokio::time::Instant::now().checked_add(limit));
                    }
                    let retry_limit = recovery_budget.limit(stream_retries, &recovery);
                    let hard_budget = max_session_tokens.is_some()
                        || max_session_cost_microdollars.is_some();
                    let eligible = provider_retries_enabled
                        && (waiting_for_network || stream_retries < retry_limit)
                        && !(hard_budget && failed_usage_unknown);
                    let host_delay = if waiting_for_network {
                        network_wait_delay(&effect_run_id, network_retries)
                    } else {
                        retry_after(&recovery.error, stream_retries)
                    };
                    let decision = if eligible {
                        let decision_future = provider_retry_decision(ProviderRetryRequest {
                            hooks: &provider_retry_hooks,
                            context: ProviderRetryContext {
                                operation: None,
                                run_id: effect_run_id.clone(),
                                resource_owner: resource_owner.clone(),
                                attempt: if waiting_for_network { network_retries } else { stream_retries }.saturating_add(1),
                                max_attempts: (!waiting_for_network).then_some(retry_limit),
                                host_delay,
                                kind: if waiting_for_network {
                                    ProviderRetryKind::WaitingForNetwork
                                } else if recovery.qualified && interrupted_inference_error(&recovery.error) {
                                    ProviderRetryKind::InterruptedInference
                                } else if recovery.opened {
                                    ProviderRetryKind::StreamStart
                                } else {
                                    ProviderRetryKind::BeforeGeneration
                                },
                            },
                            abort: &abort,
                        });
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break 'run FinishReason::Aborted,
                            _ = wait_network_deadline(network_deadline) => break 'run FinishReason::Failed(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: failed_usage_unknown }),
                            decision = decision_future => decision,
                        }
                    } else {
                        ProviderRetryDecision { proceed: false, additional_delay: Duration::ZERO }
                    };
                    if abort.is_set() {
                        break 'run FinishReason::Aborted;
                    }
                    if !decision.proceed {
                        let error = if (failed_usage_unknown && hard_budget)
                            || (stream_retries > 0 && !is_replayable_network_failure(&recovery.error)) {
                            AgentError::ProviderRecovery {
                                retries: stream_retries,
                                usage_unknown: failed_usage_unknown,
                                source: recovery.error,
                            }
                        } else {
                            provider_failure(recovery.error, stream_retries)
                        };
                        break 'run FinishReason::Failed(error);
                    }
                    if waiting_for_network {
                        network_retries = network_retries.saturating_add(1);
                    } else {
                        recovery_budget.admit(&recovery);
                        stream_retries += 1;
                    }
                    let delay = host_delay.saturating_add(decision.additional_delay);
                    let mut diagnostic = provider_retry_diagnostic(&model, &recovery.error);
                    if failed_usage_unknown {
                        diagnostic = format!("failed_usage=unknown {diagnostic}");
                        truncate_public_diagnostic(&mut diagnostic);
                    }
                    stream_context.provider_retry();
                    let ev = if waiting_for_network {
                        AgentEvent::ProviderWaitingForNetwork {
                            attempt: network_retries, delay, error: diagnostic,
                        }
                    } else {
                        AgentEvent::ProviderRetry {
                            attempt: stream_retries, max_attempts: retry_limit,
                            delay, error: diagnostic,
                        }
                    };
                    notify_observers(&observers, &ev);
                    yield ev;
                    let wait = tokio::time::sleep(delay);
                    tokio::pin!(wait);
                    loop {
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break 'run FinishReason::Aborted,
                            _ = wait_network_deadline(network_deadline) => break 'run FinishReason::Failed(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: failed_usage_unknown }),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                    context_capacity.invalidate();
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => break 'run FinishReason::Aborted,
                                None => control_open = false,
                            },
                            _ = &mut wait => break,
                        }
                    }
                    // Resume the ordinary safe preparation boundary, not a
                    // stale clone: steering, FinishNow and tool snapshots agree.
                }

                // ── Drain control at the turn boundary ─────────────────────
                while control_open {
                    match control_rx.try_recv() {
                        Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                        Ok(Control::FollowUp(input)) => followups.push_back(input),
                        Ok(Control::FinishNow(input)) => {
                            input.push_pending(&mut pending_steer);
                            answer_only = true;
                            finish_pending = true;
                            context_capacity.invalidate();
                        }
                        Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                        Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                        Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                        Ok(Control::Abort) => break 'run FinishReason::Aborted,
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                    }
                }
                if abort.is_set() {
                    break 'run FinishReason::Aborted;
                }

                if let Some(selection) = if native.connection.is_none() { pending_reasoning.take() } else { None } {
                    if let Err(error) = persist_reasoning_selection(session, &model, &selection) {
                        break 'run FinishReason::Failed(error);
                    }
                    reasoning = selection.clone();
                    if let Some(binding) = &stream_delegation {
                        binding.update_reasoning(selection.clone());
                    }
                    *effective_reasoning = selection;
                    context_capacity.invalidate();
                }

                // ── Steering enters here, at the model-turn boundary ───────
                pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                    let queued = if std::mem::take(&mut finish_pending) {
                        std::mem::take(&mut pending_steer)
                    } else {
                        match steering_mode {
                            QueueDeliveryMode::All => std::mem::take(&mut pending_steer),
                            QueueDeliveryMode::OneAtATime => vec![pending_steer.remove(0)],
                        }
                    };
                    let visible_tools = if answer_only {
                        &[][..]
                    } else {
                        tool_defs.as_slice()
                    };
                    let observation = ContextObservation {
                        tracker: &stream_context,
                        model: &model,
                        system: &system,
                        tools: visible_tools,
                    };
                    match deliver_control_inputs(
                        queued,
                        ControlDeliveryKind::Steering,
                        session,
                        &control_prompt_metadata,
                        &mut terminal_gate_evidence,
                        &observation,
                        Some(&abort),
                    ).await {
                        ControlDelivery::Completed { event } => {
                            if let Some(ev) = event {
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                        }
                        ControlDelivery::Interrupted { event, finish } => {
                            if let Some(ev) = event {
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            break 'run finish;
                        }
                    }
                }

                // ── Turn guard ─────────────────────────────────────────────
                if let Some(limit) = max_turns {
                    if completed_turns >= limit {
                        break 'run FinishReason::MaxTurns;
                    }
                }

                // Freeze one coherent schema/implementation snapshot after
                // control and steering have settled but before context sizing.
                // Every call emitted by this request resolves against exactly
                // the tool set the provider saw.
                let (current_revision, current_tools) = extension_host.tool_snapshot();
                if current_revision != tool_revision {
                    tool_revision = current_revision;
                    if tools_enabled && !answer_only {
                        let next_tool_defs: Vec<ToolDef> = current_tools
                            .iter()
                            .map(|tool| advertised_tool_definition(tool.as_ref(), &model))
                            .collect();
                        if let Err(error) = require_tool_schema_budget(
                            &next_tool_defs,
                            tool_schema_budget_bytes,
                        ) {
                            break 'run FinishReason::Failed(error);
                        }
                        tool_defs = next_tool_defs;
                        tool_map.clear();
                        tool_map.reserve(current_tools.len());
                        for tool in &current_tools {
                            let definition = tool.definition();
                            tool_map.insert(definition.name, Arc::clone(tool));
                        }
                        registered_tools = tool_map.keys().cloned().collect();
                        registered_tools.sort();
                        announced_tools.extend(registered_tools.iter().cloned());
                    }
                }
                let request_tool_defs = if answer_only {
                    Vec::new()
                } else {
                    tool_defs.clone()
                };

                // ── Reconstruct and size context for this exact turn ───────
                // This gate is inside the autonomous loop, after every tool
                // result, and uses the exact active tool schema set.
                let (compaction_event_tx, mut compaction_event_rx) =
                    mpsc::unbounded_channel::<AgentEvent>();
                let capacity = {
                    let mut compaction = CompactionContext {
                        run_id: &effect_run_id,
                        resource_owner: &resource_owner,
                        retry_hooks: &provider_retry_hooks,
                        compaction_strategy: extension_host.compaction_strategy.as_ref(),
                        max_network_wait,
                        provider_retries_enabled,
                        client: &client,
                        model: &model,
                        compaction_model: &compaction_model,
                        summary_operation: crate::events::ProviderOperation::LocalCompaction,
                        session,
                        usage: &mut run_usage,
                        run_cost: &mut run_cost,
                        cache_retention,
                        reasoning: &reasoning,
                        reasoning_mode,
                        session_id: &session_id,
                        max_session_tokens,
                        max_session_cost_microdollars,
                        abort: &abort,
                        mode: if background_tools.is_empty() && native.connection.is_none() { auto_compaction_mode } else { AgentCompactionMode::Disabled },
                        threshold_fraction: compaction_threshold_fraction,
                        keep_recent_tokens: compaction_keep_recent_tokens,
                        events: &compaction_event_tx,
                        context: &stream_context,
                        tool_generation: tool_revision,
                        capacity: &mut context_capacity,
                        telemetry: turn_context.clone(),
                    };
                    let operation = compaction.ensure_capacity(
                        &system,
                        &request_tool_defs,
                        compaction_reserve_tokens,
                        provider_output_ceiling,
                    );
                    tokio::pin!(operation);
                    let result = loop {
                        tokio::select! {
                            biased;
                            _ = abort.wait() => break Err(AgentError::Cancelled),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); }
                                None => control_open = false,
                            },
                            Some(event) = compaction_event_rx.recv() => {
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            result = &mut operation => break result,
                        }
                    };
                    while let Ok(event) = compaction_event_rx.try_recv() {
                        notify_observers(&observers, &event);
                        yield event;
                    }
                    result
                };
                let capacity = match capacity {
                    Ok(capacity) => capacity,
                    Err(error) => {
                        break 'run if matches!(&error, AgentError::Cancelled) {
                            FinishReason::Aborted
                        } else {
                            FinishReason::Failed(error)
                        };
                    }
                };
                pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                    context_capacity.invalidate();
                    continue 'run;
                }
                let input_tokens = capacity.input_tokens;
                let request_max_output_tokens = capacity.max_output_tokens;
                let messages = match session.context() {
                    Ok(m) => m,
                    Err(e) => break 'run FinishReason::Failed(e.into()),
                };
                let active_system = capacity.active_system;
                let responses =
                    if auto_compaction_mode == AgentCompactionMode::NativeResponses {
                        match native_responses_options(
                            session,
                            &model,
                            &active_system,
                            service_tier,
                        ) {
                            Ok(options) => Some(options),
                            Err(error) => break 'run FinishReason::Failed(error),
                        }
                    } else {
                        match durable_responses_options(
                            session,
                            &model,
                            &active_system,
                            service_tier,
                        ) {
                            Ok(options) => options,
                            Err(error) => break 'run FinishReason::Failed(error),
                        }
                    };

                let request = Request {
                    system: if active_system.is_empty() { None } else { Some(active_system.clone()) },
                    messages,
                    tools: request_tool_defs.clone(),
                    tool_choice: if answer_only {
                        ToolChoice::None
                    } else {
                        ToolChoice::Auto
                    },
                    max_output_tokens: Some(request_max_output_tokens),
                    temperature: None,
                    stop: vec![],
                    reasoning: match request_reasoning_for_replay(
                        session,
                        &model,
                        responses.as_ref(),
                        &reasoning,
                    ) {
                        Ok(selection) => selection,
                        Err(error) => break 'run FinishReason::Failed(error),
                    },
                    reasoning_mode,
                    responses,
                    output_format: OutputFormat::Text,
                    output_modalities: output_modalities.clone(),
                    compatibility: CompatibilityMode::Strict,
                    cache_retention,
                    session_id: Some(session_id.clone()),
                };
                let prepared = PreparedTurn::new(
                    session.head(),
                    active_system.clone(),
                    tool_revision,
                    request,
                    input_tokens,
                );
                let current_tool_generation = extension_host.tool_snapshot().0;
                if !prepared.is_current(session, &active_system, current_tool_generation) {
                    // Re-enter the boundary so a publication or append that
                    // crossed compaction cannot pair an old request with a new
                    // tool map or durable cursor.
                    continue 'run;
                }
                let input_tokens = prepared.input_tokens;
                let request = prepared.request;

                let reserved_output_tokens = match reservation_output_tokens(
                    session, &model, request_max_output_tokens, max_session_tokens, max_session_cost_microdollars,
                ) {
                    Ok(tokens) => tokens,
                    Err(error) => break 'run FinishReason::Failed(error),
                };
                if let Err(error) = reserve_request_tokens(
                    session,
                    input_tokens,
                    reserved_output_tokens,
                    max_session_tokens,
                ) {
                    break 'run FinishReason::Failed(error);
                }
                if let Err(error) = reserve_request_cost_with_tier(
                    session,
                    &model,
                    input_tokens,
                    reserved_output_tokens,
                    max_session_cost_microdollars,
                    request.responses.as_ref().and_then(|options| options.service_tier),
                    request.cache_retention,
                ) {
                    break 'run FinishReason::Failed(error);
                }

                // ── Open the provider stream (abortable) ───────────────────
                // Row 3.5: one logical provider request. It is opened here so
                // that turn iterations that never reach the provider (steering
                // or a stale prepared turn) do not fabricate a request span.
                let request_guard = turn_context.begin_typed::<ProviderRequestSpan>(
                    RequestAttributes {
                        operation: SpanOperation::Assistant,
                    },
                );
                turn_attempt_opened = true;
                // A new provider request for this model turn starts here.
                // Anchor first-token-latency measurement for consumers that
                // track it per attempt: the first OutputDelta of this stream
                // measured from this event is the attempt's TTFT.
                let ev = AgentEvent::TurnStarted;
                notify_observers(&observers, &ev);
                yield ev;
                let qualified = !native_enabled && qualified_inference_replacement(&model, &request);
                let native_delta = match native_steering::required_input_request(request.clone(), session, &model) {
                    Ok(request) => request, Err(error) => break 'run FinishReason::Failed(error),
                };
                let opening_client = client.track_request_dispatch();
                let opened = tokio::select! {
                    biased;
                    _ = abort.wait() => Ok(None),
                    _ = wait_network_deadline(network_deadline) => {
                        if opening_client.request_may_have_been_sent() {
                            let first = !session.has_uncertain_usage();
                            let recorded = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "assistant_turn");
                            if first {
                                let event = AgentEvent::ProviderUsageUncertain;
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            if let Err(error) = recorded {
                                break 'run FinishReason::Failed(error.into());
                            }
                            failed_usage_unknown = true;
                        }
                        break 'run FinishReason::Failed(AgentError::NetworkWaitLimit { limit: max_network_wait.unwrap(), usage_unknown: failed_usage_unknown });
                    },
                    result = async {
                        if let Some(connection) = native.connection.take() {
                            Ok(Some(native_steering::ProviderStream::Native(connection, native_updates_tx.clone(), None)))
                        } else if native_enabled {
                            opening_client.steerable_responses(&model, request).await.map(|connection| Some(native_steering::ProviderStream::Native(connection, native_updates_tx.clone(), None)))
                        } else {
                            open_provider_stream(&opening_client, &model, request, &abort).await.map(|stream| stream.map(native_steering::ProviderStream::Ordinary))
                        }
                    } => result,
                };
                let mut response_stream = match opened {
                    Err(error) if native_enabled => {
                        if opening_client.request_may_have_been_sent() {
                            if let Err(record_error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "native_steering") { break 'run FinishReason::Failed(record_error.into()); }
                            let event = AgentEvent::ProviderUsageUncertain; notify_observers(&observers, &event); yield event;
                        }
                        break 'run FinishReason::Failed(error.into());
                    }
                    Err(error) if context_retries < MAX_PROVIDER_RETRIES && looks_like_context_error(&error) => {
                        context_retries += 1;
                        let compacted = {
                            let mut compaction = CompactionContext {
                        run_id: &effect_run_id,
                        resource_owner: &resource_owner,
                        retry_hooks: &provider_retry_hooks,
                        compaction_strategy: extension_host.compaction_strategy.as_ref(),
                        max_network_wait,
                        provider_retries_enabled,
                        client: &client,
                                model: &model,
                                compaction_model: &compaction_model,
                                summary_operation: crate::events::ProviderOperation::LocalCompaction,
                                session,
                                usage: &mut run_usage,
                                run_cost: &mut run_cost,
                                cache_retention,
                                reasoning: &reasoning,
                                reasoning_mode,
                                session_id: &session_id,
                                max_session_tokens,
                                max_session_cost_microdollars,
                                abort: &abort,
                                mode: if background_tools.is_empty() && native.connection.is_none() { auto_compaction_mode } else { AgentCompactionMode::Disabled },
                                threshold_fraction: compaction_threshold_fraction,
                                keep_recent_tokens: compaction_keep_recent_tokens,
                                events: &compaction_event_tx,
                                context: &stream_context,
                                tool_generation: tool_revision,
                                capacity: &mut context_capacity,
                                telemetry: turn_context.clone(),
                            };
                            let operation = compaction.force_one_boundary(
                                &system,
                                &request_tool_defs,
                                compaction_reserve_tokens,
                            );
                            tokio::pin!(operation);
                            let result = loop {
                                tokio::select! {
                                    biased;
                                    _ = abort.wait() => break Err(AgentError::Cancelled),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); }
                                None => control_open = false,
                            },
                            Some(event) = compaction_event_rx.recv() => {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    result = &mut operation => break result,
                                }
                            };
                            while let Ok(event) = compaction_event_rx.try_recv() {
                                notify_observers(&observers, &event);
                                yield event;
                            }
                            result
                        };
                        if let Err(compaction_error) = compacted {
                            break 'run if matches!(&compaction_error, AgentError::Cancelled) {
                                FinishReason::Aborted
                            } else {
                                FinishReason::Failed(compaction_error)
                            };
                        }
                        continue 'run;
                    }
                    Err(error) => {
                        pending_recovery = Some(PendingProviderRecovery {
                            error, qualified, saw_generation: false, opened: false,
                        });
                        continue 'run;
                    }
                    Ok(None) => break 'run FinishReason::Aborted,
                    Ok(Some(s)) => {
                        // Connectivity recovered. Body deadlines belong to the
                        // provider; a later pre-send outage gets its own clock.
                        network_deadline = None;
                        s
                    },
                };
                if let Some(control) = response_stream.control() {
                    if native.control.is_none() { native.begin(format!("{effect_run_id}:{completed_turns}"), control); }
                }
                if native.required_input && native_delta.messages.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(_))))) {
                    native.required_input = false;
                    if let Some(control) = native.control.as_ref() {
                        if let Err(error) = control.continue_with(native_delta.clone()).await { break 'run FinishReason::Failed(error.into()); }
                    }
                }
                // Parity 1e.2 durability half: encode the in-flight assistant
                // message into compact frames and journal them beside the
                // session between deltas, so a killed process can republish the
                // partial prefix. Frames never include a terminal event, and a
                // journal fault never affects the provider stream.
                let mut assistant_frame_encoder = octet_ai::AssistantMessageFrameEncoder::new(
                    model.spec.id.clone(),
                    model.spec.protocol,
                );
                let mut assistant_frame_journal = session.begin_assistant_frame_journal().ok();

                // ── Consume the stream, staying responsive to control ──────
                // Text/tool deltas dominate this hot path; keep StreamEvent
                // inline rather than allocating a box for every event.
                #[allow(clippy::large_enum_variant)]
                enum Next {
                    Event(Option<Result<StreamEvent, AiError>>),
                    Ctl(Option<Control>),
                    Delegation(Option<DelegationTelemetrySnapshot>),
                    Steering(octet_ai::SteeringUpdate),
                    Abort,
                }
                let mut attempt_saw_generation = false;
                // Row 3.5: the streaming response is its own boundary nested
                // under the request that produced it.
                let stream_guard = request_guard
                    .context()
                    .begin_typed::<ProviderStreamSpan>(EmptyAttributes {});
                let turn = loop {
                    let next = tokio::select! {
                        biased;
                        _ = abort.wait() => Next::Abort,
                        c = control_rx.recv(), if control_open => Next::Ctl(c),
                        Some(update) = native_updates_rx.recv() => Next::Steering(update),
                        snapshot = async {
                            match &mut delegation_telemetry {
                                Some(receiver) => next_delegation_snapshot(receiver).await,
                                None => std::future::pending().await,
                            }
                        }, if delegation_telemetry.is_some() => Next::Delegation(snapshot),
                        ev = response_stream.next() => Next::Event(ev),
                    };
                    // Apply the selected update before subsequently queued ones;
                    // both paths must drive required-input continuations.
                    let next = match next {
                        Next::Steering(update) => {
                            if let Err(error) = native.update(update, session, &model) { break 'run FinishReason::Failed(error); }
                            None
                        }
                        next => Some(next),
                    };
                    while let Ok(update) = native_updates_rx.try_recv() {
                        if let Err(error) = native.update(update, session, &model) { break 'run FinishReason::Failed(error); }
                    }
                    if native.required_input && native_delta.messages.iter().any(|message| matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(_))))) {
                        native.required_input = false;
                        if let Some(control) = native.control.as_ref() {
                            if let Err(error) = control.continue_with(native_delta.clone()).await { break 'run FinishReason::Failed(error.into()); }
                        }
                    }
                    match native.deliver(session, &control_prompt_metadata, &mut terminal_gate_evidence) {
                        Ok(Some(event)) => { notify_observers(&observers, &event); yield event; },
                        Ok(None) => {}, Err(error) => break 'run FinishReason::Failed(error),
                    }
                    let Some(next) = next else { continue; };
                    if matches!(next, Next::Event(Some(Ok(StreamEvent::Started { .. })))) { native.started(response_stream.response_id()); }
                    let next = match next {
                        Next::Event(Some(Ok(StreamEvent::Finished(response)))) => {
                            match incomplete_responses_error(&model, &response) {
                                Some(error) => Next::Event(Some(Err(error))),
                                None => Next::Event(Some(Ok(StreamEvent::Finished(response)))),
                            }
                        }
                        next => next,
                    };
                    match next {
                        Next::Abort | Next::Ctl(Some(Control::Abort)) => {
                            if let Some(journal) = assistant_frame_journal.as_mut() {
                                journal.settle();
                            }
                            break Err(FinishReason::Aborted);
                        }
                        Next::Ctl(Some(Control::Steer(input))) => {
                            match native.submit(input, session, &model).await {
                                Ok(Some(input)) => input.push_pending(&mut pending_steer),
                                Ok(None) => {}, Err(error) => break 'run FinishReason::Failed(error),
                            }
                        },
                        Next::Steering(_) => unreachable!("handled above"),
                        Next::Ctl(Some(Control::FollowUp(input))) => followups.push_back(input),
                        Next::Ctl(Some(Control::FinishNow(input))) => {
                            input.push_pending(&mut pending_steer);
                            answer_only = true;
                            finish_pending = true;
                            context_capacity.invalidate();
                        }
                        Next::Ctl(Some(Control::SetReasoning(selection))) => pending_reasoning = Some(selection),
                        Next::Ctl(Some(Control::SetSteeringMode(mode))) => steering_mode = mode,
                        Next::Ctl(Some(Control::SetFollowUpMode(mode))) => follow_up_mode = mode,
                        Next::Ctl(None) => control_open = false,
                        Next::Delegation(Some(snapshot)) => {
                            let event = AgentEvent::DelegationUpdated { snapshot };
                            notify_observers(&observers, &event);
                            yield event;
                        }
                        Next::Delegation(None) => delegation_telemetry = None,
                        Next::Event(None) | Next::Event(Some(Err(_))) => {
                            let error = match next {
                                Next::Event(Some(Err(error))) => error,
                                _ => AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish),
                            };
                            if native_enabled {
                                native.connection = response_stream.into_native();
                                if let Err(record_error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "native_steering") { break 'run FinishReason::Failed(record_error.into()); }
                                let event = AgentEvent::ProviderUsageUncertain; notify_observers(&observers, &event); yield event;
                                break 'run FinishReason::Failed(error.into());
                            }
                            // Retire the failed stream before hooks, backoff,
                            // compaction or any replacement can open a transport.
                            drop(response_stream);
                            // An observed failure is settled, unlike an undriven
                            // Run drop. Its prefix must not block the replacement
                            // journal or reappear after a successful retry.
                            if let Some(journal) = assistant_frame_journal.as_mut() {
                                journal.settle();
                            }
                            if !attempt_saw_generation
                                && context_retries < MAX_PROVIDER_RETRIES
                                && looks_like_context_error(&error)
                            {
                                stream_context.provider_retry();
                                context_retries += 1;
                                let compacted = {
                                    let mut compaction = CompactionContext {
                        run_id: &effect_run_id,
                        resource_owner: &resource_owner,
                        retry_hooks: &provider_retry_hooks,
                        compaction_strategy: extension_host.compaction_strategy.as_ref(),
                        max_network_wait,
                        provider_retries_enabled,
                        client: &client,
                                        model: &model,
                                        compaction_model: &compaction_model,
                                        summary_operation: crate::events::ProviderOperation::LocalCompaction,
                                        session,
                                        usage: &mut run_usage,
                                        run_cost: &mut run_cost,
                                        cache_retention,
                                        reasoning: &reasoning,
                                        reasoning_mode,
                                        session_id: &session_id,
                                        max_session_tokens,
                                        max_session_cost_microdollars,
                                        abort: &abort,
                                        mode: if background_tools.is_empty() && native.connection.is_none() { auto_compaction_mode } else { AgentCompactionMode::Disabled },
                                        threshold_fraction: compaction_threshold_fraction,
                                        keep_recent_tokens: compaction_keep_recent_tokens,
                                        events: &compaction_event_tx,
                                        context: &stream_context,
                                        tool_generation: tool_revision,
                                        capacity: &mut context_capacity,
                                        telemetry: turn_context.clone(),
                                    };
                                    let operation = compaction.force_one_boundary(
                                        &system,
                                        &request_tool_defs,
                                        compaction_reserve_tokens,
                                    );
                                    tokio::pin!(operation);
                                    let result = loop {
                                        tokio::select! {
                                            biased;
                                            _ = abort.wait() => break Err(AgentError::Cancelled),
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => { abort.set(); }
                                None => control_open = false,
                            },
                            Some(event) = compaction_event_rx.recv() => {
                                                notify_observers(&observers, &event);
                                                yield event;
                                            }
                                            result = &mut operation => break result,
                                        }
                                    };
                                    while let Ok(event) = compaction_event_rx.try_recv() {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    result
                                };
                                match compacted {
                                    Ok(()) => continue 'run,
                                    Err(error) if matches!(&error, AgentError::Cancelled) => {
                                        break 'run FinishReason::Aborted;
                                    }
                                    Err(error) => {
                                        break 'run FinishReason::Failed(error);
                                    }
                                }
                            }
                            pending_recovery = Some(PendingProviderRecovery {
                                error, qualified, saw_generation: attempt_saw_generation, opened: true,
                            });
                            continue 'run;
                        }
                        Next::Event(Some(Ok(event))) => {
                            stream_context.observe_stream(&event);
                            // Journal the frame this event produces, if any.
                            // Terminal events produce no frame and are handled
                            // by settlement below.
                            if let Ok(Some(frame)) = assistant_frame_encoder.encode(&event) {
                                if let Some(journal) = assistant_frame_journal.as_mut() {
                                    let _ = journal.append(&frame);
                                }
                            }
                            match event {
                            StreamEvent::ProviderLifecycle(lifecycle) => {
                                let ev = AgentEvent::ProviderLifecycle { lifecycle };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            StreamEvent::TextDelta { delta, .. } => {
                                attempt_saw_generation = true;
                                let ev = AgentEvent::OutputDelta {
                                    channel: OutputChannel::Text,
                                    text: delta,
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            StreamEvent::ReasoningDelta { delta, .. } => {
                                attempt_saw_generation = true;
                                let ev = AgentEvent::OutputDelta {
                                    channel: OutputChannel::Reasoning,
                                    text: delta,
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            // `octet-ai` assembles and validates the complete
                            // message. Tool deltas are provisional: execute only
                            // after the assistant turn is durably persisted.
                            StreamEvent::ToolCallStart { .. }
                            | StreamEvent::ToolCallArgsDelta { .. }
                            | StreamEvent::ToolCallEnd { .. } => {
                                attempt_saw_generation = true;
                            }
                            StreamEvent::MediaCompleted { index, media } => {
                                attempt_saw_generation = true;
                                let ev = AgentEvent::OutputMedia { index, media };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }
                            StreamEvent::Finished(response) => {
                                // Terminal settlement: the frame sequence is
                                // partial progress only and must never be
                                // republished once the attempt is complete.
                                if let Some(journal) = assistant_frame_journal.as_mut() {
                                    journal.settle();
                                }
                                break Ok(response)
                            }
                            _ => {}
                            }
                        },
                    }
                };
                let response = match turn {
                    Ok(response) => {
                        turn_attempt_succeeded = true;
                        CompletionAttributes::usage(&response.usage)
                            .with_uncertainty(session.has_uncertain_usage())
                            .record(&request_guard.span);
                        stream_guard.finish(false);
                        request_guard.finish(false);
                        response
                    }
                    Err(reason) => break 'run reason,
                };
                // Context-recovery attempts are scoped to one logical provider
                // turn. A successful response proves the current compacted
                // prefix is accepted and restores the recovery budget for a
                // later autonomous turn in the same run.
                context_retries = 0;
                stream_retries = 0;
                recovery_budget = ProviderRecoveryBudget::default();
                network_retries = 0;
                network_deadline = None;
                failed_usage_unknown = session.has_uncertain_usage();
                // Max-turns counts completed provider turns. Context rejection
                // and transport recovery happen within the same logical turn
                // and must not consume the autonomous work budget.
                completed_turns = completed_turns.saturating_add(1);
                if native.has_pending() {
                    native.connection = response_stream.into_native();
                } else {
                    drop(response_stream);
                    native.control = None;
                }

                // ── Persist the completed assistant message ────────────────
                // StopReason is semantic control data, not parser metadata. It
                // must be inspected before deciding whether a no-tool turn is
                // a successful completion.
                let stop_reason = response.stop_reason.clone();
                let turn_usage = response.usage;
                let raw_responses_output = response.responses_output.clone();
                let deferred_handle = response.deferred.clone();
                let deferred_diagnostics = response.diagnostics.clone();
                let assistant = response.message;
                let calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        octet_ai::AssistantPart::ToolCall(tc) => Some(tc.clone()),
                        _ => None,
                    })
                    .collect();

                if auto_compaction_mode == AgentCompactionMode::NativeResponses
                    && model.spec.protocol == Protocol::OpenAiResponses
                    && raw_responses_output.is_none()
                    // A parked request has no response output by definition;
                    // it is a durable suspension, not a malformed native turn.
                    && !matches!(stop_reason, StopReason::Deferred)
                {
                    add_usage(&mut run_usage, &turn_usage);
                    let turn_cost = response.cost;
                    if let Err(error) = session.record_rejected_responses_turn_usage(
                        model.endpoint.id.clone(),
                        model.spec.id.clone(),
                        turn_usage,
                        turn_cost,
                    ) {
                        break 'run FinishReason::Failed(error.into());
                    }
                    run_cost.add(turn_cost);
                    break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                        stop_reason:
                            "native Responses mode requires non-empty authoritative terminal output"
                                .to_owned(),
                    });
                }

                let persistence_context = assistant_persistence_context(
                    &effect_run_id,
                    &resource_owner,
                    &assistant,
                    stop_reason.clone(),
                );
                let persistence_metadata = collect_persistence_metadata(
                    &persistence_metadata_hooks,
                    &persistence_context,
                    &abort,
                )
                .await;

                // ── Durable deferred park (rows 4.12 / 1e.1) ─────────────
                // The provider did not finish this request. Retrying the
                // generation request would be dishonest and could bill the same
                // effect twice, so the turn is persisted with its deferred stop
                // reason (Pi keeps that boundary in history) and the run parks
                // at a durable `deferred.suspended` leaf. A later permitted
                // pass polls the recorded handle; only a valid handle parks.
                if matches!(stop_reason, StopReason::Deferred) {
                    let deferred_usage_billed = usage_is_billed(&turn_usage);
                    let assistant_entry = if deferred_usage_billed {
                        match session.append_assistant_turn_with_metadata(
                            assistant.clone(),
                            model.endpoint.id.clone(),
                            model.spec.id.clone(),
                            turn_usage,
                            response.cost,
                            stop_reason.clone(),
                            None,
                            persistence_metadata.clone(),
                        ) {
                            Ok(entry) => entry,
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        }
                    } else {
                        // A parked request reports no billed usage. Recording a
                        // zero-token unpriced operation would falsely block hard
                        // cost ceilings, so the boundary is persisted without a
                        // usage record; the settled poll's response carries the
                        // authoritative usage for the whole request.
                        match session.append_with_metadata(
                            EntryValue::Message(Message::Assistant(assistant.clone())),
                            persistence_metadata.clone(),
                        ) {
                            Ok(entry) => entry,
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        }
                    };
                    add_usage(&mut run_usage, &turn_usage);
                    run_cost.add(response.cost);
                    let operation_id = format!("{effect_run_id}:deferred:{completed_turns}");
                    let declaration = DeferredResponseDeclaration {
                        stop_reason: crate::tools::deferred::DeferredStopReason::Deferred,
                        api: deferred_handle
                            .as_ref()
                            .map(|handle| handle.api.clone())
                            .unwrap_or_default(),
                        handle: deferred_handle.clone().map(DeferredHandle::from),
                    };
                    let identity = deferred_model_identity(&model);
                    let store = session.deferred_run_store();
                    match store.suspend(
                        &identity,
                        &operation_id,
                        &assistant_entry.0,
                        declaration,
                    ) {
                        Ok(DeferredSuspendDecision::Suspended(leaf)) => {
                            let suspension = DeferredRunSuspended {
                                operation_id: leaf.operation_id.clone(),
                                source_entry_id: leaf.source_entry_id.clone(),
                                stop_reason: crate::tools::deferred::DeferredStopReason::Deferred,
                                handle: leaf.handle.clone(),
                                poll: leaf.poll,
                                generation: leaf.generation,
                            };
                            for observer in &observers.observers {
                                observer.on_run_suspend(&suspension);
                            }
                            let _deferred_guard = telemetry.begin_typed::<DeferredRunSpan>(
                                DeferredRunAttributes {
                                    operation_id: bounded_deferred_label(&leaf.operation_id),
                                    stop_reason: "deferred".to_owned(),
                                    phase: "suspended".to_owned(),
                                    poll: leaf.poll,
                                    generation: leaf.generation,
                                    recovery: false,
                                    diagnostics: deferred_diagnostics.len(),
                                },
                            );
                            break 'run FinishReason::Failed(AgentError::DeferredSuspended {
                                operation_id: leaf.operation_id.clone(),
                                poll: leaf.poll,
                                generation: leaf.generation,
                            });
                        }
                        Ok(DeferredSuspendDecision::Settled) => {}
                        Ok(DeferredSuspendDecision::Failed(failure)) => {
                            break 'run FinishReason::Failed(
                                AgentError::DeferredSuspensionRefused {
                                    diagnostic: failure.diagnostic.clone(),
                                },
                            );
                        }
                        Err(error) => break 'run FinishReason::Failed(error.into()),
                    }
                }
                let assistant_entry = match session.append_assistant_turn_with_metadata(
                    assistant.clone(),
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    turn_usage,
                    response.cost,
                    stop_reason.clone(),
                    raw_responses_output,
                    persistence_metadata,
                ) {
                    Ok(entry) => entry,
                    Err(error) => break 'run FinishReason::Failed(error.into()),
                };
                if let Err(error) = native.settle_successor(session, &model, assistant_entry) { break 'run FinishReason::Failed(error); }
                native.completed_prefix();
                match native.deliver(session, &control_prompt_metadata, &mut terminal_gate_evidence) {
                    Ok(Some(event)) => { notify_observers(&observers, &event); yield event; },
                    Ok(None) => {}, Err(error) => break 'run FinishReason::Failed(error),
                }
                context_capacity.observe_assistant_response(session, &model, &turn_usage);
                add_usage(&mut run_usage, &turn_usage);
                let turn_cost = response.cost;
                run_cost.add(turn_cost);
                let normal_end = matches!(stop_reason, StopReason::EndTurn | StopReason::StopSequence);
                let output_truncated = matches!(stop_reason, StopReason::MaxTokens);
                let needs_continuation = output_truncated
                    || matches!(stop_reason, StopReason::PauseTurn)
                    || (matches!(stop_reason, StopReason::Steered) && native.has_pending())
                    || matches!(&stop_reason, StopReason::Other(reason) if reason == "tool_output_locked");
                if normal_end && calls.is_empty() && background_tools.is_empty() && !assistant_has_terminal_content(&assistant) {
                    // A normal stop without terminal content is not a completed
                    // turn. Persist its message and usage above, then fail without
                    // retrying; the stop alone does not establish the cause.
                    break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                        stop_reason: incomplete_terminal_response_reason(
                            &assistant,
                            &stop_reason,
                            &turn_usage,
                            &response.diagnostics,
                            request_max_output_tokens,
                        ),
                    });
                }
                let gated_candidate = completion_policy == CompletionPolicy::TerminalGate
                    && background_tools.is_empty() && !native.has_pending()
                    && calls.is_empty()
                    && normal_end;

                // Candidate turns stay provisional until their isolated gate
                // returns R. Tool turns and natural-policy answers commit now.
                if !gated_candidate {
                    let session_cost = priced_session_subtotal(session, &model);
                    let ev = AgentEvent::TurnFinished {
                        message: assistant.clone(),
                        stop_reason: stop_reason.clone(),
                        turn_usage,
                        turn_cost,
                        usage: run_usage,
                        session_cost_microdollars: session_cost,
                        run_cost_microdollars: run_cost.microdollars,
                    };
                    notify_observers(&observers, &ev);
                    yield ev;
                }

                // Results from the previous response retain their original IDs.
                // The concurrent assistant is committed first: it was generated
                // without these results. Sync/effectful work is a strict barrier.
                let completed_background = !background_tools.is_empty();
                while !background_tools.is_empty() {
                    let operation = background_tools.settle_one(session, &model, &sandbox,
                        &stream_context, &mut run_usage, &mut terminal_gate_evidence);
                    tokio::pin!(operation);
                    let settled = loop {
                        tokio::select! {
                            biased;
                            result = &mut operation => break result,
                            control = control_rx.recv(), if control_open => match control {
                                Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Some(Control::FollowUp(input)) => followups.push_back(input),
                                Some(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer); answer_only = true; finish_pending = true;
                                }
                                Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Some(Control::Abort) => abort.set(),
                                None => control_open = false,
                            },
                        }
                    };
                    match settled {
                        Ok(events) => for event in events { notify_observers(&observers, &event); yield event; },
                        Err(error) => break 'run FinishReason::Failed(error),
                    }
                    context_capacity.invalidate();
                }

                // Drain control before deciding whether a provisional candidate
                // is terminal. New user input takes precedence over the gate.
                {
                    let mut admission = control_admission.lock().unwrap_or_else(|error| error.into_inner());
                    while control_open {
                        match control_rx.try_recv() {
                            Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                            Ok(Control::FollowUp(input)) => followups.push_back(input),
                            Ok(Control::FinishNow(input)) => {
                                input.push_pending(&mut pending_steer);
                                answer_only = true;
                                finish_pending = true;
                                context_capacity.invalidate();
                            }
                            Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                            Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                            Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                            Ok(Control::Abort) => {
                                abort.set();
                                break;
                            }
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                        }
                    }
                    pending_steer.retain(ReservedInput::is_pending);
                    if !gated_candidate && !completed_background && !native.has_pending() && calls.is_empty() && normal_end && !needs_continuation
                        && pending_steer.is_empty() && pending_reasoning.is_none() && followups.is_empty() {
                        *admission = false;
                    }
                }

                // A response is not successful merely because it contains no
                // tool calls. Refusals, pauses, provider-specific reasons, and
                // malformed tool-use endings are terminal failures; a length
                // stop gets one corrective continuation instead.
                if !normal_end
                    && !needs_continuation
                    && !matches!(stop_reason, StopReason::ToolUse)
                {
                    break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                        stop_reason: stop_reason.as_canonical().to_owned(),
                    });
                }

                if native.has_pending() && calls.is_empty() {
                    if abort.is_set() { break 'run FinishReason::Aborted; }
                    continue 'run;
                }
                if completed_background && calls.is_empty() && normal_end {
                    if abort.is_set() { break 'run FinishReason::Aborted; }
                    continue 'run;
                }
                if calls.is_empty() {
                    if abort.is_set() {
                        if gated_candidate {
                            let ev = AgentEvent::CandidateRejected {
                                usage: run_usage,
                                run_cost_microdollars: run_cost.microdollars,
                                session_cost_microdollars: priced_session_subtotal(session, &model),
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        break 'run FinishReason::Aborted;
                    }
                    if needs_continuation {
                        let instruction = continuation_instruction(&stop_reason);
                        if let Err(e) = session.append(user_message(UserInput::from(instruction))) {
                            break 'run FinishReason::Failed(e.into());
                        }
                        continue;
                    }
                    if !normal_end {
                        break 'run FinishReason::Failed(AgentError::IncompleteResponse {
                            stop_reason: stop_reason.as_canonical().to_owned(),
                        });
                    }
                    // Steering and follow-ups make this a normal intermediate
                    // turn, so commit it without spending a gate request.
                    pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                        if gated_candidate {
                            let session_cost = priced_session_subtotal(session, &model);
                            let ev = AgentEvent::TurnFinished {
                                message: assistant.clone(),
                                stop_reason: stop_reason.clone(),
                                turn_usage,
                                turn_cost,
                                usage: run_usage,
                                session_cost_microdollars: session_cost,
                                run_cost_microdollars: run_cost.microdollars,
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        continue;
                    }
                    if !followups.is_empty() {
                        if gated_candidate {
                            let session_cost = priced_session_subtotal(session, &model);
                            let ev = AgentEvent::TurnFinished {
                                message: assistant.clone(),
                                stop_reason: stop_reason.clone(),
                                turn_usage,
                                turn_cost,
                                usage: run_usage,
                                session_cost_microdollars: session_cost,
                                run_cost_microdollars: run_cost.microdollars,
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        let queued = match follow_up_mode {
                            QueueDeliveryMode::All => followups.drain(..).collect::<Vec<_>>(),
                            QueueDeliveryMode::OneAtATime => {
                                vec![followups.pop_front().expect("follow-up queue is non-empty")]
                            }
                        };
                        let visible_tools = if answer_only {
                            &[][..]
                        } else {
                            tool_defs.as_slice()
                        };
                        let observation = ContextObservation {
                            tracker: &stream_context,
                            model: &model,
                            system: &system,
                            tools: visible_tools,
                        };
                        match deliver_control_inputs(
                            queued,
                            ControlDeliveryKind::FollowUp,
                            session,
                            &control_prompt_metadata,
                            &mut terminal_gate_evidence,
                            &observation,
                            Some(&abort),
                        ).await {
                            ControlDelivery::Completed { event } => {
                                if let Some(ev) = event {
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                            }
                            ControlDelivery::Interrupted { event, finish } => {
                                if let Some(ev) = event {
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                break 'run finish;
                            }
                        }
                        continue;
                    }
                    if let Some(evidence) = terminal_gate_evidence.as_ref() {
                        let capsule = terminal_gate_capsule(evidence, &assistant);
                        let decision = {
                            let mut gate = TerminalGateContext {
                                run_id: &effect_run_id,
                                resource_owner: &resource_owner,
                                retry_hooks: &provider_retry_hooks,
                                max_network_wait,
                                provider_retries_enabled,
                                events: &compaction_event_tx,
                                client: &client,
                                model: &model,
                                session,
                                usage: &mut run_usage,
                                run_cost: &mut run_cost,
                                cache_retention,
                                session_id: &session_id,
                                max_session_tokens,
                                max_session_cost_microdollars,
                                abort: &abort,
                            };
                            let operation = gate.decide(capsule);
                            tokio::pin!(operation);
                            loop {
                                tokio::select! {
                                    biased;
                                    _ = abort.wait() => break Err(AgentError::Cancelled),
                                    control = control_rx.recv(), if control_open => match control {
                                        Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                        Some(Control::FollowUp(input)) => followups.push_back(input),
                                        Some(Control::FinishNow(input)) => {
                                            input.push_pending(&mut pending_steer);
                                            answer_only = true;
                                            finish_pending = true;
                                        }
                                        Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                        Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                        Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                        Some(Control::Abort) => { abort.set(); }
                                        None => control_open = false,
                                    },
                                    Some(event) = compaction_event_rx.recv() => {
                                        notify_observers(&observers, &event);
                                        yield event;
                                    }
                                    result = &mut operation => break result,
                                }
                            }
                        };
                        while let Ok(event) = compaction_event_rx.try_recv() {
                            notify_observers(&observers, &event);
                            yield event;
                        }
                        let return_candidate = matches!(decision, Ok(TerminalGateDecision::Return));
                        if return_candidate {
                            let session_cost = priced_session_subtotal(session, &model);
                            let ev = AgentEvent::TurnFinished {
                                message: assistant.clone(),
                                stop_reason: stop_reason.clone(),
                                turn_usage,
                                turn_cost,
                                usage: run_usage,
                                session_cost_microdollars: session_cost,
                                run_cost_microdollars: run_cost.microdollars,
                            };
                            notify_observers(&observers, &ev);
                            yield ev;
                        }
                        // Linearize successful submissions against terminal
                        // admission, including the gate's final poll and the
                        // TurnFinished suspension. Never hold this lock at yield.
                        {
                            let mut admission = control_admission.lock().unwrap_or_else(|error| error.into_inner());
                            while control_open {
                                match control_rx.try_recv() {
                                    Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                    Ok(Control::FollowUp(input)) => followups.push_back(input),
                                    Ok(Control::FinishNow(input)) => {
                                        input.push_pending(&mut pending_steer);
                                        answer_only = true;
                                        finish_pending = true;
                                        context_capacity.invalidate();
                                    }
                                    Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                    Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                    Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                    Ok(Control::Abort) => abort.set(),
                                    Err(mpsc::error::TryRecvError::Empty) => break,
                                    Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                                }
                            }
                            pending_steer.retain(ReservedInput::is_pending);
                            if return_candidate && pending_steer.is_empty() && pending_reasoning.is_none() && followups.is_empty() {
                                *admission = false;
                            }
                        }
                        if abort.is_set() {
                            break 'run FinishReason::Aborted;
                        }
                        if decision.is_ok() {
                            // Steering and follow-ups make this a normal intermediate
                            // turn, so commit it without spending a gate request.
                            pending_steer.retain(ReservedInput::is_pending);
                if native.connection.is_none() && (!pending_steer.is_empty() || pending_reasoning.is_some()) {
                                if gated_candidate && !return_candidate {
                                    let session_cost = priced_session_subtotal(session, &model);
                                    let ev = AgentEvent::TurnFinished {
                                        message: assistant.clone(),
                                        stop_reason: stop_reason.clone(),
                                        turn_usage,
                                        turn_cost,
                                        usage: run_usage,
                                        session_cost_microdollars: session_cost,
                                        run_cost_microdollars: run_cost.microdollars,
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                continue;
                            }
                            if !followups.is_empty() {
                                if gated_candidate && !return_candidate {
                                    let session_cost = priced_session_subtotal(session, &model);
                                    let ev = AgentEvent::TurnFinished {
                                        message: assistant.clone(),
                                        stop_reason: stop_reason.clone(),
                                        turn_usage,
                                        turn_cost,
                                        usage: run_usage,
                                        session_cost_microdollars: session_cost,
                                        run_cost_microdollars: run_cost.microdollars,
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                let queued = match follow_up_mode {
                                    QueueDeliveryMode::All => followups.drain(..).collect::<Vec<_>>(),
                                    QueueDeliveryMode::OneAtATime => {
                                        vec![followups.pop_front().expect("follow-up queue is non-empty")]
                                    }
                                };
                                let visible_tools = if answer_only {
                                    &[][..]
                                } else {
                                    tool_defs.as_slice()
                                };
                                let observation = ContextObservation {
                                    tracker: &stream_context,
                                    model: &model,
                                    system: &system,
                                    tools: visible_tools,
                                };
                                match deliver_control_inputs(
                                    queued,
                                    ControlDeliveryKind::FollowUp,
                                    session,
                                    &control_prompt_metadata,
                                    &mut terminal_gate_evidence,
                                    &observation,
                                    Some(&abort),
                                ).await {
                                    ControlDelivery::Completed { event } => {
                                        if let Some(ev) = event {
                                            notify_observers(&observers, &ev);
                                            yield ev;
                                        }
                                    }
                                    ControlDelivery::Interrupted { event, finish } => {
                                        if let Some(ev) = event {
                                            notify_observers(&observers, &ev);
                                            yield ev;
                                        }
                                        break 'run finish;
                                    }
                                }
                                continue;
                            }
                        }
                        match decision {
                            Ok(TerminalGateDecision::Return) => {
                                break 'run FinishReason::Completed;
                            }
                            Ok(TerminalGateDecision::Continue) => {
                                let ev = AgentEvent::CandidateRejected {
                                    usage: run_usage,
                                    run_cost_microdollars: run_cost.microdollars,
                                    session_cost_microdollars: priced_session_subtotal(session, &model),
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                                if let Err(error) = session.append(user_message(UserInput::from(
                                    TERMINAL_GATE_CORRECTION,
                                ))) {
                                    break 'run FinishReason::Failed(error.into());
                                }
                                continue;
                            }
                            Err(AgentError::Cancelled) => {
                                let ev = AgentEvent::CandidateRejected {
                                    usage: run_usage,
                                    run_cost_microdollars: run_cost.microdollars,
                                    session_cost_microdollars: priced_session_subtotal(session, &model),
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                                break 'run FinishReason::Aborted;
                            }
                            Err(error) => {
                                let ev = AgentEvent::CandidateRejected {
                                    usage: run_usage,
                                    run_cost_microdollars: run_cost.microdollars,
                                    session_cost_microdollars: priced_session_subtotal(session, &model),
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                                break 'run FinishReason::Failed(error);
                            }
                        }
                    }
                    break 'run FinishReason::Completed;
                }

                // A model can emit several independent observations in one
                // turn. Scan only the next contiguous run of exact,
                // host-classified read observations; every mutation, process,
                // delegation, extension, network, unknown, schema-invalid, or
                // sequential tool is a barrier. The live read predicate is
                // intentionally broader than the crash-replay predicate:
                // HostRead remains ambient authority and is never relabeled.
                let parallel_active_skills = session
                    .head()
                    .and_then(|head| session.resolve_active_skills(&head).ok())
                    .map(|state| state.active_skills)
                    .unwrap_or_default();
                let classification_context = ToolContext {
                    workspace: &sandbox.workspace,
                    sandbox: &sandbox,
                    execution_scope: &tool_scope,
                    resource_owner: &resource_owner,
                    active_skills: &parallel_active_skills,
                    registered_tools: &registered_tools,
                    progress: ToolProgressSink::null(),
                    cancellation: CancellationToken::default(),
                };
                // Defer only a complete, bounded batch of independent host
                // observations. Mixed/sync/effectful batches keep ordinary order.
                // Hard ceilings serialize tool accounting before another request.
                if model.responses_features().async_tools
                    && max_session_tokens.is_none() && max_session_cost_microdollars.is_none()
                    && calls.len() <= MAX_PARALLEL_READ_WAVE_WIDTH
                    && !abort.is_set()
                    && calls.iter().enumerate().all(|(index, call)| {
                        call.async_execution
                            && request_tool_defs.iter().any(|definition| definition.name == call.name && definition.async_execution)
                            && parallel_read_candidate(call, index, answer_only, output_truncated, &tool_map, &classification_context)
                    }) {
                    for (index, call) in calls.iter().enumerate() {
                        let invocation = match session.tool_invocation(index) {
                            Ok(handle) => handle,
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        };
                        stream_context.tool_started();
                        let event = AgentEvent::ToolStarted { id: call.id.clone(), name: call.name.clone(), args: call.arguments_value().expect("complete admitted arguments") };
                        notify_observers(&observers, &event); yield event;
                        background_tools.start(call.clone(), invocation, tool_map[&call.name].clone(),
                            tool_call_hooks.clone(), effect_broker.clone(), effect_run_id.clone(), tool_revision,
                            sandbox.clone(), tool_scope.clone(), resource_owner.clone(), parallel_active_skills.clone(),
                            registered_tools.clone(), background_cancellation.clone());
                    }
                    continue 'run;
                }
                let mut parallel_results: VecDeque<ParallelReadWaveExecution> = VecDeque::new();
                // Row 4.10: every finalized result of this assistant batch, in
                // emitted order, decides the batch's termination request.
                let mut termination_requests: Vec<bool> = Vec::with_capacity(calls.len());

                // Calls in one assistant response form a single batch. Do not
                // treat parallel or otherwise batched identical calls as a
                // no-progress loop; only compare against earlier responses.
                let batch_fingerprints: Vec<(String, String)> = calls
                    .iter()
                    .filter(|call| call.argument_error.is_none())
                    .filter_map(|call| {
                        call.arguments_value().ok().map(|args| {
                            (
                                call.name.clone(),
                                tool_call_arguments_fingerprint(&call.name, &args),
                            )
                        })
                    })
                    .collect();

                // ── Commit tool results in emitted order ───────────────────
                let mut call_index = 0usize;
                while call_index < calls.len() {
                    if parallel_results.is_empty()
                        && !abort.is_set()
                        && parallel_read_candidate(
                            &calls[call_index],
                            call_index,
                            answer_only,
                            output_truncated,
                            &tool_map,
                            &classification_context,
                        )
                    {
                        let mut wave_end = call_index;
                        while wave_end < calls.len()
                            && wave_end - call_index < MAX_PARALLEL_READ_WAVE_WIDTH
                            && parallel_read_candidate(
                                &calls[wave_end],
                                wave_end,
                                answer_only,
                                output_truncated,
                                &tool_map,
                                &classification_context,
                            )
                        {
                            wave_end += 1;
                        }
                        // A single eligible call gains no overlap and keeps the
                        // ordinary sequential path's hook/control behavior.
                        if wave_end - call_index > 1 {
                            // Only this admitted, bounded wave owns live slots.
                            // Over-limit/static refusals never allocate handles.
                            let invocation_handles = match (call_index..wave_end)
                                .map(|index| session.tool_invocation(index))
                                .collect::<Result<Vec<_>, _>>() {
                                Ok(handles) => handles,
                                Err(error) => break 'run FinishReason::Failed(error.into()),
                            };
                            // Row 3.5: one tool boundary per call in the wave.
                            // They are settled together once the wave resolves.
                            let mut wave_tool_guards =
                                Vec::with_capacity(wave_end - call_index);
                            for call in &calls[call_index..wave_end] {
                                wave_tool_guards.push(turn_context.begin_typed::<ToolSpan>(
                                    ToolAttributes {
                                        name: call.name.clone(),
                                    },
                                ));
                                let parsed = call
                                    .arguments_value()
                                    .expect("parallel read wave validates arguments");
                                stream_context.tool_started();
                                let ev = AgentEvent::ToolStarted {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    args: parsed,
                                };
                                notify_observers(&observers, &ev);
                                yield ev;
                            }

                            let operation = execute_parallel_read_wave(
                                &calls[call_index..wave_end],
                                &invocation_handles,
                                &tool_map,
                                &tool_call_hooks,
                                &effect_broker,
                                &effect_run_id,
                                tool_revision,
                                &sandbox,
                                &tool_scope,
                                &resource_owner,
                                &parallel_active_skills,
                                &registered_tools,
                                abort.cancellation.clone(),
                            );
                            tokio::pin!(operation);
                            let mut abort_observed = abort.is_set();
                            let completed = loop {
                                tokio::select! {
                                    biased;
                                    results = &mut operation => break results,
                                    _ = abort.wait(), if !abort_observed => {
                                        abort_observed = true;
                                    }
                                    control = control_rx.recv(), if control_open => match control {
                                        Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                        Some(Control::FollowUp(input)) => followups.push_back(input),
                                        Some(Control::FinishNow(input)) => {
                                            input.push_pending(&mut pending_steer);
                                            answer_only = true;
                                            finish_pending = true;
                                            context_capacity.invalidate();
                                        }
                                        Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                        Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                        Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                        Some(Control::Abort) => {
                                            abort.set();
                                            abort_observed = true;
                                        }
                                        None => control_open = false,
                                    },
                                }
                            };
                            for (guard, entry) in
                                wave_tool_guards.into_iter().zip(completed.iter())
                            {
                                if let Ok(output) = &entry.execution.result {
                                    if let Some(usage) = output.usage() {
                                        CompletionAttributes::usage(usage).record(&guard.span);
                                    }
                                }
                                guard.finish(tool_execution_failed(&entry.execution.result));
                            }
                            parallel_results.extend(completed);
                        }
                    }
                    let call = calls[call_index].clone();
                    let argument_error = call.argument_error;
                    let parsed = call.arguments_value();
                    let call_fingerprint = if argument_error.is_none() {
                        parsed.as_ref().ok().map(|args| {
                            (
                                call.name.clone(),
                                tool_call_arguments_fingerprint(&call.name, args),
                            )
                        })
                    } else {
                        None
                    };
                    let repeated_recently = call_fingerprint.as_ref().map_or(0, |fingerprint| {
                        recent_tool_calls
                            .iter()
                            .filter(|previous| *previous == fingerprint)
                            .count()
                    });
                    let should_annotate_repetition =
                        repeated_recently >= REPEATED_TOOL_CALL_THRESHOLD;
                    let (preexecuted, deferred_after) =
                        match parallel_results.pop_front() {
                            Some(ParallelReadWaveExecution { execution, after }) => {
                                (Some(execution), after)
                            }
                            None => (None, None),
                        };
                    let invocation = if argument_error.is_none()
                        && preexecuted.is_none()
                        && !answer_only
                        && !output_truncated
                        && call_index < MAX_TOOL_CALLS_PER_TURN
                        && !abort.is_set()
                        && tool_map.contains_key(&call.name)
                        && parsed.is_ok()
                    {
                        match session.tool_invocation(call_index) {
                            Ok(handle) => Some(handle),
                            Err(error) => break 'run FinishReason::Failed(error.into()),
                        }
                    } else {
                        None
                    };
                    let mut tool_guard: Option<SpanGuard> = None;
                    if preexecuted.is_none() {
                        // Row 3.5: one tool boundary per executed call, settled
                        // at the durable result boundary below.
                        tool_guard = Some(turn_context.begin_typed::<ToolSpan>(
                            ToolAttributes {
                                name: call.name.clone(),
                            },
                        ));
                        stream_context.tool_started();
                        let ev = AgentEvent::ToolStarted {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            args: parsed
                                .as_ref()
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }
                    let CompletedToolExecution {
                        result,
                        mut policy_decision,
                        duration,
                        mut progress_rx,
                        progress_sink,
                        cancellation_won,
                        started_unix_ms,
                        finished_unix_ms,
                    } = if let Some(argument_error) = argument_error {
                        // Do not classify effects, run hooks, or invoke the
                        // tool for a call already rejected by the request's
                        // schema snapshot. The paired static error is durable
                        // and safe to show back to the model.
                        rejected_argument_tool_execution(
                            argument_error,
                            &sandbox,
                            &effect_broker,
                        )
                    } else if let Some(execution) = preexecuted {
                        execution
                    } else {
                        // Create a fresh progress channel for every sequential
                        // call. Non-streaming tools simply never push into it.
                        let (progress_tx, mut progress_rx) =
                            mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
                        let progress_sink = ToolProgressSink::live(progress_tx);
                        let progress_sink = match &invocation {
                            Some(handle) => progress_sink.with_invocation(handle.clone()),
                            None => progress_sink,
                        };
                        let mut cancellation_won = false;
                        let start = std::time::Instant::now();
                        let started_at = Arc::new(AtomicU64::new(u64::MAX));
                        let started_at_marker = Arc::clone(&started_at);
                        let policy_decision_slot = Arc::new(Mutex::new(None));
                        // Row 4.8: the live panel's replaceable publications are
                        // paced for the whole call, and forced at its terminal
                        // boundary so a finished call never leaves stale state.
                        let mut live_preview = LivePreviewPacer::new();
                        // Row 4.7: the harness half of durable partial-output
                        // checkpoints. The tracker is created per invocation and
                        // dropped with the call, so a settled call has nothing
                        // left to republish.
                        #[cfg(any(unix, windows))]
                        let mut live_partial_output = partial_output_checkpoints.as_ref().and_then(|config| {
                            let mut resolved = config.clone();
                            if resolved.sink.is_none() {
                                resolved.sink = invocation.as_ref().map(|handle| {
                                    Arc::new(handle.clone()) as Arc<dyn crate::tool::PartialOutputCheckpointSink>
                                });
                            }
                            LivePartialOutput::for_call(&resolved, &call.name)
                        });
                        let result: Result<ToolOutput, ToolError> = if answer_only {
                            Err(ToolError::new(format!(
                                "tool call `{}` was not executed: the user requested an immediate final answer without tools",
                                call.name
                            )))
                        } else if output_truncated {
                            Err(ToolError::new(format!(
                                "tool call `{}` was not executed: the provider reached its output token limit, so the arguments may be truncated; re-issue the call with complete arguments",
                                call.name
                            )))
                        } else if call_index >= MAX_TOOL_CALLS_PER_TURN {
                            Err(ToolError::new(
                                "tool call skipped: per-turn tool-call limit reached",
                            ))
                    } else if abort.is_set() {
                        cancellation_won = true;
                        Err(cancelled_tool_error())
                    } else {
                        match (tool_map.get(&call.name), parsed) {
                            (None, _) => {
                                Err(ToolError::new(format!("unknown tool: {}", call.name)))
                            }
                            (Some(_), Err(_)) => {
                                let (error, decision) =
                                    invalid_tool_arguments_denial(&sandbox, &effect_broker);
                                *policy_decision_slot
                                    .lock()
                                    .expect("policy decision slot is not poisoned") = Some(decision);
                                Err(error)
                            }
                            (Some(tool), Ok(args)) => {
                                let active_skills = session
                                    .head()
                                    .and_then(|head| session.resolve_active_skills(&head).ok())
                                    .map(|state| state.active_skills)
                                    .unwrap_or_default();
                                let tool_ctx = ToolContext {
                                    workspace: &sandbox.workspace,
                                    sandbox: &sandbox,
                                    execution_scope: &tool_scope,
                                    resource_owner: &resource_owner,
                                    active_skills: &active_skills,
                                    registered_tools: &registered_tools,
                                    progress: progress_sink.clone(),
                                    cancellation: abort.cancellation.clone(),
                                };
                                let hook_arguments = args.clone();
                                let effect_committed = Arc::new(AtomicBool::new(false));
                                let committed_marker = Arc::clone(&effect_committed);
                                let policy_decision_marker = Arc::clone(&policy_decision_slot);
                                let operation = async {
                                    let admission = reserve_tool_effect(
                                        &effect_broker,
                                        tool.as_ref(),
                                        &call.name,
                                        &args,
                                        &tool_ctx,
                                        &resource_owner,
                                        &effect_run_id,
                                        tool_revision,
                                        &call.id,
                                        true,
                                    )
                                    .await;
                                    let ToolEffectAdmission {
                                        intent,
                                        reservation: effect_reservation,
                                        effect,
                                    } = match admission {
                                        Ok(admission) => admission,
                                        Err(ToolEffectAdmissionError { error, decision }) => {
                                            *policy_decision_marker
                                                .lock()
                                                .expect("policy decision slot is not poisoned") =
                                                Some(decision);
                                            return Err(error);
                                        }
                                    };
                                    for hook in &tool_call_hooks {
                                        if hook
                                            .before_tool_call(
                                                &call.name,
                                                &hook_arguments,
                                                &tool_ctx,
                                            )
                                            .await
                                            .is_err()
                                        {
                                            if tool_ctx.cancellation.is_cancelled() {
                                                return Err(cancelled_tool_error());
                                            }
                                            let (error, decision) = secondary_hook_denial(
                                                &sandbox,
                                                &effect_broker,
                                                Some(effect),
                                            );
                                            *policy_decision_marker
                                                .lock()
                                                .expect("policy decision slot is not poisoned") =
                                                Some(decision);
                                            return Err(error);
                                        }
                                    }
                                    if tool_ctx.cancellation.is_cancelled() {
                                        return Err(cancelled_tool_error());
                                    }
                                    let receipt = effect_reservation.commit(&intent).map_err(|error| {
                                        let (error, decision) = effect_reservation_commit_denial(
                                            &sandbox,
                                            &effect_broker,
                                            effect,
                                            &error,
                                        );
                                        *policy_decision_marker
                                            .lock()
                                            .expect("policy decision slot is not poisoned") =
                                            Some(decision);
                                        error
                                    })?;
                                    *policy_decision_marker
                                        .lock()
                                        .expect("policy decision slot is not poisoned") =
                                        Some(policy_decision(
                                            &sandbox,
                                            &effect_broker,
                                            Some(effect),
                                            Some(receipt.authorization()),
                                            None,
                                        ));
                                    committed_marker.store(true, Ordering::Release);
                                    started_at_marker
                                        .store(crate::session::now_unix_millis(), Ordering::Release);
                                    tool.execute(args, &tool_ctx).await
                                };
                                tokio::pin!(operation);
                                // Cancellation drops the pinned future, which
                                // kills any child process tree it spawned.
                                let outcome = loop {
                                    // Row 4.8's trailing timer: one deadline,
                                    // recomputed per wake, that publishes the
                                    // held replaceable state exactly once.
                                    let flush_at = live_preview
                                        .flush_deadline(std::time::Instant::now())
                                        .map(tokio::time::Instant::from_std);
                                    tokio::select! {
                                        biased;
                                        _ = abort.wait() => break None,
                                        r = &mut operation => break Some(r),
                                        c = control_rx.recv(), if control_open => match c {
                                            Some(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                            Some(Control::FollowUp(input)) => followups.push_back(input),
                                            Some(Control::FinishNow(input)) => {
                                                input.push_pending(&mut pending_steer);
                                                answer_only = true;
                                                finish_pending = true;
                                                context_capacity.invalidate();
                                            }
                                            Some(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                            Some(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                            Some(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                            Some(Control::Abort) => {
                                                abort.set();
                                                break None;
                                            }
                                            None => control_open = false,
                                        },
                                        progress = progress_rx.recv() => {
                                            if let Some(p) = progress {
                                                // `operation` can enqueue progress and synchronously
                                                // trigger cancellation during the same select poll,
                                                // after the biased abort branch was already checked.
                                                // Recheck before accepting semantic state.
                                                match settle_tool_progress(p, abort.is_set(), session) {
                                                    ProgressSettlement::Cancelled => break None,
                                                    ProgressSettlement::Settled => {}
                                                    ProgressSettlement::Emit(p) => {
                                                        // Row 4.7: publish the
                                                        // bounded live snapshot
                                                        // before the panel sees
                                                        // the chunk, so durability
                                                        // never lags the panel.
                                                        #[cfg(any(unix, windows))]
                                                        if let Some(checkpoints) =
                                                            live_partial_output.as_mut()
                                                        {
                                                            checkpoints.observe_progress(
                                                                &p,
                                                                std::time::Instant::now(),
                                                            );
                                                        }
                                                        if let Some(progress) = forward_tool_progress(
                                                            p,
                                                            &mut live_preview,
                                                            std::time::Instant::now(),
                                                        ) {
                                                            let ev = AgentEvent::ToolProgress {
                                                                id: call.id.clone(),
                                                                progress,
                                                            };
                                                            notify_observers(&observers, &ev);
                                                            yield ev;
                                                        }
                                                    }
                                                }
                                            }
                                        },
                                        snapshot = async {
                                            match &mut delegation_telemetry {
                                                Some(receiver) => next_delegation_snapshot(receiver).await,
                                                None => std::future::pending().await,
                                            }
                                        }, if delegation_telemetry.is_some() => {
                                            // Keep delegated-worker telemetry
                                            // flowing while a long root tool is
                                            // executing, not only while the root
                                            // streams from the provider.
                                            match snapshot {
                                                Some(snapshot) => {
                                                    let event =
                                                        AgentEvent::DelegationUpdated { snapshot };
                                                    notify_observers(&observers, &event);
                                                    yield event;
                                                }
                                                None => delegation_telemetry = None,
                                            }
                                        },
                                        _ = tokio::time::sleep_until(
                                            flush_at.unwrap_or_else(tokio::time::Instant::now)
                                        ), if flush_at.is_some() => {
                                            // Publish the collapsed latest
                                            // replaceable state once its pace
                                            // deadline passed. Nothing else is
                                            // ever held back.
                                            if let Some(decoration) = live_preview
                                                .take_due(std::time::Instant::now())
                                            {
                                                let ev = AgentEvent::ToolProgress {
                                                    id: call.id.clone(),
                                                    progress: ToolProgress::Decoration(decoration),
                                                };
                                                notify_observers(&observers, &ev);
                                                yield ev;
                                            }
                                        },
                                    }
                                };
                                let result = match outcome {
                                    Some(_) if abort.is_set() => {
                                        cancellation_won = true;
                                        Err(cancelled_tool_error())
                                    }
                                    Some(result) => result,
                                    None => {
                                        cancellation_won = true;
                                        Err(cancelled_tool_error())
                                    }
                                };
                                // Terminal boundary: the call is over, so the
                                // panel gets the collapsed latest decoration
                                // now, whatever the pace deadline says.
                                if let Some(decoration) =
                                    live_preview.settle(std::time::Instant::now())
                                {
                                    let ev = AgentEvent::ToolProgress {
                                        id: call.id.clone(),
                                        progress: ToolProgress::Decoration(decoration),
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                                if effect_committed.load(Ordering::Acquire) {
                                    let (output, is_error) = match &result {
                                        Ok(output) => (output.text.as_str(), output.is_error()),
                                        Err(error) => (error.message.as_str(), true),
                                    };
                                    for hook in &tool_call_hooks {
                                        hook.after_tool_call(
                                            &call.name,
                                            &hook_arguments,
                                            output,
                                            is_error,
                                            &tool_ctx,
                                        )
                                        .await;
                                    }
                                }
                                result
                            }
                        }
                        };
                        let policy_decision = policy_decision_slot
                            .lock()
                            .expect("policy decision slot is not poisoned")
                            .take();
                        let started_at_value = started_at.load(Ordering::Acquire);
                        let started_unix_ms =
                            (started_at_value != u64::MAX).then_some(started_at_value);
                        CompletedToolExecution {
                            result,
                            policy_decision,
                            duration: start.elapsed(),
                            started_unix_ms,
                            finished_unix_ms: Some(crate::session::now_unix_millis()),
                            progress_rx,
                            progress_sink,
                            cancellation_won,
                        }
                    };
                    if let Some(after) = deferred_after {
                        run_parallel_after_tool_hooks(
                            after,
                            &tool_call_hooks,
                            &result,
                            &sandbox,
                            &tool_scope,
                            &resource_owner,
                            &parallel_active_skills,
                            &registered_tools,
                            abort.cancellation.clone(),
                        )
                        .await;
                    }
                    let result = if should_annotate_repetition {
                        annotate_repeated_tool_result(result, repeated_recently)
                    } else {
                        result
                    };

                    apply_execution_policy_denial(&mut policy_decision, &result);

                    // Emit policy metadata before the durable result commit: a
                    // session-write failure must not hide a decision already
                    // made for this exact call.
                    if let Some(decision) = policy_decision {
                        let ev = AgentEvent::ToolPolicyDecision {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            decision,
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }

                    // ── COMMIT BOUNDARY ──────────────────────────────────
                    // Tool::execute resolved (or an immediate error was
                    // produced). Persist the result immediately before
                    // draining progress or checking abort. An abort
                    // received after this point cannot erase an already-
                    // committed result.
                    // Every tool owns the same configured output allowance.
                    // A large early result must never starve later successful
                    // calls in the same model turn. Structured media is lowered
                    // only when the active model/protocol can replay it safely.
                    // Announce tools that appeared as a consequence of this
                    // execution (extension/MCP registrations). Later requests
                    // exclude announced schemas under deferred tool loading.
                    let (_, snapshot_tools) = extension_host.tool_snapshot();
                    let newly_added: Vec<String> = snapshot_tools
                        .iter()
                        .map(|tool| tool.definition().name)
                        .filter(|name| !announced_tools.contains(name))
                        .collect();
                    if !newly_added.is_empty() {
                        announced_tools.extend(newly_added.iter().cloned());
                    }
                    // Recorded before the durable commit below, so the batch's
                    // termination decision can never be taken from a result
                    // that was not actually placed. A failed call has no
                    // result and never requests termination.
                    termination_requests
                        .push(result.as_ref().map(ToolOutput::terminates_run).unwrap_or(false));
                    let (message, accepted_media, text, is_error, details) = lower_tool_result(
                        call.id.clone(),
                        &result,
                        &model,
                        sandbox.max_output_bytes,
                        newly_added,
                    );
                    let owner_images = if owner_tool_images_enabled {
                        Some(ToolOutput::new("").with_owner_presentation_images(
                            lowered_tool_result_media(&message),
                        ))
                    } else {
                        None
                    };
                    if let Some(evidence) = terminal_gate_evidence.as_mut() {
                        evidence.record_action(&call.name, &call.arguments_json, is_error, &text);
                    }
                    if let Err(e) = session.append_with_metadata(
                        EntryValue::Message(Message::User(message)),
                        details.map(|tool_output| EntryMetadata {
                            tool_output: Some(tool_output),
                            tool_started_unix_ms: started_unix_ms,
                            tool_finished_unix_ms: finished_unix_ms,
                            ..EntryMetadata::default()
                        }),
                    ) {
                        break 'run FinishReason::Failed(e.into());
                    }
                    // Internal durable-delivery tools may provisionally lease
                    // work while executing. Acknowledge it only once the
                    // complete, untruncated result is in the session.
                    resolve_tool_delivery_after_persistence(&result, sandbox.max_output_bytes);

                    // ── Drain accepted progress before ToolFinished ───────
                    let mut drain_preview = LivePreviewPacer::new();
                    while let Ok(p) = progress_rx.try_recv() {
                        match settle_tool_progress(p, cancellation_won, session) {
                            ProgressSettlement::Cancelled => continue,
                            ProgressSettlement::Settled => {}
                            ProgressSettlement::Emit(p) => {
                                if let Some(progress) = forward_tool_progress(
                                    p,
                                    &mut drain_preview,
                                    std::time::Instant::now(),
                                ) {
                                    let ev = AgentEvent::ToolProgress {
                                        id: call.id.clone(),
                                        progress,
                                    };
                                    notify_observers(&observers, &ev);
                                    yield ev;
                                }
                            }
                        }
                    }
                    // The call is over: whatever replaceable state the drain
                    // collapsed reaches the panel before ToolFinished.
                    if let Some(decoration) = drain_preview.settle(std::time::Instant::now()) {
                        let ev = AgentEvent::ToolProgress {
                            id: call.id.clone(),
                            progress: ToolProgress::Decoration(decoration),
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }
                    // Report dropped progress if any.
                    let (dropped_bytes, dropped_events) = progress_sink.take_dropped();
                    if dropped_bytes > 0 || dropped_events > 0 {
                        let ev = AgentEvent::ToolProgress {
                            id: call.id.clone(),
                            progress: ToolProgress::Dropped {
                                bytes: dropped_bytes,
                                events: dropped_events,
                            },
                        };
                        notify_observers(&observers, &ev);
                        yield ev;
                    }

                    stream_context.tool_finished();
                    let result = match result {
                        Ok(output) => Ok(output
                            .without_media_payloads_for(accepted_media)
                            .with_is_error(is_error)),
                        Err(error) => Err(error),
                    };
                    // Pi's per-tool-result usage is billed turn accounting, not
                    // model context: it is added to the run's cumulative totals
                    // below and never to `turn_usage` or a context estimate.
                    let tool_usage = result
                        .as_ref()
                        .ok()
                        .and_then(|output| output.usage().copied());
                    let tool_failed = tool_execution_failed(&result);
                    let mut ev = AgentEvent::ToolFinished {
                        id: call.id.clone(),
                        result,
                        duration,
                    };
                    notify_observers(&observers, &ev);
                    if let (Some(images), AgentEvent::ToolFinished { result: Ok(output), .. }) =
                        (owner_images, &mut ev)
                    {
                        output.attach_owner_presentation_images(images);
                    }
                    yield ev;
                    if let Some(usage) = &tool_usage {
                        add_usage(&mut run_usage, usage);
                    }
                    if let Some(guard) = tool_guard {
                        if let Some(usage) = &tool_usage {
                            CompletionAttributes::usage(usage).record(&guard.span);
                        }
                        guard.finish(tool_failed);
                    }
                    call_index += 1;

                }
                for fingerprint in batch_fingerprints {
                    recent_tool_calls.push_back(fingerprint);
                    while recent_tool_calls.len() > MAX_RECENT_TOOL_CALLS {
                        recent_tool_calls.pop_front();
                    }
                }

                // Every emitted call now has a durable result, including calls
                // that were never started because the user aborted. Do not
                // enter another model turn after controlled cancellation.
                if abort.is_set() {
                    break 'run FinishReason::Aborted;
                }

                // Row 4.10: Pi's unanimity rule, applied to exactly one
                // assistant batch. Every emitted call already has a durable
                // result above, so a unanimous request ends the run instead of
                // entering another model turn; any sibling that did not ask to
                // stop keeps the batch going, and its result is never discarded.
                if batch_requests_termination(termination_requests.iter().copied()) {
                    // ToolFinished yields to the caller while admission is open.
                    // Drain and close under the same lock used by send(), just
                    // as for a natural terminal answer. Accepted user controls
                    // take precedence over a tool's request to stop.
                    let terminal = {
                        let mut admission = control_admission.lock().unwrap_or_else(|error| error.into_inner());
                        while control_open {
                            match control_rx.try_recv() {
                                Ok(Control::Steer(input)) => input.push_pending(&mut pending_steer),
                                Ok(Control::FollowUp(input)) => followups.push_back(input),
                                Ok(Control::FinishNow(input)) => {
                                    input.push_pending(&mut pending_steer);
                                    answer_only = true;
                                    finish_pending = true;
                                    context_capacity.invalidate();
                                }
                                Ok(Control::SetReasoning(selection)) => pending_reasoning = Some(selection),
                                Ok(Control::SetSteeringMode(mode)) => steering_mode = mode,
                                Ok(Control::SetFollowUpMode(mode)) => follow_up_mode = mode,
                                Ok(Control::Abort) => { abort.set(); break; }
                                Err(mpsc::error::TryRecvError::Empty) => break,
                                Err(mpsc::error::TryRecvError::Disconnected) => control_open = false,
                            }
                        }
                        pending_steer.retain(ReservedInput::is_pending);
                        let terminal = pending_steer.is_empty() && followups.is_empty()
                            && pending_reasoning.is_none() && !native.has_pending();
                        if terminal { *admission = false; }
                        terminal
                    };
                    if abort.is_set() {
                        break 'run FinishReason::Aborted;
                    }
                    if terminal {
                        break 'run FinishReason::Completed;
                    }
                    if pending_steer.is_empty() && !followups.is_empty() {
                        let queued = match follow_up_mode {
                            QueueDeliveryMode::All => followups.drain(..).collect::<Vec<_>>(),
                            QueueDeliveryMode::OneAtATime => vec![followups.pop_front().expect("follow-up queue is non-empty")],
                        };
                        let observation = ContextObservation {
                            tracker: &stream_context,
                            model: &model,
                            system: &system,
                            tools: if answer_only { &[][..] } else { tool_defs.as_slice() },
                        };
                        match deliver_control_inputs(queued, ControlDeliveryKind::FollowUp, session,
                            &control_prompt_metadata, &mut terminal_gate_evidence, &observation, Some(&abort)).await {
                            ControlDelivery::Completed { event } => {
                                if let Some(ev) = event { notify_observers(&observers, &ev); yield ev; }
                            }
                            ControlDelivery::Interrupted { event, finish } => {
                                if let Some(ev) = event { notify_observers(&observers, &ev); yield ev; }
                                break 'run finish;
                            }
                        }
                    }
                }

                if needs_continuation && !native.has_pending() {
                    let instruction = continuation_instruction(&stop_reason);
                    if let Err(e) = session.append(user_message(UserInput::from(instruction))) {
                        break 'run FinishReason::Failed(e.into());
                    }
                }
                // Context reconstruction coalesces the consecutive tool-result
                // entries into the provider-required single user message.
            };

            if session.has_unsettled_native_steering() {
                if let Err(error) = session.record_usage_uncertainty(model.endpoint.id.clone(), model.spec.id.clone(), "native_steering") { reason = FinishReason::Failed(error.into()); }
                let event = AgentEvent::ProviderUsageUncertain; notify_observers(&observers, &event); yield event;
            }
            if let Err(error) = native.cancel(session, &model) { reason = FinishReason::Failed(error); }

            // Every driven terminal cancels and pairs accepted background work.
            // Run drop instead aborts task handles; restart never replays them.
            background_cancellation.cancel();
            while !background_tools.is_empty() {
                match background_tools.settle_one(session, &model, &sandbox,
                    &stream_context, &mut run_usage, &mut terminal_gate_evidence).await {
                    Ok(events) => for event in events { notify_observers(&observers, &event); yield event; },
                    Err(error) => { reason = FinishReason::Failed(error); break; }
                }
            }

            // Row 3.5: settle the final turn before the run boundary. A turn
            // that never opened a provider attempt is not an error; one that
            // opened an attempt without a finished response is.
            if let Some(settled) = previous_turn.take() {
                settled.finish(
                    matches!(reason, FinishReason::Failed(_))
                        || (turn_attempt_opened && !turn_attempt_succeeded),
                );
            }
            *control_admission.lock().unwrap_or_else(|error| error.into_inner()) = false;
            control_rx.close();
            while control_rx.try_recv().is_ok() {}
            pending_steer.clear();
            followups.clear();
            // A fully driven prompt always leaves an explicit durable restore
            // point, including controlled abort/max-turn/failure outcomes. A
            // dropped stream is not complete and never reaches this boundary.
            // Failed provider turns also need an assistant boundary. Without
            // one, the next prompt is appended after the unresolved user task
            // and models commonly continue the stale request instead.
            if matches!(reason, FinishReason::Failed(_)) {
                if let Err(error) = close_failed_turn(session, &model) {
                    reason = FinishReason::Failed(error);
                }
            }
            if let Some(delegation) = &stream_delegation {
                // Session-scoped lifetime: the fleet survives this run. Mark
                // the detachment boundary explicitly, then mirror each
                // extension-owned worker's accounting delta into the root
                // ledger exactly once so surviving workers are never
                // double-counted and never lose accounting.
                delegation.detach_run();
                for delegated in delegation.delegated_usage_records() {
                    match mirror_delegated_uncertainty(session, &model, delegated.usage_uncertain) {
                        Ok(true) => {
                            let event = AgentEvent::ProviderUsageUncertain;
                            notify_observers(&observers, &event);
                            yield event;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            // Persistence failed, but observed uncertainty must
                            // still stop presentation from claiming complete cost.
                            let event = AgentEvent::ProviderUsageUncertain;
                            notify_observers(&observers, &event);
                            yield event;
                            reason = FinishReason::Failed(error.into());
                            break;
                        }
                    }
                    if let Err(error) = record_delegated_usage_once(session, DelegatedUsage {
                        agent_id: delegated.agent_id,
                        turn_count: delegated.turn_count,
                        tool_call_count: delegated.tool_call_count,
                        endpoint: model.endpoint.id.clone(),
                        model: model.spec.id.clone(),
                        usage: delegated.usage,
                        cost: delegated.cost,
                    }) {
                        reason = FinishReason::Failed(error.into());
                        break;
                    }
                }
                if let Some(receiver) = delegation_telemetry.as_mut() {
                    if receiver.has_changed().unwrap_or(false) {
                        let snapshot = { receiver.borrow_and_update().clone() };
                        if let Some(snapshot) = snapshot {
                            let event = AgentEvent::DelegationUpdated { snapshot };
                            notify_observers(&observers, &event);
                            yield event;
                        }
                    }
                }
                delegation.detach_telemetry();
            }
            // Capacity checks use the incremental total-only cache. Refresh the
            // detailed snapshot once at the settled boundary so observers retain
            // an accurate final breakdown without paying for it on every turn.
            let _ = observe_context_tracker(&stream_context, session, &model, &system, &tool_defs);
            let checkpoint_usage = (completed_turns > 0).then_some(run_usage);
            let checkpoint_cost = model
                .spec
                .pricing
                .as_ref()
                .filter(|_| run_cost.unpriced_operations == 0)
                .map(|_| run_cost.microdollars);
            if let Err(error) = session.checkpoint_with_telemetry(
                first_entry.clone(),
                checkpoint_usage,
                checkpoint_cost,
            ) {
                reason = FinishReason::Failed(error.into());
            }
            let head = session.head().unwrap_or(first_entry);
            stream_context.run_finished(&reason);
            // Row 3.5: the run span settles at the durable run boundary, after
            // every recovery and checkpoint path has finalized `reason`.
            run_guard.finish(matches!(reason, FinishReason::Failed(_)));
            stream_lifecycle.finished.store(true, Ordering::Release);
            let ev = AgentEvent::RunFinished { head, reason };
            notify_observers(&observers, &ev);
            yield ev;
        };

        Ok(Run {
            stream: Box::pin(stream),
            control,
            lifecycle,
            context,
            delegation: run_delegation,
        })
    }

    /// Declares the Agent's static tool overlay complete and releases queued
    /// dynamic extension catalog updates. Products should call this after
    /// installing collaboration or other post-construction host tools, before
    /// exposing the Agent for its first prompt.
    pub fn finalize_tool_surface(&self) {
        self.extensions.finalize_tool_surface();
    }

    /// Runs to completion, returning the aggregate output.
    ///
    /// A run that ends with [`FinishReason::Failed`] is returned as `Err`;
    /// aborted and max-turns runs return `Ok` with their reason.
    pub async fn complete(&mut self, input: impl Into<UserInput>) -> Result<RunOutput, AgentError> {
        let mut run = self.prompt(input).await?;
        let mut text = String::new();
        let mut media = Vec::new();
        // Output is provisional until its provider turn reaches `Finished`.
        // A retry invalidates only the current attempt, not output committed by
        // earlier tool turns in the same autonomous run.
        let mut committed_text_len = 0usize;
        let mut committed_media_len = 0usize;
        let mut usage = Usage::default();
        // Pi's per-tool-result usage is billed turn accounting, not model
        // context. A tool batch that runs after the last `TurnFinished` (a run
        // that ends on a tool call) is folded here so `RunOutput.usage` stays a
        // complete billed subtotal without ever becoming a context estimate.
        let mut trailing_tool_usage = Usage::default();
        let mut run_cost: u64 = 0;
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: delta,
                } => text.push_str(&delta),
                AgentEvent::OutputMedia {
                    media: output_media,
                    ..
                } => media.push(output_media),
                AgentEvent::ProviderRetry { .. } => {
                    text.truncate(committed_text_len);
                    media.truncate(committed_media_len);
                }
                AgentEvent::CandidateRejected {
                    usage: total,
                    run_cost_microdollars: cost,
                    ..
                } => {
                    text.truncate(committed_text_len);
                    media.truncate(committed_media_len);
                    usage = total;
                    trailing_tool_usage = Usage::default();
                    run_cost = cost;
                }
                AgentEvent::SteeringDelivered { .. }
                | AgentEvent::FollowUpDelivered { .. }
                | AgentEvent::CompactionStarted { .. }
                | AgentEvent::CompactionFinished { .. } => {}
                AgentEvent::ToolFinished {
                    result: Ok(output), ..
                } => {
                    if let Some(tool_usage) = output.usage() {
                        add_usage(&mut trailing_tool_usage, tool_usage);
                    }
                }
                AgentEvent::TurnFinished {
                    usage: total,
                    run_cost_microdollars: cost,
                    ..
                } => {
                    committed_text_len = text.len();
                    committed_media_len = media.len();
                    usage = total;
                    trailing_tool_usage = Usage::default();
                    run_cost = cost;
                }
                AgentEvent::RunFinished { head, reason } => {
                    return match reason {
                        FinishReason::Failed(e) => Err(e),
                        reason => {
                            let mut total_usage = usage;
                            add_usage(&mut total_usage, &trailing_tool_usage);
                            Ok(RunOutput {
                                text,
                                media,
                                usage: total_usage,
                                cost_microdollars: run_cost,
                                head,
                                reason,
                            })
                        }
                    };
                }
                AgentEvent::ToolProgress { .. } => {}
                _ => {}
            }
        }
        // Unreachable for a started run: the stream always ends with RunFinished.
        Err(AgentError::RunEnded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::DEFAULT_PREVIEW_MIN_EMIT_INTERVAL;
    use base64::Engine as _;

    fn image_input_fixture(large: bool) -> InputPart {
        let encoded = if large {
            // Valid opaque 4002x2 PNG: the fallback resizes it to <=4000px.
            "iVBORw0KGgoAAAANSUhEUgAAD6IAAAACCAYAAABIFvMzAAAAPUlEQVR4nO3OoQEAAAgDoP3/9EzeoIFAJ00KAAAAAAAAAAAAAAAAAAAAK9cBAAAAAAAAAAAAAAAAAAAAfhkmlU0biXxThgAAAABJRU5ErkJggg=="
        } else {
            "iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC"
        };
        InputPart::Media(Media::image_bytes(
            bytes::Bytes::from(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap(),
            ),
            "image/png".parse().unwrap(),
        ))
    }

    #[tokio::test]
    async fn explicit_model_image_limits_override_host_fallback() {
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        Arc::make_mut(&mut model.spec).preset.image_input_limits = Some(ImageInputLimits {
            max_width: 2,
            max_height: 2,
            max_bytes: 1_000,
        });
        let prepared = prepare_user_images(
            UserInput::from(vec![image_input_fixture(false)]),
            &model,
            None,
        )
        .await
        .unwrap();
        let InputPart::Media(Media::Image(image)) = &prepared.parts[0] else {
            panic!("image expected")
        };
        let ImageSource::Inline(bytes) = &image.source else {
            panic!("inline image expected")
        };
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 2);
    }

    #[tokio::test]
    async fn user_images_are_prepared_before_history_and_invalid_batches_are_atomic() {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            Session::create(directory.path().join("images.jsonl")).unwrap(),
            ExtensionHost::new(),
        );
        let invalid = UserInput::from(vec![
            image_input_fixture(false),
            InputPart::Media(Media::image_bytes(
                bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\ntruncated"),
                "image/png".parse().unwrap(),
            )),
        ]);
        assert!(matches!(
            agent.prompt(invalid).await,
            Err(AgentError::ImageInput(ImageInputError::InvalidImage))
        ));
        assert!(agent.session().entries().is_empty());
        let run = agent
            .prompt(UserInput::from(vec![
                InputPart::Text("describe".into()),
                image_input_fixture(true),
            ]))
            .await
            .unwrap();
        drop(run);
        let context = agent.session().context().unwrap();
        let Message::User(user) = &context[0] else {
            panic!("user history expected")
        };
        assert!(matches!(&user.content[0], UserPart::Text(text) if text == "describe"));
        let UserPart::Media(Media::Image(image)) = &user.content[1] else {
            panic!("image history expected")
        };
        let ImageSource::Inline(bytes) = &image.source else {
            panic!("inline image expected")
        };
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        assert!(width <= FALLBACK_IMAGE_LIMITS.max_width);
        assert_eq!(
            image.media_type.as_ref().map(octet_ai::Mime::essence_str),
            Some("image/png")
        );
    }

    #[tokio::test]
    async fn image_batches_are_bounded_before_decode_and_abort_before_append() {
        let directory = tempfile::tempdir().unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            Session::create(directory.path().join("bounded-images.jsonl")).unwrap(),
            ExtensionHost::new(),
        );
        let many = UserInput::from(
            (0..9)
                .map(|_| image_input_fixture(false))
                .collect::<Vec<_>>(),
        );
        assert!(matches!(
            agent.prompt(many).await,
            Err(AgentError::ImageInputBatchLimit)
        ));
        assert!(agent.session().entries().is_empty());
        let bytes = UserInput::from(
            (0..5)
                .map(|_| {
                    InputPart::Media(Media::image_bytes(
                        bytes::Bytes::from(vec![0; 4 * 1024 * 1024 + 1]),
                        "image/png".parse().unwrap(),
                    ))
                })
                .collect::<Vec<_>>(),
        );
        assert!(matches!(
            agent.prompt(bytes).await,
            Err(AgentError::ImageInputBatchLimit)
        ));
        assert!(agent.session().entries().is_empty());

        let abort = AbortFlag::default();
        abort.set();
        let bounded = UserInput::from(
            (0..8)
                .map(|_| image_input_fixture(true))
                .collect::<Vec<_>>(),
        );
        assert!(matches!(
            prepare_user_images(bounded, &agent.model, Some(&abort)).await,
            Err(AgentError::Cancelled)
        ));
        assert!(agent.session().entries().is_empty());
    }

    #[tokio::test]
    async fn invalid_queued_image_never_enters_history_and_releases_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("queued-images.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let tracker = ContextTracker::default();
        let observation = ContextObservation {
            tracker: &tracker,
            model: &model,
            system: "",
            tools: &[],
        };
        let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        control
            .try_steer(UserInput::from(vec![
                image_input_fixture(false),
                InputPart::Media(Media::image_bytes(
                    bytes::Bytes::from_static(b"invalid"),
                    "image/png".parse().unwrap(),
                )),
            ]))
            .unwrap();
        let Control::Steer(input) = rx.recv().await.unwrap() else {
            panic!("steering expected")
        };
        let result = deliver_control_inputs(
            vec![input],
            ControlDeliveryKind::Steering,
            &mut session,
            &EntryMetadata::default(),
            &mut None,
            &observation,
            None,
        )
        .await;
        assert!(matches!(
            result,
            ControlDelivery::Interrupted {
                event: None,
                finish: FinishReason::Failed(AgentError::ImageInput(ImageInputError::InvalidImage))
            }
        ));
        assert!(session.entries().is_empty());
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
        assert_eq!(
            control.pending_bytes.available_permits(),
            MAX_PENDING_CONTROL_BYTES
        );
    }

    fn test_run_control(byte_limit: usize) -> (RunControl, mpsc::Receiver<Control>) {
        let (tx, rx) = mpsc::channel(8);
        (
            RunControl {
                reasoning_model: None,
                ultra_observed: false,
                admission: Arc::new(Mutex::new(true)),
                tx,
                pending_count: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_CONTROL_INPUTS)),
                pending_bytes: Arc::new(tokio::sync::Semaphore::new(byte_limit)),
                abort: Arc::new(AbortFlag::default()),
            },
            rx,
        )
    }

    fn gate_candidate(text: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantPart::Text(text.to_owned())],
            model: octet_ai::ModelId("test".into()),
            protocol: Protocol::OpenAiChat,
        }
    }

    #[test]
    fn incomplete_terminal_response_diagnostic_is_content_free_and_bounded() {
        let hostile = "private-payload-\u{1b}[31m\n".repeat(512);
        let assistant = AssistantMessage {
            content: vec![
                AssistantPart::Text(hostile.clone()),
                AssistantPart::Reasoning(octet_ai::ReasoningPart {
                    text: Some(hostile.clone()),
                    state: None,
                }),
                AssistantPart::ToolCall(ToolCall {
                    async_execution: false,
                    id: octet_ai::ToolCallId(hostile.clone()),
                    name: hostile.clone(),
                    arguments_json: hostile.clone(),
                    argument_error: None,
                }),
            ],
            model: octet_ai::ModelId(hostile.clone()),
            protocol: Protocol::OpenAiChat,
        };
        let usage = Usage {
            output_tokens: u64::MAX,
            reasoning_tokens: u64::MAX,
            ..Usage::default()
        };
        let mut diagnostics = vec![
            octet_ai::Diagnostic {
                code: "chat_defaulted_stop_reason".to_owned(),
                message: hostile.clone(),
            },
            octet_ai::Diagnostic {
                code: "chat_usage_missing-untrusted-code".to_owned(),
                message: hostile.clone(),
            },
        ];
        for usage_missing in [false, true] {
            if usage_missing {
                diagnostics.push(octet_ai::Diagnostic {
                    code: "chat_usage_missing".to_owned(),
                    message: hostile.clone(),
                });
            }
            let reason = incomplete_terminal_response_reason(
                &assistant,
                &StopReason::Other(hostile.clone()),
                &usage,
                &diagnostics,
                u64::MAX,
            );
            assert!(reason.starts_with("provider returned reasoning but no answer text"));
            assert!(reason.contains("stop=other; chat_stop_defaulted=true"));
            assert_eq!(reason.contains("usage=not_reported"), usage_missing);
            assert_eq!(reason.contains("usage=canonical"), !usage_missing);
            assert_eq!(
                reason.contains("; output_tokens=18446744073709551615;"),
                !usage_missing
            );
            assert_eq!(
                reason.contains("reasoning_tokens=18446744073709551615;"),
                !usage_missing
            );
            assert!(reason.contains("request_max_output_tokens=18446744073709551615"));
            assert!(reason.len() < 320);
            let public = public_error_diagnostic(
                &AgentError::IncompleteResponse {
                    stop_reason: reason,
                },
                "test",
                "test",
            );
            assert!(public.contains("not automatically retried"));
            for forbidden in ["private-payload", "untrusted-code", "\u{1b}", "\n"] {
                assert!(!public.contains(forbidden));
            }
        }
    }

    #[test]
    fn compaction_summary_parts_reject_empty_whitespace_and_oversize() {
        for invalid in [
            "".to_owned(),
            " \n\t ".to_owned(),
            "x".repeat(MAX_COMPACTION_HANDOFF_BYTES + 1),
        ] {
            assert!(matches!(
                validate_compaction_summary_part(&invalid),
                Err(AgentError::IncompleteResponse { .. })
            ));
        }
        validate_compaction_summary_part("## Goal\ncontinue")
            .expect("normal summaries remain valid");

        let mut main = "## Goal\ncontinue".to_owned();
        let original = main.clone();
        assert!(matches!(
            append_compaction_turn_prefix(&mut main, " \n\t "),
            Err(AgentError::IncompleteResponse { .. })
        ));
        assert_eq!(main, original, "an invalid split prefix is never merged");
    }

    #[test]
    fn glm_sized_default_threshold_does_not_compact_a_120k_request() {
        let context_window = 1_310_720u64;
        let estimate = 120_000u64;
        let reserve = DEFAULT_COMPACTION_RESERVE_TOKENS;
        let over_capacity = estimate > context_window.saturating_sub(reserve);
        assert!(!over_capacity, "this case is not Overflow recovery");
        for (fraction, expected_threshold) in [(1.0, false), (0.09, true)] {
            let threshold = ((context_window as f64) * fraction).floor() as u64;
            let over_threshold = estimate.saturating_add(reserve) > threshold;
            assert_eq!(over_threshold, expected_threshold, "fraction={fraction}");
        }
    }

    struct CompactionSummaryScript {
        responses: Mutex<VecDeque<String>>,
        requests: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl octet_ai::HostStreamTransport for CompactionSummaryScript {
        async fn stream(
            &self,
            model: octet_ai::HostStreamModel,
            request: Request,
            _: Vec<octet_ai::Diagnostic>,
        ) -> Result<octet_ai::ResponseStream, AiError> {
            assert!(
                request.tools.is_empty(),
                "compaction summaries are tool-free"
            );
            self.requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let text = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted response");
            Ok(Box::pin(futures_util::stream::iter([
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::Finished(octet_ai::Response {
                    message: AssistantMessage {
                        content: vec![AssistantPart::Text(text)],
                        model: model.id,
                        protocol: model.protocol,
                    },
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    cost: None,
                    response_id: None,
                    responses_output: None,
                    deferred: None,
                    diagnostics: Vec::new(),
                })),
            ])))
        }
    }

    fn compaction_test_agent(
        directory: &std::path::Path,
        script: Arc<CompactionSummaryScript>,
    ) -> Agent {
        let mut session = Session::create(directory.join("compaction-guard.jsonl")).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("original user context".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("original assistant context".into())],
                model: octet_ai::ModelId("test".into()),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        let mut agent = active_tool_test_agent(directory, session, ExtensionHost::new());
        agent
            .client
            .register_host_stream_transport(agent.model.endpoint.id.clone(), script);
        // The tiny threshold forces the actual autonomous compaction path while
        // retaining enough context budget for the normal successful case.
        agent
            .set_compaction_token_policy(true, 0.000_01, 1)
            .unwrap();
        agent
    }

    struct ScriptedBitmapRenderer {
        calls: std::sync::atomic::AtomicUsize,
        frames: Vec<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl CompactionStrategy for Arc<ScriptedBitmapRenderer> {
        async fn render(&self, _: &str, text: &str, _: &str) -> Result<Vec<Vec<u8>>, String> {
            assert!(text.contains("original user context"));
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.frames.clone())
        }
    }

    fn bitmap_compaction_test_agent(
        directory: &std::path::Path,
        strategy: impl CompactionStrategy + 'static,
        script: Arc<CompactionSummaryScript>,
    ) -> Agent {
        let mut session = Session::create(directory.join("bitmap-compaction.jsonl")).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("original user context".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("original assistant context".into())],
                model: octet_ai::ModelId("test".into()),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        let mut extensions = ExtensionHost::new();
        extensions.compaction_strategy(strategy);
        let mut agent = active_tool_test_agent(directory, session, extensions);
        agent
            .client
            .register_host_stream_transport(agent.model.endpoint.id.clone(), script);
        agent
            .set_compaction_token_policy(true, 0.000_01, 1)
            .unwrap();
        agent
    }

    #[tokio::test]
    async fn vision_compaction_bypasses_parent_summary_and_bad_frames_keep_history() {
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC")
            .unwrap();
        for frames in [
            vec![png],
            vec![b"\x89PNG\r\n\x1a\ntruncated".to_vec()],
            vec![],
        ] {
            let directory = tempfile::tempdir().unwrap();
            let valid = frames.first().is_some_and(|frame| frame.len() > 30);
            let renderer = Arc::new(ScriptedBitmapRenderer {
                calls: std::sync::atomic::AtomicUsize::new(0),
                frames,
            });
            let script = Arc::new(CompactionSummaryScript {
                responses: Mutex::new(VecDeque::from(["normal answer".into()])),
                requests: std::sync::atomic::AtomicUsize::new(0),
            });
            let mut agent = bitmap_compaction_test_agent(
                directory.path(),
                Arc::clone(&renderer),
                Arc::clone(&script),
            );
            let result = agent.complete("new task").await;
            assert_eq!(renderer.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                script.requests.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(valid),
                "the parent model must not receive a compaction summary request"
            );
            assert_eq!(result.is_ok(), valid);
            assert_eq!(agent.session().has_snapcompact_context().unwrap(), valid);
            if valid {
                assert!(agent.session().context().unwrap().iter().any(|message| matches!(message,
                    Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Media(Media::Image(_))))
                )));
                Arc::make_mut(&mut agent.model.spec)
                    .capabilities
                    .input_modalities = octet_ai::ModalitySet::none();
                assert!(matches!(
                    agent.complete("text-only follow-up").await,
                    Err(AgentError::InvalidCompactionPolicy(_))
                ));
                assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
            } else {
                assert!(agent
                    .session()
                    .entries()
                    .iter()
                    .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
                let original = format!("{:?}", agent.session().context().unwrap());
                assert!(original.contains("original user context"));
                assert!(original.contains("original assistant context"));
            }
        }
    }

    struct SlowBitmapRenderer(watch::Sender<usize>);

    #[async_trait::async_trait]
    impl CompactionStrategy for SlowBitmapRenderer {
        async fn render(&self, _: &str, _: &str, _: &str) -> Result<Vec<Vec<u8>>, String> {
            self.0.send_modify(|calls| *calls += 1);
            tokio::time::sleep(Duration::from_secs(70)).await;
            Ok(vec![base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC")
                .unwrap()])
        }
    }

    #[tokio::test(start_paused = true)]
    async fn slow_sequential_bitmap_chunks_share_one_deadline_and_leave_history() {
        let directory = tempfile::tempdir().unwrap();
        let (tx, mut calls) = watch::channel(0usize);
        let script = Arc::new(CompactionSummaryScript {
            responses: Mutex::new(VecDeque::new()),
            requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut agent =
            bitmap_compaction_test_agent(directory.path(), SlowBitmapRenderer(tx), script);
        agent
            .session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("x".repeat(3_000))],
            })))
            .unwrap();
        let task = tokio::spawn(async move {
            let result = agent.complete("new task").await;
            (agent, result)
        });
        calls.changed().await.unwrap();
        tokio::time::advance(Duration::from_secs(70)).await;
        for _ in 0..10_000 {
            if *calls.borrow() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(*calls.borrow(), 2, "source must span at least two chunks");
        tokio::time::advance(Duration::from_secs(50)).await;
        let (agent, result) = task.await.unwrap();
        assert!(
            matches!(result, Err(AgentError::InvalidCompactionPolicy(ref error)) if error.contains("deadline"))
        );
        assert!(!agent.session().has_snapcompact_context().unwrap());
        assert!(agent
            .session()
            .entries()
            .iter()
            .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
        assert!(
            format!("{:?}", agent.session().context().unwrap()).contains("original user context")
        );
    }

    #[tokio::test]
    async fn two_bitmap_compactions_preserve_source_and_separate_transcript_sections() {
        let directory = tempfile::tempdir().unwrap();
        let renderer = Arc::new(ScriptedBitmapRenderer {
            calls: std::sync::atomic::AtomicUsize::new(0),
            frames: vec![base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC")
                .unwrap()],
        });
        let script = Arc::new(CompactionSummaryScript {
            responses: Mutex::new(VecDeque::from([
                "first answer".into(),
                "second answer".into(),
            ])),
            requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut agent =
            bitmap_compaction_test_agent(directory.path(), Arc::clone(&renderer), script);
        agent.complete("first new task").await.unwrap();
        let first = agent
            .session()
            .entries()
            .iter()
            .find_map(|entry| match &entry.value {
                EntryValue::Compaction {
                    snapcompact: Some(checkpoint),
                    ..
                } => Some(checkpoint.source_text.clone()),
                _ => None,
            })
            .expect("first bitmap checkpoint");
        agent.complete("second new task").await.unwrap();
        let sources = agent
            .session()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.value {
                EntryValue::Compaction {
                    snapcompact: Some(checkpoint),
                    ..
                } => Some(&checkpoint.source_text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sources.len(), 2);
        assert!(sources[1].starts_with(&first));
        assert!(
            sources[1].contains("\n\n[User]: first new task"),
            "{}",
            sources[1]
        );
        assert_eq!(renderer.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn bitmap_source_separates_previous_full_turn_and_split_turn_prefix() {
        let user = |text: &str| {
            Message::User(UserMessage {
                content: vec![UserPart::Text(text.into())],
            })
        };
        let preparation = HandoffPreparation {
            previous_summary: Some("[User]: previous".into()),
            messages: vec![user("new full turn")],
            turn_prefix_messages: vec![user("split prefix")],
            details: Default::default(),
        };
        assert_eq!(
            snapcompact_source(&preparation),
            "[User]: previous\n\n[User]: new full turn\n\n[User]: split prefix"
        );
    }

    #[tokio::test]
    async fn text_only_compaction_keeps_parent_summary_path() {
        let directory = tempfile::tempdir().unwrap();
        let renderer = Arc::new(ScriptedBitmapRenderer {
            calls: std::sync::atomic::AtomicUsize::new(0),
            frames: Vec::new(),
        });
        let script = Arc::new(CompactionSummaryScript {
            responses: Mutex::new(VecDeque::from([
                "## Goal\nvalid checkpoint".into(),
                "normal answer".into(),
            ])),
            requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut agent = bitmap_compaction_test_agent(
            directory.path(),
            Arc::clone(&renderer),
            Arc::clone(&script),
        );
        Arc::make_mut(&mut agent.model.spec)
            .capabilities
            .input_modalities = octet_ai::ModalitySet::none();
        assert!(agent.complete("new task").await.is_ok());
        assert_eq!(renderer.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(!agent.session().has_snapcompact_context().unwrap());
    }

    #[tokio::test]
    async fn provider_compaction_refuses_invalid_summary_without_discarding_context() {
        for invalid in [
            "".to_owned(),
            " \n\t ".to_owned(),
            "x".repeat(MAX_COMPACTION_HANDOFF_BYTES + 1),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let script = Arc::new(CompactionSummaryScript {
                responses: Mutex::new(VecDeque::from([invalid])),
                requests: std::sync::atomic::AtomicUsize::new(0),
            });
            let mut agent = compaction_test_agent(directory.path(), Arc::clone(&script));
            let error = agent.complete("new task").await.unwrap_err();
            assert!(matches!(error, AgentError::IncompleteResponse { .. }));
            assert_eq!(script.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(agent
                .session()
                .entries()
                .iter()
                .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
            let retained = format!("{:?}", agent.session().context().unwrap());
            assert!(retained.contains("original user context"));
            assert!(retained.contains("original assistant context"));
        }

        let directory = tempfile::tempdir().unwrap();
        let script = Arc::new(CompactionSummaryScript {
            responses: Mutex::new(VecDeque::from([
                "## Goal\nvalid checkpoint".into(),
                "normal answer".into(),
            ])),
            requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut agent = compaction_test_agent(directory.path(), Arc::clone(&script));
        let output = agent.complete("new task").await.unwrap();
        assert!(matches!(output.reason, FinishReason::Completed));
        assert_eq!(
            script.requests.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one summary request then one normal turn"
        );
        assert!(format!("{:?}", agent.session().context().unwrap()).contains("normal answer"));
        assert!(agent
            .session()
            .entries()
            .iter()
            .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
    }

    struct ToolBudgetTransport {
        calls: std::sync::atomic::AtomicUsize,
        expected_tool_count: usize,
    }

    #[async_trait::async_trait]
    impl octet_ai::HostStreamTransport for ToolBudgetTransport {
        async fn stream(
            &self,
            model: octet_ai::HostStreamModel,
            request: Request,
            _: Vec<octet_ai::Diagnostic>,
        ) -> Result<octet_ai::ResponseStream, AiError> {
            assert_eq!(request.tools.len(), self.expected_tool_count);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(futures_util::stream::iter([
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::Finished(octet_ai::Response {
                    message: AssistantMessage {
                        content: vec![AssistantPart::Text("accepted".into())],
                        model: model.id,
                        protocol: model.protocol,
                    },
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    cost: None,
                    response_id: None,
                    responses_output: None,
                    deferred: None,
                    diagnostics: Vec::new(),
                })),
            ])))
        }
    }

    #[test]
    fn tool_schema_budget_counts_exact_json_and_refuses_without_tool_rewriting() {
        require_tool_schema_budget(&[], 0).expect("zero budget permits no provider tools");
        let tools = vec![ToolDef {
            async_execution: false,
            name: "schema-tool".into(),
            description: "private description must not enter the diagnostic".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"payload": {"type": "string"}}}),
            constrained_sampling: None,
        }];
        let bytes = tool_schema_bytes(&tools);
        require_tool_schema_budget(&tools, bytes).expect("exactly at budget is accepted");
        let error = require_tool_schema_budget(&tools, bytes - 1).unwrap_err();
        assert!(matches!(
            &error,
            AgentError::ToolSchemaBudgetExceeded {
                actual_bytes,
                tool_count: 1,
                max_bytes,
            } if *actual_bytes == bytes && *max_bytes == bytes - 1
        ));
        let diagnostic = error.to_string();
        assert!(diagnostic.len() < 256);
        assert!(!diagnostic.contains("private description"));
        assert_eq!(tools[0].name, "schema-tool", "refusal never rewrites tools");
    }

    #[tokio::test]
    async fn tool_schema_budget_preflight_refuses_without_persisting_the_prompt_or_calling_provider(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let extensions = active_tool_test_extensions(&["schema"]);
        let mut agent = active_tool_test_agent(
            directory.path(),
            Session::create(directory.path().join("schema-budget.jsonl")).unwrap(),
            extensions,
        );
        let tool_count = agent.registered_tool_definitions().len();
        let budget = tool_schema_bytes(&agent.registered_tool_definitions());
        let transport = Arc::new(ToolBudgetTransport {
            calls: std::sync::atomic::AtomicUsize::new(0),
            expected_tool_count: tool_count,
        });
        agent
            .client
            .register_host_stream_transport(agent.model.endpoint.id.clone(), transport.clone());

        agent.set_tool_schema_budget_bytes(budget - 1);
        assert!(matches!(
            agent.prompt("retryable draft").await,
            Err(AgentError::ToolSchemaBudgetExceeded { .. })
        ));
        assert!(agent.session().entries().is_empty());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);

        agent.set_tool_schema_budget_bytes(budget);
        assert!(matches!(
            agent.complete("accepted draft").await.unwrap().reason,
            FinishReason::Completed
        ));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn terminal_gate_receipts_retain_first_and_last_twelve_online() {
        for total in [0usize, 24, 25, 10_000] {
            let mut evidence = TerminalGateEvidence::default();
            TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(0));
            for index in 0..total {
                evidence.record_action(
                    &format!("tool-{index}"),
                    &format!("args-{index}"),
                    index % 2 == 0,
                    &format!("result-{index}"),
                );
                assert_eq!(evidence.receipts.len(), (index + 1).min(24));
                assert_eq!(evidence.actions_omitted, (index + 1).saturating_sub(24));
            }
            assert_eq!(
                TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.get()),
                3 * total
            );
            let expected = if total <= 24 {
                (0..total).collect::<Vec<_>>()
            } else {
                (0..12).chain(total - 12..total).collect::<Vec<_>>()
            };
            let capsule = terminal_gate_capsule(&evidence, &gate_candidate("done"));
            let capsule: serde_json::Value = serde_json::from_str(&capsule).unwrap();
            assert_eq!(capsule["actions_omitted"], total.saturating_sub(24));
            let actions = capsule["actions"].as_array().unwrap();
            assert_eq!(actions.len(), expected.len());
            for (action, index) in actions.iter().zip(expected) {
                assert_eq!(action["tool"], format!("tool-{index}"));
                assert_eq!(action["arguments"], format!("args-{index}"));
                assert_eq!(action["result"], format!("result-{index}"));
                assert_eq!(
                    action["status"],
                    if index % 2 == 0 { "error" } else { "ok" }
                );
            }
        }
    }

    #[test]
    fn terminal_gate_projects_unicode_receipts_at_insertion_not_at_each_attempt() {
        let text = "界🙂e\u{301}".repeat(2_000);
        let mut evidence = TerminalGateEvidence::default();
        evidence.record_action("read", &text, false, &text);
        let receipt = &evidence.receipts[0];
        for (projected, limit) in [
            (&receipt.arguments, TERMINAL_GATE_ARGUMENT_LIMIT),
            (&receipt.result, TERMINAL_GATE_RESULT_LIMIT),
        ] {
            let chars = text.chars().collect::<Vec<_>>();
            let half = (limit - 32) / 2;
            let head = chars[..half].iter().collect::<String>();
            let tail = chars[chars.len() - half..].iter().collect::<String>();
            assert_eq!(
                projected,
                &format!("{head}\n[… 8000 chars total …]\n{tail}")
            );
            assert!(projected.chars().count() <= limit);
            assert!(projected.len() <= limit * 4);
        }
        let candidate = gate_candidate("done");
        TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(0));
        let first = terminal_gate_capsule(&evidence, &candidate);
        for _ in 0..10 {
            assert_eq!(terminal_gate_capsule(&evidence, &candidate), first);
        }
        // Repeated gate attempts project only the new candidate, never all
        // already-projected action arguments/results or retained requests.
        assert_eq!(TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.get()), 11);
        let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(parsed["actions"][0]["arguments"], receipt.arguments);
        assert_eq!(parsed["actions"][0]["result"], receipt.result);
        assert_eq!(text.chars().count(), 8_000);
    }

    #[test]
    fn terminal_gate_requests_bound_count_bytes_and_preserve_initial_and_latest() {
        for unit in ["x", "🙂"] {
            let body = unit.repeat(4_000);
            let request = |index| format!("request-{index}: {body}");
            let mut evidence = TerminalGateEvidence::default();
            for index in 0..1_000 {
                evidence.record_request(&request(index));
                assert!(evidence.requests.len() <= TERMINAL_GATE_REQUEST_LIMIT);
                assert!(evidence.request_bytes <= TERMINAL_GATE_REQUEST_BYTES);
                assert_eq!(
                    evidence.request_bytes,
                    evidence.requests.iter().map(String::len).sum::<usize>()
                );
                assert_eq!(
                    evidence.requests_omitted + evidence.requests.len(),
                    index + 1
                );
            }
            assert_eq!(evidence.requests[0], bounded_gate_text(&request(0), 3_000));
            let retained_suffix = evidence.requests.len() - 1;
            for (summary, index) in evidence
                .requests
                .iter()
                .skip(1)
                .zip(1_000 - retained_suffix..1_000)
            {
                assert_eq!(summary, &bounded_gate_text(&request(index), 3_000));
            }
            if unit == "🙂" {
                assert!(evidence.requests.len() < TERMINAL_GATE_REQUEST_LIMIT);
            } else {
                assert_eq!(evidence.requests.len(), TERMINAL_GATE_REQUEST_LIMIT);
            }
        }
        let mut empty = TerminalGateEvidence::default();
        for _ in 0..10_000 {
            empty.record_request("");
        }
        assert_eq!(empty.requests.len(), TERMINAL_GATE_REQUEST_LIMIT);
        assert_eq!(empty.requests_omitted, 10_000 - TERMINAL_GATE_REQUEST_LIMIT);
        assert_eq!(empty.request_bytes, 0);
    }

    #[test]
    fn terminal_gate_capsule_stays_bounded_across_repeated_requests_and_decisions() {
        // NUL takes six JSON bytes per character, worse than UTF-8 or quotes.
        let text = "\0".repeat(TERMINAL_GATE_TEXT_LIMIT);
        let candidate = gate_candidate(&text);
        let mut evidence = TerminalGateEvidence {
            prior_context: text.clone(),
            ..TerminalGateEvidence::default()
        };
        for index in 0..512usize {
            evidence.record_request(&text);
            evidence.record_action(&text, &text, index % 2 == 0, &text);
            if [23, 24, 255, 511].contains(&index) {
                let capsule = terminal_gate_capsule(&evidence, &candidate);
                assert!(capsule.len() <= TERMINAL_GATE_CAPSULE_BYTES);
                let parsed: serde_json::Value = serde_json::from_str(&capsule).unwrap();
                assert_eq!(
                    parsed["requests_omitted"],
                    index + 1 - evidence.requests.len()
                );
                assert_eq!(parsed["actions_omitted"], (index + 1).saturating_sub(24));
                assert_eq!(
                    parsed["requests"].as_array().unwrap().len(),
                    evidence.requests.len()
                );
                assert_eq!(terminal_gate_capsule(&evidence, &candidate), capsule);
            }
        }
    }

    #[tokio::test]
    async fn natural_run_has_no_terminal_gate_summary_projection_or_evidence_collection() {
        struct Script(Mutex<VecDeque<Vec<AssistantPart>>>);
        #[async_trait::async_trait]
        impl octet_ai::HostStreamTransport for Script {
            async fn stream(
                &self,
                model: octet_ai::HostStreamModel,
                _: Request,
                _: Vec<octet_ai::Diagnostic>,
            ) -> Result<octet_ai::ResponseStream, AiError> {
                let content = self.0.lock().unwrap().pop_front().expect("scripted turn");
                let stop_reason = if content
                    .iter()
                    .any(|part| matches!(part, AssistantPart::ToolCall(_)))
                {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                Ok(Box::pin(futures_util::stream::iter([
                    Ok(StreamEvent::Started { response_id: None }),
                    Ok(StreamEvent::Finished(octet_ai::Response {
                        message: AssistantMessage {
                            content,
                            model: model.id,
                            protocol: model.protocol,
                        },
                        stop_reason,
                        usage: Usage::default(),
                        cost: None,
                        response_id: None,
                        responses_output: None,
                        deferred: None,
                        diagnostics: Vec::new(),
                    })),
                ])))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            Session::create(directory.path().join("natural-evidence.jsonl")).unwrap(),
            ExtensionHost::new(),
        );
        agent.max_turns = Some(3);
        let arguments = serde_json::json!({"payload": "x".repeat(16_000)}).to_string();
        let script = Arc::new(Script(Mutex::new(VecDeque::from([
            vec![AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: octet_ai::ToolCallId("unknown-call".into()),
                name: "unregistered".into(),
                arguments_json: arguments.clone(),
                argument_error: None,
            })],
            vec![AssistantPart::Text("done".into())],
        ]))));
        agent
            .client
            .register_host_stream_transport(agent.model.endpoint.id.clone(), script.clone());
        let initial = UserInput::from("initial 🙂".repeat(2_000));
        TERMINAL_GATE_INITIAL_SUMMARIES.with(|count| count.set(0));
        TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.set(0));
        assert!(TerminalGateEvidence::for_run(
            CompletionPolicy::Natural,
            agent.session(),
            &initial
        )
        .unwrap()
        .is_none());
        let mut run = agent.prompt(initial).await.unwrap();
        run.control().steer("steer 🙂".repeat(2_000)).await.unwrap();
        let mut delivered = false;
        let mut tool_finished = false;
        let mut completed = false;
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::SteeringDelivered { messages } => {
                    assert_eq!(messages, vec!["steer 🙂".repeat(2_000)]);
                    delivered = true;
                }
                AgentEvent::ToolFinished { .. } => tool_finished = true,
                AgentEvent::RunFinished { reason, .. } => {
                    assert!(matches!(reason, FinishReason::Completed), "{reason:?}");
                    completed = true;
                }
                _ => {}
            }
        }
        drop(run);
        assert!(delivered && tool_finished && completed);
        assert!(script.0.lock().unwrap().is_empty());
        let context = agent.session().context().unwrap();
        assert_eq!(
            message_visible_text(&context[0]).unwrap(),
            "initial 🙂".repeat(2_000)
        );
        assert!(context.iter().any(|message| matches!(message,
            Message::Assistant(assistant) if assistant.content.iter().any(|part| matches!(part,
                AssistantPart::ToolCall(call) if call.arguments_json == arguments
            ))
        )));
        assert_eq!(TERMINAL_GATE_INITIAL_SUMMARIES.with(|count| count.get()), 0);
        assert_eq!(TERMINAL_GATE_TEXT_PROJECTIONS.with(|count| count.get()), 0);
    }

    #[tokio::test]
    async fn terminal_gate_control_evidence_preserves_delivery_and_reservations() {
        for policy in [CompletionPolicy::Natural, CompletionPolicy::TerminalGate] {
            let directory = tempfile::tempdir().unwrap();
            let mut session =
                Session::create(directory.path().join("control-evidence.jsonl")).unwrap();
            let model = octet_ai::ModelCatalog::builtin()
                .unwrap()
                .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
                .unwrap();
            let tracker = ContextTracker::default();
            let observation = ContextObservation {
                tracker: &tracker,
                model: &model,
                system: "",
                tools: &[],
            };
            let initial = UserInput::from("initial request");
            let mut evidence = TerminalGateEvidence::for_run(policy, &session, &initial).unwrap();
            session.append(user_message(initial)).unwrap();
            let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
            let mut expected = vec!["initial request".to_owned()];
            for index in 0..24 {
                let text = format!("control-{index}: {}", "🙂".repeat(4_000));
                let input = UserInput::from(text.clone());
                let bytes = control_input_bytes(&input);
                let kind = match index % 3 {
                    0 => {
                        control.steer(input).await.unwrap();
                        ControlDeliveryKind::Steering
                    }
                    1 => {
                        control.follow_up(input).await.unwrap();
                        ControlDeliveryKind::FollowUp
                    }
                    _ => {
                        control.finish_now(input).await.unwrap();
                        ControlDeliveryKind::Steering
                    }
                };
                let reserved = match rx.recv().await.unwrap() {
                    Control::Steer(input)
                    | Control::FollowUp(input)
                    | Control::FinishNow(input) => input,
                    _ => panic!("semantic control"),
                };
                assert_eq!(
                    control.pending_count.available_permits(),
                    MAX_PENDING_CONTROL_INPUTS - 1
                );
                assert_eq!(
                    control.pending_bytes.available_permits(),
                    MAX_PENDING_CONTROL_BYTES - bytes
                );
                let delivered = deliver_control_inputs(
                    vec![reserved],
                    kind,
                    &mut session,
                    &EntryMetadata::default(),
                    &mut evidence,
                    &observation,
                    None,
                )
                .await;
                let ControlDelivery::Completed { event: Some(event) } = delivered else {
                    panic!("durable delivery must succeed")
                };
                let messages = match event {
                    AgentEvent::SteeringDelivered { messages }
                    | AgentEvent::FollowUpDelivered { messages } => messages,
                    _ => panic!("delivery acknowledgement"),
                };
                assert_eq!(messages, vec![text.clone()]);
                expected.push(text);
                assert_eq!(
                    control.pending_count.available_permits(),
                    MAX_PENDING_CONTROL_INPUTS
                );
                assert_eq!(
                    control.pending_bytes.available_permits(),
                    MAX_PENDING_CONTROL_BYTES
                );
            }
            let persisted = session
                .context()
                .unwrap()
                .iter()
                .map(|message| message_visible_text(message).expect("complete delivered input"))
                .collect::<Vec<_>>();
            assert_eq!(persisted, expected);
            if let Some(evidence) = evidence {
                assert_eq!(evidence.requests.front().unwrap(), "initial request");
                assert_eq!(
                    evidence.requests.back().unwrap(),
                    &bounded_gate_text(expected.last().unwrap(), 3_000)
                );
                assert_eq!(
                    evidence.requests_omitted,
                    expected.len() - evidence.requests.len()
                );
                assert!(evidence.request_bytes <= TERMINAL_GATE_REQUEST_BYTES);
                assert!(evidence.requests_omitted > 0);
            } else {
                assert_eq!(policy, CompletionPolicy::Natural);
            }
        }
    }

    #[tokio::test]
    async fn steering_receipt_claim_linearizes_before_persistence() {
        let (control, _rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        let (prepared, receipt) = control
            .prepare_steer("claimed but not yet persisted")
            .unwrap();
        let payload = ReservedInput::Retractable(prepared).claim().unwrap();
        // The durable append has not happened, but delivery already owns this
        // exact payload. Neither receipt clone may recall it now.
        assert!(!receipt.is_pending());
        assert!(!receipt.clone().try_retract());
        assert!(!receipt.try_retract());
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS - 1
        );
        drop(payload);
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
    }

    #[tokio::test]
    async fn steering_receipt_racing_recall_and_claim_have_exactly_one_winner() {
        let (control, _rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        for _ in 0..64 {
            let (prepared, receipt) = control
                .prepare_steer("same text, separate authority")
                .unwrap();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let recalled = scope.spawn(|| {
                    barrier.wait();
                    receipt.try_retract()
                });
                barrier.wait();
                let payload = ReservedInput::Retractable(prepared).claim();
                assert_ne!(payload.is_some(), recalled.join().unwrap());
                drop(payload);
            });
            assert!(!receipt.is_pending());
            assert_eq!(
                control.pending_count.available_permits(),
                MAX_PENDING_CONTROL_INPUTS
            );
            assert_eq!(
                control.pending_bytes.available_permits(),
                MAX_PENDING_CONTROL_BYTES
            );
        }
    }

    #[tokio::test]
    async fn pending_controls_stay_reserved_after_ingress_drain_until_durable_delivery() {
        let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        let mut pending = Vec::new();
        for index in 0..MAX_PENDING_CONTROL_INPUTS {
            control.follow_up(format!("input-{index}")).await.unwrap();
            let Control::FollowUp(input) = rx.recv().await.unwrap() else {
                panic!("follow-up")
            };
            pending.push(input);
        }
        assert_eq!(control.pending_count.available_permits(), 0);
        assert!(matches!(
            control.steer("rejected").await,
            Err(AgentError::ControlQueueFull)
        ));
        assert!(matches!(
            control.finish_now("rejected").await,
            Err(AgentError::ControlQueueFull)
        ));
        // Mode/cancellation controls do not spend semantic-input reservations.
        control
            .set_follow_up_mode(QueueDeliveryMode::OneAtATime)
            .await
            .unwrap();
        assert!(matches!(rx.recv().await, Some(Control::SetFollowUpMode(_))));

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("controls.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let tracker = ContextTracker::default();
        let observation = ContextObservation {
            tracker: &tracker,
            model: &model,
            system: "",
            tools: &[],
        };
        let mut gate = None;
        let delivered = deliver_control_inputs(
            pending,
            ControlDeliveryKind::FollowUp,
            &mut session,
            &EntryMetadata::default(),
            &mut gate,
            &observation,
            None,
        )
        .await;
        let ControlDelivery::Completed {
            event: Some(AgentEvent::FollowUpDelivered { messages }),
        } = delivered
        else {
            panic!("durable delivery must be acknowledged")
        };
        assert_eq!(
            messages,
            (0..MAX_PENDING_CONTROL_INPUTS)
                .map(|i| format!("input-{i}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(session.entries().len(), MAX_PENDING_CONTROL_INPUTS);
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
        assert_eq!(
            control.pending_bytes.available_permits(),
            MAX_PENDING_CONTROL_BYTES
        );
        control.try_steer("accepted again").unwrap();
    }

    #[tokio::test]
    async fn control_byte_and_ingress_saturation_are_typed_and_rollback_reservations() {
        let input = UserInput::from(vec![InputPart::Media(Media::audio_bytes(
            bytes::Bytes::from_static(b"audio-payload"),
            octet_ai::AudioFormat::Wav,
        ))]);
        let bytes = control_input_bytes(&input);
        assert!(bytes >= b"audio-payload".len() + std::mem::size_of::<InputPart>());
        let (control, mut rx) = test_run_control(bytes);
        control.try_follow_up(input).unwrap();
        let held = rx.recv().await.unwrap();
        assert_eq!(control.pending_bytes.available_permits(), 0);
        assert!(matches!(
            control.try_steer("x"),
            Err(AgentError::ControlQueueFull)
        ));
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS - 1
        );
        control.abort();
        assert!(control.abort.is_set());
        drop(held);
        assert_eq!(control.pending_bytes.available_permits(), bytes);

        let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        for _ in 0..8 {
            control.try_steer("queued").unwrap();
        }
        let reserved = control.pending_bytes.available_permits();
        assert!(matches!(
            control.try_follow_up("full ingress"),
            Err(AgentError::ControlQueueFull)
        ));
        assert_eq!(control.pending_bytes.available_permits(), reserved);
        // A cancelled async admission was never accepted and frees its permit.
        use futures_util::FutureExt;
        assert!(control.steer("waiting").now_or_never().is_none());
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS - 8
        );
        rx.close();
        assert!(matches!(
            control.try_steer("ended"),
            Err(AgentError::RunEnded)
        ));
        drop(rx);
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
    }

    #[tokio::test]
    async fn pending_control_reservations_release_on_failed_persistence_and_run_abort_or_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failed-delivery.jsonl");
        let mut session = Session::create(&path).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let tracker = ContextTracker::default();
        let observation = ContextObservation {
            tracker: &tracker,
            model: &model,
            system: "",
            tools: &[],
        };
        let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
        control.try_steer("accepted before failure").unwrap();
        let Control::Steer(input) = rx.recv().await.unwrap() else {
            panic!("steer")
        };
        // A concurrent writer makes the session's observed-length fence fail.
        let mut other = Session::open(&path).unwrap();
        other.append(user_message("other writer".into())).unwrap();
        let result = deliver_control_inputs(
            vec![input],
            ControlDeliveryKind::Steering,
            &mut session,
            &EntryMetadata::default(),
            &mut None,
            &observation,
            None,
        )
        .await;
        assert!(matches!(
            result,
            ControlDelivery::Interrupted {
                event: None,
                finish: FinishReason::Failed(_)
            }
        ));
        assert_eq!(
            control.pending_count.available_permits(),
            MAX_PENDING_CONTROL_INPUTS
        );
        assert!(session.entries().is_empty());

        for abort in [false, true] {
            let path = directory.path().join(format!("run-{abort}.jsonl"));
            let mut agent = active_tool_test_agent(
                directory.path(),
                Session::create(path).unwrap(),
                ExtensionHost::new(),
            );
            let mut run = agent.prompt("start").await.unwrap();
            let control = run.control();
            for _ in 0..8 {
                control.try_follow_up("accepted").unwrap();
            }
            if abort {
                control.abort();
                let mut finished = 0;
                while let Some(event) = run.next().await {
                    if let AgentEvent::RunFinished { reason, .. } = event {
                        assert!(matches!(reason, FinishReason::Aborted));
                        finished += 1;
                        assert_eq!(
                            control.pending_count.available_permits(),
                            MAX_PENDING_CONTROL_INPUTS
                        );
                    }
                }
                assert_eq!(finished, 1);
            }
            drop(run);
            assert!(matches!(
                control.try_follow_up("after termination"),
                Err(AgentError::RunEnded)
            ));
            assert_eq!(
                control.pending_count.available_permits(),
                MAX_PENDING_CONTROL_INPUTS
            );
        }
    }

    #[test]
    fn bash_owner_retirement_waits_for_overlapping_agents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owner.jsonl");
        let first = active_tool_test_agent(
            directory.path(),
            Session::create(&path).unwrap(),
            ExtensionHost::new(),
        );
        let owner = first.resource_owner.clone();
        let mut second = active_tool_test_agent(
            directory.path(),
            Session::open(&path).unwrap(),
            ExtensionHost::new(),
        );
        assert_eq!(BASH_OWNER_LEASES.lock().unwrap()[&owner], 2);
        drop(first);
        assert_eq!(BASH_OWNER_LEASES.lock().unwrap()[&owner], 1);
        second
            .replace_session_at_idle(Session::open(&path).unwrap())
            .unwrap();
        assert_eq!(BASH_OWNER_LEASES.lock().unwrap()[&owner], 1);
        second
            .replace_session_at_idle(Session::create(directory.path().join("other.jsonl")).unwrap())
            .unwrap();
        assert!(!BASH_OWNER_LEASES.lock().unwrap().contains_key(&owner));
        let next_owner = second.resource_owner.clone();
        drop(second);
        assert!(!BASH_OWNER_LEASES.lock().unwrap().contains_key(&next_owner));
    }

    struct PromptTool {
        name: &'static str,
        snippet: Option<String>,
        guidelines: &'static [&'static str],
    }

    #[async_trait::async_trait]
    impl Tool for PromptTool {
        fn definition(&self) -> ToolDef {
            ToolDef {
                async_execution: false,
                name: self.name.to_owned(),
                description: format!("{} tool", self.name),
                parameters: serde_json::json!({"type": "object"}),
                constrained_sampling: None,
            }
        }

        fn prompt_snippet(&self) -> Option<&str> {
            self.snippet.as_deref()
        }

        fn prompt_guidelines(&self) -> &[&str] {
            self.guidelines
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolContext<'_>,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new("executed"))
        }
    }

    #[test]
    fn tool_prompt_section_renders_snippets_and_guidelines_and_skips_silent_tools() {
        let listed = PromptTool {
            name: "listed",
            snippet: Some("run the thing".to_owned()),
            guidelines: &["prefer flags", "report failures"],
        };
        let silent = PromptTool {
            name: "silent",
            snippet: None,
            guidelines: &["never rendered"],
        };
        let section = render_tool_prompt_section([&listed as &dyn Tool, &silent as &dyn Tool])
            .expect("one contributing tool renders a section");
        assert_eq!(
            section,
            "Available tools:\n- listed: run the thing\n  - prefer flags\n  - report failures"
        );
        // A registration that contributes nothing must not enlarge a prompt.
        assert!(render_tool_prompt_section([&silent as &dyn Tool]).is_none());
        assert!(render_tool_prompt_section(Vec::<&dyn Tool>::new()).is_none());
    }

    #[test]
    fn tool_prompt_section_is_bounded_on_a_character_boundary() {
        let oversized = "é".repeat(MAX_TOOL_PROMPT_SECTION_BYTES);
        let huge = PromptTool {
            name: "huge",
            snippet: Some(oversized),
            guidelines: &[],
        };
        let section = render_tool_prompt_section([&huge as &dyn Tool]).unwrap();
        assert!(
            section.len() <= MAX_TOOL_PROMPT_SECTION_BYTES,
            "the section must respect its byte budget: {}",
            section.len()
        );
        assert!(
            section.ends_with('…'),
            "truncation is marked: {:?}",
            &section[section.len().saturating_sub(8)..]
        );
        assert!(
            section.is_char_boundary(section.len()),
            "a truncated section stays valid UTF-8"
        );
        // Truncation happens past the header and first entry, so the model still
        // sees which tool the elided detail belongs to.
        assert!(section.starts_with("Available tools:\n- huge: é"));
    }

    #[test]
    fn prepared_turn_rejects_stale_durable_head_system_and_tool_generation() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("prepared-turn.jsonl")).unwrap();
        let request = Request {
            system: Some("system".into()),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(16),
            temperature: None,
            stop: Vec::new(),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            responses: None,
            output_format: OutputFormat::Text,
            output_modalities: OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: CacheRetention::default(),
            session_id: Some("session".into()),
        };
        let prepared = PreparedTurn::new(session.head(), "system".into(), 7, request, 3);

        assert!(prepared.is_current(&session, "system", 7));
        assert!(!prepared.is_current(&session, "changed", 7));
        assert!(!prepared.is_current(&session, "system", 8));

        session
            .append(user_message(UserInput::from("new durable input")))
            .unwrap();
        assert!(!prepared.is_current(&session, "system", 7));
    }

    #[test]
    fn assistant_persistence_context_ignores_provider_metadata() {
        use octet_ai::{ModelId, ProviderPartMetadata};

        let assistant = AssistantMessage {
            content: vec![
                AssistantPart::Text("visible".into()),
                AssistantPart::ProviderMetadata(ProviderPartMetadata::GoogleThoughtSignature {
                    signature: "opaque-continuation".into(),
                }),
            ],
            model: ModelId("gemini-test".into()),
            protocol: Protocol::GoogleGenerativeAi,
        };

        let context =
            assistant_persistence_context("run", "owner", &assistant, StopReason::EndTurn);
        assert_eq!(context.text_bytes, "visible".len());
        assert_eq!(context.tool_call_count, 0);
        assert_eq!(context.reasoning_part_count, 0);
        assert_eq!(context.media_part_count, 0);
    }

    #[test]
    fn idle_session_replacement_updates_the_durable_owner() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.jsonl");
        let replacement_path = directory.path().join("replacement.jsonl");
        let first = Session::create(&first_path).unwrap();
        let replacement = Session::create(&replacement_path).unwrap();
        let replacement_owner = replacement.resource_owner_key();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let mut agent = Agent::new(AgentConfig {
            client: AiClient::new(),
            model,
            session: first,
            system: "system".into(),
            sandbox: SandboxConfig::new(directory.path()),
            effect_broker: EffectBroker::default(),
            extensions: ExtensionHost::new(),
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        let first_owner = agent.resource_owner.clone();
        agent.set_prompt_display_text(Some("old session draft".into()));

        agent.replace_session_at_idle(replacement).unwrap();

        assert_eq!(agent.session().path(), replacement_path);
        assert_eq!(agent.resource_owner, replacement_owner);
        assert_eq!(agent.session_id, replacement_owner);
        assert_eq!(agent.prompt_display_text, None);
        assert_ne!(agent.resource_owner, first_owner);
    }

    #[test]
    fn request_output_uses_provider_ceiling_then_clamps_to_remaining_context() {
        assert_eq!(
            resolve_request_max_output_tokens(200_000, 20_000, 65_536),
            65_536
        );
        // The remaining window is 30_000; the reserved headroom is 1% of the
        // window (2_000), so the request never sits on the boundary.
        assert_eq!(
            resolve_request_max_output_tokens(200_000, 170_000, 65_536),
            28_000
        );
    }

    /// Regression for a real local vLLM rejection: the model's window is
    /// 131_072, octet's estimate was one token below the provider's count, and
    /// the requested output filled the gap exactly, so the provider refused with
    /// `prompt + requested > window`. A provider that counts one token more than
    /// the estimate must still fit.
    #[test]
    fn request_output_keeps_estimator_slack_for_a_locally_served_model() {
        let window = 131_072;
        let estimated_input = 100_176;
        let requested = resolve_request_max_output_tokens(window, estimated_input, window);
        assert_eq!(requested, 29_586);
        // Provider-side counting differences of this size are covered.
        for provider_input in [
            estimated_input + 1,
            estimated_input + 512,
            estimated_input + 1_310,
        ] {
            assert!(
                provider_input + requested <= window,
                "provider input {provider_input} + requested {requested} exceeded {window}"
            );
        }
        // A prompt that already fills or exceeds the window reserves nothing and
        // cannot fabricate a negative cap.
        assert_eq!(resolve_request_max_output_tokens(window, window, window), 0);
        assert_eq!(
            resolve_request_max_output_tokens(window, window + 5_000, window),
            0
        );
        // Small windows keep a proportionate reserve rather than a fixed bite.
        assert_eq!(request_output_headroom(8_192), 256);
        assert_eq!(request_output_headroom(131_072), 1_310);
        assert_eq!(request_output_headroom(1_048_576), 4_096);
    }

    #[test]
    fn provisional_delivery_rolls_back_when_generic_tool_output_limiting_truncates_it() {
        use std::sync::atomic::{AtomicI8, Ordering};

        let resolution = Arc::new(AtomicI8::new(0));
        let committed = Arc::clone(&resolution);
        let rolled_back = Arc::clone(&resolution);
        let result: Result<ToolOutput, ToolError> = Ok(ToolOutput::new("x".repeat(128))
            .with_delivery_commit(
                move || committed.store(1, Ordering::SeqCst),
                move || rolled_back.store(-1, Ordering::SeqCst),
            ));
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let (_, _, persisted_text, _, _) = lower_tool_result(
            octet_ai::ToolCallId("delivery".into()),
            &result,
            &model,
            32,
            Vec::new(),
        );
        assert_ne!(persisted_text, result.as_ref().unwrap().text);
        assert!(persisted_text.len() <= 32);

        resolve_tool_delivery_after_persistence(&result, 32);
        assert_eq!(resolution.load(Ordering::SeqCst), -1);
    }

    #[test]
    fn repeated_tool_annotation_is_bounded_and_model_visible() {
        let result = annotate_repeated_tool_result(Ok(ToolOutput::new("result")), 2).unwrap();
        assert!(result.text.contains("exact call repeated 3x"));
        assert_eq!(
            result
                .content_parts()
                .iter()
                .filter_map(|part| match part {
                    ToolOutputContentPart::Text(text) => Some(text.as_str()),
                    ToolOutputContentPart::Media(_) => None,
                })
                .collect::<String>(),
            result.text
        );
        assert!(
            !annotate_repeated_tool_result(Ok(ToolOutput::new("result")), 1)
                .unwrap()
                .text
                .contains("diagnostic")
        );
    }

    #[test]
    fn repeated_tool_annotation_preserves_machine_readable_output() {
        let original = r#"{"timed_out":false,"messages":[]}"#;
        let result = annotate_repeated_tool_result(Ok(ToolOutput::new(original)), 2).unwrap();
        assert_eq!(result.text, original);
    }

    #[test]
    fn malformed_registered_arguments_create_a_secret_safe_policy_denial() {
        let workspace = tempfile::tempdir().unwrap();
        let sandbox = SandboxConfig::new(workspace.path());
        let broker = EffectBroker::new(crate::effect::EffectPolicy::Controlled);
        let sensitive_argument = "sensitive-argument-marker";
        let call = ToolCall {
            async_execution: false,
            id: octet_ai::ToolCallId("call_malformed".into()),
            name: "bash".into(),
            arguments_json: format!(r#"{{"command":"{sensitive_argument}""#),
            argument_error: None,
        };
        assert!(call.arguments_value().is_err());

        let (error, decision) = invalid_tool_arguments_denial(&sandbox, &broker);

        assert_eq!(
            error.policy_denial_code(),
            Some(ToolPolicyDenialCode::InvalidToolArguments)
        );
        assert_eq!(error.message, "invalid tool arguments");
        assert_eq!(decision.effect, None);
        assert!(!decision.allowed);
        assert_eq!(decision.authorization, None);
        assert_eq!(
            decision.denial_code,
            Some(ToolPolicyDenialCode::InvalidToolArguments)
        );
        let diagnostic = serde_json::to_string(&decision).unwrap();
        assert!(!diagnostic.contains(sensitive_argument));
        assert!(!error.message.contains(sensitive_argument));
    }

    #[test]
    fn reservation_commit_rejection_keeps_final_policy_denied() {
        let workspace = tempfile::tempdir().unwrap();
        let sandbox = SandboxConfig::new(workspace.path());
        let broker = EffectBroker::new(crate::effect::EffectPolicy::Controlled);
        let (error, decision) = effect_reservation_commit_denial(
            &sandbox,
            &broker,
            ToolEffect::WorkspaceMutation,
            &crate::effect::EffectBrokerError::GrantRejected,
        );

        assert_eq!(
            error.policy_denial_code(),
            Some(ToolPolicyDenialCode::EffectReservationCommitDenied)
        );
        assert_eq!(error.message, "effect reservation could not be committed");
        assert_eq!(decision.effect, Some(ToolEffect::WorkspaceMutation));
        assert!(!decision.allowed);
        assert_eq!(decision.authorization, None);
        assert_eq!(
            decision.denial_code,
            Some(ToolPolicyDenialCode::EffectReservationCommitDenied)
        );
    }

    #[test]
    fn execution_policy_denial_replaces_prior_admission() {
        let workspace = tempfile::tempdir().unwrap();
        let mut decision = Some(ToolPolicyDecision {
            effect: Some(ToolEffect::WorkspaceRead),
            allowed: true,
            authorization: Some(crate::effect::EffectAuthorization::Policy),
            denial_code: None,
            policy: SandboxConfig::new(workspace.path())
                .effective_tool_policy(crate::effect::EffectPolicy::Controlled),
        });
        let result: Result<ToolOutput, ToolError> = Err(ToolError::policy_denied(
            ToolPolicyDenialCode::WorkspaceConfinement,
            "path escapes the workspace",
        ));

        apply_execution_policy_denial(&mut decision, &result);

        let decision = decision.unwrap();
        assert!(!decision.allowed);
        assert_eq!(decision.authorization, None);
        assert_eq!(
            decision.denial_code,
            Some(ToolPolicyDenialCode::WorkspaceConfinement)
        );
    }

    #[test]
    fn repeated_tool_annotation_preserves_policy_denial_code() {
        let error = annotate_repeated_tool_result(
            Err(ToolError::policy_denied(
                ToolPolicyDenialCode::WorkspaceConfinement,
                "path escapes the workspace",
            )),
            REPEATED_TOOL_CALL_THRESHOLD,
        )
        .unwrap_err();
        assert_eq!(
            error.policy_denial_code(),
            Some(ToolPolicyDenialCode::WorkspaceConfinement)
        );
    }

    #[test]
    fn response_header_failures_are_not_automatically_replayed() {
        for timeout in [false, true] {
            let error = AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::ResponseHeaders,
                timeout,
                message: "response headers unavailable".into(),
            });
            assert!(!retryable_before_generation(&error));
            assert!(!retryable_stream_start(&error));
            assert_eq!(provider_retry_limit(&error), 0);
        }
    }

    #[test]
    fn body_timeout_is_not_automatically_retried() {
        let error = AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: true,
            message: "stream idle deadline reached".into(),
        });
        assert!(!retryable_stream_start(&error));
        assert_eq!(provider_retry_limit(&error), 0);
    }

    #[test]
    fn context_deadline_and_throttling_are_not_misclassified_as_overflow() {
        let deadline = AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Body,
            timeout: true,
            message: "context deadline exceeded".into(),
        });
        assert!(!looks_like_context_error(&deadline));

        let throttled = AiError::Provider(octet_ai::ProviderError {
            code: Some("rate_limit_exceeded".into()),
            kind: Some("throttled".into()),
            message: "context window exceeded in shared capacity".into(),
            request_id: None,
        });
        assert!(!looks_like_context_error(&throttled));
    }

    /// A strict local/self-hosted server rejects an over-long request with a
    /// plain 400. That is a request-size condition compaction can repair, not a
    /// permanent policy/auth/quota rejection, so every shape it can take must
    /// reach the compaction path.
    #[test]
    fn request_size_rejections_reach_the_compaction_path_in_every_server_shape() {
        const VLLM: &str = "This model's maximum context length is 131072 tokens. However, \
you requested 30896 output tokens and your prompt contains at least 100177 input tokens, \
for a total of at least 131073 tokens. Please reduce the length of the input prompt or the \
number of requested output tokens. (parameter=input_tokens, value=100177)";
        let shapes = [
            // Bare status with no machine-readable provider code.
            AiError::Http(octet_ai::HttpError {
                status: "400".parse().unwrap(),
                request_id: None,
                retry_after: None,
                provider_code: None,
                body_snippet: Some(format!(
                    r#"{{"object":"error","message":"{VLLM}","type":"BadRequestError","code":400}}"#
                )),
                retryable: false,
            }),
            // Body carried a numeric machine-readable code.
            AiError::Http(octet_ai::HttpError {
                status: "400".parse().unwrap(),
                request_id: None,
                retry_after: None,
                provider_code: Some("400".into()),
                body_snippet: Some(VLLM.into()),
                retryable: false,
            }),
            // Canonical provider envelope: code 400, kind BadRequestError.
            AiError::Provider(octet_ai::ProviderError {
                code: Some("400".into()),
                kind: Some("BadRequestError".into()),
                message: VLLM.into(),
                request_id: None,
            }),
            // Some servers answer 413/422 for the same condition.
            AiError::Http(octet_ai::HttpError {
                status: "413".parse().unwrap(),
                request_id: None,
                retry_after: None,
                provider_code: Some("413".into()),
                body_snippet: Some(VLLM.into()),
                retryable: false,
            }),
        ];
        for (index, error) in shapes.into_iter().enumerate() {
            assert!(
                looks_like_context_error(&error),
                "shape {index} did not reach the compaction path: {error:?}"
            );
        }
        // A genuine policy/auth/quota/not-found rejection in the same envelope
        // still vetoes, because compaction cannot repair it.
        for (code, kind) in [
            (Some("invalid_prompt"), None),
            (Some("cyber_policy"), None),
            (Some("invalid_api_key"), None),
            (Some("insufficient_quota"), None),
            (Some("401"), None),
            (Some("403"), None),
            (Some("404"), None),
        ] {
            let error = AiError::Provider(octet_ai::ProviderError {
                code: code.map(str::to_owned),
                kind: kind.map(str::to_owned),
                message: VLLM.into(),
                request_id: None,
            });
            assert!(
                !looks_like_context_error(&error),
                "{code:?}/{kind:?} must stay vetoed"
            );
        }
        // 408/429 are excluded from the permanent set by design (they are
        // connectivity/rate conditions), so a bare numeric 429 carrying an
        // explicit context-overflow message still reaches the compaction path:
        // the server stated the request no longer fits.
        let throttled_with_context_text = AiError::Provider(octet_ai::ProviderError {
            code: Some("429".into()),
            kind: None,
            message: VLLM.into(),
            request_id: None,
        });
        assert!(looks_like_context_error(&throttled_with_context_text));
        // A named rate-limit rejection still never destroys context.
        let throttled = AiError::Provider(octet_ai::ProviderError {
            code: Some("rate_limit_exceeded".into()),
            kind: None,
            message: VLLM.into(),
            request_id: None,
        });
        assert!(!looks_like_context_error(&throttled));
    }

    #[test]
    fn provider_validation_errors_do_not_retry_but_transient_failures_do() {
        let validation = AiError::Provider(octet_ai::ProviderError {
            code: Some("400".into()),
            kind: Some("Bad Request".into()),
            message: "reasoning_effort is invalid".into(),
            request_id: None,
        });
        assert!(!retryable_stream_start(&validation));

        for transient in [
            octet_ai::ProviderError {
                code: Some("503".into()),
                kind: Some("server_error".into()),
                message: "temporarily unavailable".into(),
                request_id: None,
            },
            octet_ai::ProviderError {
                code: Some("rate_limit_exceeded".into()),
                kind: Some("overloaded".into()),
                message: "try again".into(),
                request_id: None,
            },
        ] {
            assert!(retryable_stream_start(&AiError::Provider(transient)));
        }
    }

    #[test]
    fn provider_retry_diagnostics_include_bounded_operational_details() {
        let model = tool_media_model(Protocol::OpenAiChat, octet_ai::ModalitySet::none());
        let errors = [
            AiError::Http(octet_ai::HttpError {
                status: http::StatusCode::TOO_MANY_REQUESTS,
                request_id: Some("req-429".into()),
                retry_after: Some(Duration::from_secs(3)),
                provider_code: Some("rate_limit_exceeded".into()),
                body_snippet: Some(r#"{"error":{"message":"temporarily rate limited"}}"#.into()),
                retryable: true,
            }),
            AiError::Provider(octet_ai::ProviderError {
                code: Some("upstream_error".into()),
                kind: Some("server_error".into()),
                message: "upstream temporarily unavailable".into(),
                request_id: Some("req-stream".into()),
            }),
            AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Connect,
                timeout: false,
                message: "connection reset by peer".into(),
            }),
        ];

        let retry = provider_retry_diagnostic(&model, &errors[0]);
        assert!(retry.contains("status=429 (rate limited)"), "{retry}");
        assert!(retry.contains("code=rate_limit_exceeded"), "{retry}");
        assert!(retry.contains("retry_after=3s"), "{retry}");
        assert!(retry.contains("request_id=req-429"), "{retry}");

        for error in &errors[1..] {
            let diagnostic = provider_retry_diagnostic(&model, error);
            assert!(diagnostic.contains("provider="), "{diagnostic}");
            assert!(diagnostic.contains("model="), "{diagnostic}");
            assert!(diagnostic.contains("phase="), "{diagnostic}");
        }

        let credential_error = AiError::Http(octet_ai::HttpError {
            status: http::StatusCode::UNAUTHORIZED,
            request_id: Some("req-auth".into()),
            retry_after: None,
            provider_code: Some("invalid_api_key".into()),
            body_snippet: Some(r#"{"error":{"message":"invalid api key: sk-secret"}}"#.into()),
            retryable: false,
        });
        let diagnostic = provider_retry_diagnostic(&model, &credential_error);
        assert!(diagnostic.contains("status=401 (authentication failed)"));
        assert!(!diagnostic.contains("sk-secret"));
    }

    #[test]
    fn provider_context_limit_variants_are_classified_as_overflow() {
        for message in [
            "model_context_window_exceeded",
            "prompt is too long",
            "request_too_large",
            "context window exceeds limit",
        ] {
            let error = AiError::Provider(octet_ai::ProviderError {
                code: None,
                kind: None,
                message: message.into(),
                request_id: None,
            });
            assert!(looks_like_context_error(&error), "{message}");
        }

        let request_too_large = AiError::Http(octet_ai::HttpError {
            status: "413".parse().unwrap(),
            request_id: None,
            retry_after: None,
            provider_code: Some("request_too_large".into()),
            body_snippet: Some("request exceeds the context window".into()),
            retryable: false,
        });
        assert!(looks_like_context_error(&request_too_large));

        let media_too_large = AiError::Http(octet_ai::HttpError {
            status: "413".parse().unwrap(),
            request_id: None,
            retry_after: None,
            provider_code: Some("image_too_large".into()),
            body_snippet: Some("uploaded image payload exceeds 20 MB".into()),
            retryable: false,
        });
        assert!(!looks_like_context_error(&media_too_large));
    }

    #[test]
    fn non_timeout_network_failure_gets_five_retries_and_friendly_failure() {
        let error = AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Connect,
            timeout: false,
            message: "connection refused".into(),
        });
        assert!(retryable_before_generation(&error));
        assert!(retryable_stream_start(&error));
        assert_eq!(provider_retry_limit(&error), 5);

        let failure = provider_failure(error, 5).to_string();
        assert!(failure.contains("Are you connected to the internet?"));
        assert!(failure.contains("connection"));
        assert!(!failure.contains("connection refused"));
    }

    #[test]
    fn public_provider_failures_include_safe_operational_details() {
        let errors = [
            (
                AgentError::Ai(AiError::Http(octet_ai::HttpError {
                    status: http::StatusCode::BAD_REQUEST,
                    request_id: Some("req-400".into()),
                    retry_after: None,
                    provider_code: Some("invalid_request".into()),
                    body_snippet: Some(
                        r#"{"error":{"message":"model does not support this request"}}"#.into(),
                    ),
                    retryable: false,
                })),
                "status=400 (bad request) code=invalid_request detail=model does not support this request request_id=req-400",
            ),
            (
                AgentError::Ai(AiError::Provider(octet_ai::ProviderError {
                    code: Some("upstream_error".into()),
                    kind: Some("server_error".into()),
                    message: "upstream temporarily unavailable".into(),
                    request_id: Some("req-stream".into()),
                })),
                "phase=response body (provider error) code=upstream_error kind=server_error detail=upstream temporarily unavailable request_id=req-stream",
            ),
            (
                AgentError::Ai(AiError::Transport(octet_ai::TransportError {
                    phase: octet_ai::TransportPhase::Body,
                    timeout: true,
                    message: "stream idle beyond its timeout".into(),
                })),
                "phase=response body timeout hint=Provider acceptance and failed-attempt usage are uncertain. Inspect provider state before retrying explicitly. detail=stream idle beyond its timeout",
            ),
            (
                AgentError::IncompleteResponse {
                    stop_reason: "refusal".to_owned(),
                },
                "phase=response completion reason=refusal",
            ),
        ];

        for (error, suffix) in errors {
            let diagnostic = public_error_diagnostic(&error, "openai", "gpt-test");
            assert!(diagnostic.ends_with(suffix), "{diagnostic}");
            assert!(diagnostic.starts_with("provider=openai model=gpt-test "));
        }

        let error = AgentError::Ai(AiError::Http(octet_ai::HttpError {
            status: http::StatusCode::UNAUTHORIZED,
            request_id: Some("req-auth".into()),
            retry_after: None,
            provider_code: Some("invalid_api_key".into()),
            body_snippet: Some(r#"{"error":{"message":"invalid api key: sk-secret"}}"#.into()),
            retryable: false,
        }));
        let diagnostic = public_error_diagnostic(&error, "openrouter", "openrouter/test");
        assert!(diagnostic.contains("status=401 (authentication failed)"));
        assert!(diagnostic.contains("code=invalid_api_key"));
        assert!(diagnostic.contains("request_id=req-auth"));
        assert!(!diagnostic.contains("sk-secret"));

        assert_eq!(
            public_error_diagnostic(&AgentError::RunEnded, "openai", "gpt-test"),
            "the run has already finished"
        );
    }

    #[test]
    fn connect_timeout_is_not_automatically_retried() {
        let error = AiError::Transport(octet_ai::TransportError {
            phase: octet_ai::TransportPhase::Connect,
            timeout: true,
            message: "connection timed out".into(),
        });
        assert!(!retryable_before_generation(&error));
        assert!(!retryable_stream_start(&error));
        assert_eq!(provider_retry_limit(&error), 0);
    }

    #[test]
    fn stream_failure_delegates_classification_to_inner_failure() {
        let progress = octet_ai::StreamProgress {
            provider_events: 412,
            decoded_events: 38,
            content_bytes: 18_204,
            buffered_bytes: 96,
            first_body_seen: true,
            elapsed_ms: 97_321,
            last_event_ms: Some(97_000),
        };

        // Body disconnects remain ambiguous, even before visible generation.
        let disconnect = AiError::StreamFailure {
            inner: Box::new(AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Body,
                timeout: false,
                message: "connection reset by peer".into(),
            })),
            progress,
        };
        assert_eq!(ai_error_phase(&disconnect), "response body");
        assert!(!retryable_before_generation(&disconnect));
        assert!(!retryable_stream_start(&disconnect));
        assert!(!is_replayable_network_failure(&disconnect));
        assert!(!looks_like_context_error(&disconnect));
        assert_eq!(provider_retry_limit(&disconnect), 0);

        // A stream that ended on a provider 503 frame keeps that frame's
        // retry budget instead of being demoted to the wrapper's behavior.
        let server_error = AiError::StreamFailure {
            inner: Box::new(AiError::Provider(octet_ai::ProviderError {
                code: Some("503".into()),
                kind: Some("server_error".into()),
                message: "temporarily unavailable".into(),
                request_id: None,
            })),
            progress,
        };
        assert_eq!(
            ai_error_phase(&server_error),
            "response body (provider error)"
        );
        assert!(retryable_stream_start(&server_error));
        assert!(!is_replayable_network_failure(&server_error));
        assert_eq!(provider_retry_limit(&server_error), MAX_PROVIDER_RETRIES);

        // A transport timeout with a context-flavoured message must still
        // never be classified as context overflow, wrapped or bare.
        let deadline = AiError::StreamFailure {
            inner: Box::new(AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Body,
                timeout: true,
                message: "context deadline exceeded".into(),
            })),
            progress,
        };
        assert!(!looks_like_context_error(&deadline));
        assert!(!retryable_stream_start(&deadline));
        assert_eq!(provider_retry_limit(&deadline), 0);

        // A post-send heartbeat deadline has ambiguous provider acceptance, so
        // it is terminal even if no generation was decoded.
        let heartbeat = AiError::StreamFailure {
            inner: Box::new(AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Body,
                timeout: true,
                message: "Responses WebSocket heartbeat acknowledgement timed out".into(),
            })),
            progress,
        };
        assert!(!retryable_before_generation(&heartbeat));
        assert!(!retryable_stream_start(&heartbeat));
        assert!(!is_replayable_network_failure(&heartbeat));
        assert_eq!(provider_retry_limit(&heartbeat), 0);

        // And a provider context-error frame inside a 2xx stream must still
        // be detected through the wrapper, so compaction still triggers.
        let overflow = AiError::StreamFailure {
            inner: Box::new(AiError::Provider(octet_ai::ProviderError {
                code: None,
                kind: None,
                message: "prompt is too long".into(),
                request_id: None,
            })),
            progress,
        };
        assert!(looks_like_context_error(&overflow));

        // Even the unit variant keeps its exact phase label through the
        // wrapper.
        let canceled = AiError::StreamFailure {
            inner: Box::new(AiError::Canceled),
            progress,
        };
        assert_eq!(ai_error_phase(&canceled), "request cancellation");
    }

    #[test]
    fn websocket_connection_limit_is_retried_before_generation() {
        let error = octet_ai::ProviderError {
            code: Some("websocket_connection_limit_reached".into()),
            kind: None,
            message: "create a new websocket connection".into(),
            request_id: None,
        };
        assert!(provider_requests_connection_refresh(&error));
        assert!(retryable_stream_start(&AiError::Provider(error)));
        assert_eq!(
            provider_retry_limit(&AiError::Provider(octet_ai::ProviderError {
                code: Some("websocket_connection_limit_reached".into()),
                kind: None,
                message: "create a new websocket connection".into(),
                request_id: None,
            })),
            MAX_PROVIDER_RETRIES
        );
    }

    #[test]
    fn stream_failure_diagnostic_appends_wire_progress_inside_the_public_bound() {
        let progress = octet_ai::StreamProgress {
            provider_events: 412,
            decoded_events: 38,
            content_bytes: 18_204,
            buffered_bytes: 96,
            first_body_seen: true,
            elapsed_ms: 97_321,
            last_event_ms: Some(97_000),
        };
        let suffix = "stream_progress=frames=412 events=38 content=18204B buffered=96B first_byte=seen elapsed=97321ms last_event=97000ms";

        let inner = AiError::Provider(octet_ai::ProviderError {
            // Four oversized fields push the bare diagnostic past the public
            // bound, so the wrapper must reserve room for its progress field
            // instead of letting truncation clip the progress off the end.
            code: Some("x".repeat(600)),
            kind: Some("y".repeat(600)),
            message: "z".repeat(600),
            request_id: Some("w".repeat(600)),
        });
        let bare = public_ai_error_diagnostic(&inner, "openai", "gpt-test");
        assert_eq!(
            bare.len(),
            MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES,
            "fixture must overflow the public bound"
        );
        assert!(bare.ends_with('…'));

        let wrapped = public_ai_error_diagnostic(
            &AiError::StreamFailure {
                inner: Box::new(inner),
                progress,
            },
            "openai",
            "gpt-test",
        );
        assert!(
            wrapped.len() <= MAX_PUBLIC_PROVIDER_DIAGNOSTIC_BYTES,
            "wrapped diagnostic must stay inside the public bound: {}",
            wrapped.len()
        );
        assert!(
            wrapped.ends_with(suffix),
            "truncation must not clip the progress field: {wrapped}"
        );
        assert!(wrapped.contains("phase=response body (provider error)"));
    }

    #[test]
    fn bare_ai_error_variants_surface_bounded_detail() {
        let errors = [
            AiError::Config(octet_ai::ConfigError::Parse(
                "malformed endpoint file".into(),
            )),
            AiError::Auth(octet_ai::AuthError::Resolve),
            AiError::Validation(octet_ai::ValidationError::OrphanToolResult(
                octet_ai::ToolCallId("call_orphan".into()),
            )),
            AiError::Unsupported(octet_ai::UnsupportedError::Image),
            AiError::Decode(octet_ai::DecodeError::Json("unterminated string".into())),
            AiError::Pricing(octet_ai::PricingError::ArithmeticOverflow),
            AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish),
        ];
        for error in &errors {
            let diagnostic = public_ai_error_diagnostic(error, "openai", "gpt-test");
            assert!(diagnostic.contains("detail="), "{diagnostic}");
            assert!(
                diagnostic.starts_with("provider=openai model=gpt-test phase="),
                "{diagnostic}"
            );
        }
        let config = public_ai_error_diagnostic(
            &AiError::Config(octet_ai::ConfigError::Parse(
                "malformed endpoint file".into(),
            )),
            "openai",
            "gpt-test",
        );
        assert_eq!(
            config,
            "provider=openai model=gpt-test phase=request preparation detail=Parse error: malformed endpoint file"
        );
        let canceled = public_ai_error_diagnostic(&AiError::Canceled, "openai", "gpt-test");
        assert_eq!(
            canceled,
            "provider=openai model=gpt-test phase=request cancellation"
        );
    }

    #[test]
    fn request_estimator_counts_inline_media_semantically_not_as_base64_text() {
        let image = Media::image_bytes(
            bytes::Bytes::from(vec![7u8; 1024 * 1024]),
            "image/png".parse().unwrap(),
        );
        let messages = vec![Message::User(UserMessage {
            content: vec![UserPart::Media(image)],
        })];

        let estimate = estimate_request_tokens("system", &messages, &[]);
        assert!(estimate >= ESTIMATED_IMAGE_TOKENS, "{estimate}");
        assert!(
            estimate < 10_000,
            "inline image bytes were miscounted as text tokens: {estimate}"
        );
    }

    #[test]
    fn responses_capacity_estimates_only_new_opaque_items_and_rebuilds_on_compaction() {
        let directory = tempfile::tempdir().unwrap();
        let mut session =
            Session::create(directory.path().join("responses-capacity.jsonl")).unwrap();
        let model = tool_media_model(Protocol::OpenAiResponses, octet_ai::ModalitySet::none());
        session.append(user_message("prefix".into())).unwrap();
        let baseline =
            context_breakdown(&session, &model, "system", &session.context().unwrap(), &[]);
        let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);
        cache.estimate(&session, &model, "system", &[], 1).unwrap();
        for turn in 0..32 {
            session.append(user_message("request".into())).unwrap();
            let output = octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(
                serde_json::json!({
                    "type": "message", "id": format!("message-{turn}"),
                    "role": "assistant", "content": [{"type": "output_text", "text": "answer"}],
                    "opaque_future_field": "large payload".repeat(1024)
                }),
            )
            .unwrap()]);
            session
                .append_assistant_turn(
                    AssistantMessage {
                        content: vec![AssistantPart::Text("answer".into())],
                        model: model.spec.id.clone(),
                        protocol: Protocol::OpenAiResponses,
                    },
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    Usage::default(),
                    None,
                    StopReason::EndTurn,
                    Some(output),
                )
                .unwrap();
            let incremental = cache.estimate(&session, &model, "system", &[], 1).unwrap();
            let full = reconcile_context_estimate(
                &session,
                &model,
                "system",
                &session.context().unwrap(),
                &[],
            );
            assert!(incremental.input_tokens >= full.input_tokens);
            assert_eq!(cache.full_rebuilds(), 1);
        }
        let kept = session.append(user_message("kept".into())).unwrap();
        session.compact("summary", kept).unwrap();
        let incremental = cache.estimate(&session, &model, "system", &[], 1).unwrap();
        let full = reconcile_context_estimate(
            &session,
            &model,
            "system",
            &session.context().unwrap(),
            &[],
        );
        assert_eq!(incremental, full);
        assert_eq!(cache.full_rebuilds(), 2);
    }

    #[test]
    fn canonical_capacity_advances_new_messages_without_rebuilding_history() {
        use octet_ai::{ModelCatalog, ModelId};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("capacity.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let system = "system";
        session
            .append(user_message(UserInput::from("first message")))
            .unwrap();
        let messages = session.context().unwrap();
        let baseline = context_breakdown(&session, &model, system, &messages, &[]);
        let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);

        session
            .append(user_message(UserInput::from("second message")))
            .unwrap();
        let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
        let full_messages = session.context().unwrap();
        let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

        assert!(
            incremental.input_tokens >= full.input_tokens,
            "incremental capacity undercounted the request: incremental={incremental:?} full={full:?}"
        );
        assert_eq!(cache.full_rebuilds(), 0);
    }

    #[test]
    fn canonical_capacity_overbounds_coalesced_tool_results_without_a_full_scan() {
        use octet_ai::{ModelCatalog, ModelId, ToolResult, ToolResultPart};

        fn tool_result(id: &str, text: &str) -> EntryValue {
            EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: octet_ai::ToolCallId(id.into()),
                    content: vec![ToolResultPart::Text(text.into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            }))
        }

        let directory = tempfile::tempdir().unwrap();
        let mut session =
            Session::create(directory.path().join("coalesced-capacity.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let system = "system";
        session.append(tool_result("one", "first result")).unwrap();
        let messages = session.context().unwrap();
        let baseline = context_breakdown(&session, &model, system, &messages, &[]);
        let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);

        session.append(tool_result("two", "second result")).unwrap();
        let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
        let full_messages = session.context().unwrap();
        let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

        assert_eq!(
            full_messages.len(),
            1,
            "tool results should coalesce in context"
        );
        assert!(
            incremental.input_tokens >= full.input_tokens,
            "coalesced tool result undercounted the request: incremental={incremental:?} full={full:?}"
        );
        assert_eq!(cache.full_rebuilds(), 0);
    }

    #[test]
    fn canonical_capacity_reanchors_to_authoritative_provider_usage() {
        use octet_ai::{AssistantMessage, AssistantPart, ModelCatalog, ModelId};

        let directory = tempfile::tempdir().unwrap();
        let mut session =
            Session::create(directory.path().join("provider-capacity.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let system = "system";
        session
            .append(user_message(UserInput::from("prompt")))
            .unwrap();
        let messages = session.context().unwrap();
        let baseline = context_breakdown(&session, &model, system, &messages, &[]);
        let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);
        let usage = Usage {
            input_tokens: 90_000,
            output_tokens: 10_000,
            total_tokens: 100_000,
            ..Usage::default()
        };

        session
            .append_assistant_turn(
                AssistantMessage {
                    content: vec![AssistantPart::Text("answer".into())],
                    model: model.spec.id.clone(),
                    protocol: model.spec.protocol,
                },
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                usage,
                None,
                StopReason::EndTurn,
                None,
            )
            .unwrap();
        cache.observe_assistant_response(&session, &model, &usage);
        let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
        let full_messages = session.context().unwrap();
        let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

        assert_eq!(incremental.provider_tokens, Some(100_000));
        assert_eq!(incremental.provider_tokens, full.provider_tokens);
        assert!(incremental.input_tokens >= full.input_tokens);
        assert_eq!(cache.full_rebuilds(), 0);
    }

    #[test]
    fn canonical_capacity_rebuilds_after_a_local_compaction_boundary() {
        use octet_ai::{ModelCatalog, ModelId};

        let directory = tempfile::tempdir().unwrap();
        let mut session =
            Session::create(directory.path().join("compaction-capacity.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let system = "system";
        session
            .append(user_message(UserInput::from("old message")))
            .unwrap();
        let first_kept = session
            .append(user_message(UserInput::from("kept message")))
            .unwrap();
        let messages = session.context().unwrap();
        let baseline = context_breakdown(&session, &model, system, &messages, &[]);
        let mut cache = ContextCapacityCache::seeded(&session, 1, &baseline);

        session.compact("summary", first_kept).unwrap();
        let incremental = cache.estimate(&session, &model, system, &[], 1).unwrap();
        let full_messages = session.context().unwrap();
        let full = reconcile_context_estimate(&session, &model, system, &full_messages, &[]);

        assert_eq!(incremental, full);
        assert_eq!(cache.full_rebuilds(), 1);
    }

    fn tool_media_model(protocol: Protocol, modalities: octet_ai::ModalitySet) -> Model {
        let base = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let mut spec = (*base.spec).clone();
        spec.protocol = protocol;
        spec.capabilities.input_modalities = modalities;
        Model {
            spec: Arc::new(spec),
            endpoint: base.endpoint,
        }
    }

    #[test]
    fn owner_images_come_only_from_accepted_durable_protocol_parts() {
        let raw = Ok(ToolOutput::new("image").with_media(Media::image_bytes(
            bytes::Bytes::from_static(b"payload-sentinel"),
            "image/png".parse().unwrap(),
        )));
        for protocol in [
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        ] {
            for supported in [false, true] {
                let modalities = if supported {
                    octet_ai::ModalitySet::none().with(octet_ai::Modality::Image)
                } else {
                    octet_ai::ModalitySet::none()
                };
                let (message, _, _, _, _) = lower_tool_result(
                    octet_ai::ToolCallId("call".into()),
                    &raw,
                    &tool_media_model(protocol, modalities),
                    4096,
                    Vec::new(),
                );
                let owner = ToolOutput::new("")
                    .with_owner_presentation_images(lowered_tool_result_media(&message));
                assert_eq!(owner.media().len(), usize::from(supported));
                assert!(!owner.presentation_images_omitted());
                assert!(!format!("{owner:?}").contains("payload-sentinel"));
            }
        }
    }

    #[test]
    fn ambiguous_stream_endings_have_no_retry_budget_and_actionable_hints() {
        for error in [
            AiError::StreamProtocol(octet_ai::StreamProtocolError::MissingFinish),
            AiError::StreamProtocol(octet_ai::StreamProtocolError::PrematureEof),
            AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Body,
                timeout: false,
                message: "connection reset".into(),
            }),
        ] {
            assert!(!retryable_before_generation(&error));
            assert!(!retryable_stream_start(&error));
            assert!(!is_replayable_network_failure(&error));
            assert_eq!(provider_retry_limit(&error), 0);
            let diagnostic = public_ai_error_diagnostic(&error, "test", "test");
            assert!(
                diagnostic.contains("Inspect provider state before retrying explicitly"),
                "{diagnostic}"
            );
        }
    }

    #[test]
    fn anthropic_tool_image_stays_inside_the_paired_result() {
        let model = tool_media_model(
            Protocol::AnthropicMessages,
            octet_ai::ModalitySet::none().with(octet_ai::Modality::Image),
        );
        let result = Ok(ToolOutput::new("read=image").with_media(Media::image_bytes(
            bytes::Bytes::from_static(b"png"),
            "image/png".parse().unwrap(),
        )));
        let (message, accepted, _, is_error, _) = lower_tool_result(
            octet_ai::ToolCallId("call".into()),
            &result,
            &model,
            4096,
            Vec::new(),
        );
        assert_eq!(accepted, vec![ToolOutputMediaKind::Image]);
        assert!(!is_error);
        assert_eq!(message.content.len(), 1);
        let UserPart::ToolResult(result) = &message.content[0] else {
            panic!("tool result must remain first");
        };
        assert!(matches!(
            result.content.get(1),
            Some(ToolResultPart::Media(Media::Image(_)))
        ));
    }

    #[test]
    fn ordered_tool_parts_keep_text_image_text_order_under_one_text_budget() {
        let text_limit = TOOL_TRUNCATION_MARKER.len() + 6;
        for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
            let model = tool_media_model(
                protocol,
                octet_ai::ModalitySet::none().with(octet_ai::Modality::Image),
            );
            let result = Ok(ToolOutput::from_content_parts([
                ToolOutputContentPart::Text("ABCDEFGHIJKLMNOPQRSTUVWXYZ".into()),
                ToolOutputContentPart::Media(Media::image_bytes(
                    bytes::Bytes::from_static(b"png"),
                    "image/png".parse().unwrap(),
                )),
                ToolOutputContentPart::Text("abcdefghijklmnopqrstuvwxyz".into()),
            ]));

            let (message, accepted, persisted_text, is_error, _) = lower_tool_result(
                octet_ai::ToolCallId("call".into()),
                &result,
                &model,
                text_limit,
                Vec::new(),
            );

            assert_eq!(accepted, vec![ToolOutputMediaKind::Image]);
            assert!(!is_error);
            assert!(persisted_text.len() <= text_limit);
            let UserPart::ToolResult(result) = &message.content[0] else {
                panic!("expected canonical tool result");
            };
            assert_eq!(result.content.len(), 3);
            assert!(matches!(
                &result.content[0],
                ToolResultPart::Text(text)
                    if text == &format!("ABC{TOOL_TRUNCATION_MARKER}")
            ));
            assert!(matches!(
                result.content[1],
                ToolResultPart::Media(Media::Image(_))
            ));
            assert!(matches!(
                &result.content[2],
                ToolResultPart::Text(text) if text == "xyz"
            ));
            let provider_text_bytes = result
                .content
                .iter()
                .filter_map(|part| match part {
                    ToolResultPart::Text(text) => Some(text.len()),
                    ToolResultPart::Media(_) => None,
                })
                .sum::<usize>();
            assert_eq!(provider_text_bytes, text_limit);
        }
    }

    #[test]
    fn lowering_keeps_structured_details_outside_provider_visible_content() {
        let model = tool_media_model(Protocol::OpenAiResponses, octet_ai::ModalitySet::none());
        let result = Ok(ToolOutput::new("Found one source.")
            .try_with_details(
                Some(serde_json::json!({"sources": [{"title": "Primary"}]})),
                Some(serde_json::json!({"cache": "miss"})),
            )
            .unwrap());
        let (message, _, _, is_error, details) = lower_tool_result(
            octet_ai::ToolCallId("call".into()),
            &result,
            &model,
            4096,
            Vec::new(),
        );

        assert!(!is_error);
        let details = details.expect("durable details");
        assert_eq!(
            details.structured_content(),
            Some(&serde_json::json!({"sources": [{"title": "Primary"}]}))
        );
        assert_eq!(
            details.metadata(),
            Some(&serde_json::json!({"cache": "miss"}))
        );
        let UserPart::ToolResult(provider_result) = &message.content[0] else {
            panic!("expected canonical tool result");
        };
        assert_eq!(provider_result.content.len(), 1);
        assert!(matches!(
            provider_result.content[0],
            ToolResultPart::Text(ref text) if text == "Found one source."
        ));
    }

    #[test]
    fn openai_chat_wav_and_mp3_follow_the_paired_tool_result() {
        let model = tool_media_model(
            Protocol::OpenAiChat,
            octet_ai::ModalitySet::none().with(octet_ai::Modality::Audio),
        );
        for format in [octet_ai::AudioFormat::Wav, octet_ai::AudioFormat::Mp3] {
            let result = Ok(ToolOutput::new("read=audio").with_media(Media::audio_bytes(
                bytes::Bytes::from_static(b"audio"),
                format,
            )));
            let (message, accepted, _, is_error, _) = lower_tool_result(
                octet_ai::ToolCallId("call".into()),
                &result,
                &model,
                4096,
                Vec::new(),
            );
            assert_eq!(accepted, vec![ToolOutputMediaKind::Audio]);
            assert!(!is_error);
            assert!(matches!(message.content[0], UserPart::ToolResult(_)));
            assert!(matches!(
                message.content[1],
                UserPart::Media(Media::Audio(_))
            ));
        }
    }

    #[test]
    fn unsupported_tool_audio_is_an_error_without_media_or_indicator() {
        let responses = tool_media_model(
            Protocol::OpenAiResponses,
            octet_ai::ModalitySet::none().with(octet_ai::Modality::Audio),
        );
        let audio = Ok(ToolOutput::new("read=audio").with_media(Media::audio_bytes(
            bytes::Bytes::from_static(b"audio"),
            octet_ai::AudioFormat::Wav,
        )));
        let (message, accepted, text, is_error, _) = lower_tool_result(
            octet_ai::ToolCallId("call".into()),
            &audio,
            &responses,
            4096,
            Vec::new(),
        );
        assert!(accepted.is_empty());
        assert!(is_error);
        assert!(text.contains("protocol cannot replay audio"));
        assert_eq!(message.content.len(), 1);

        let chat = tool_media_model(
            Protocol::OpenAiChat,
            octet_ai::ModalitySet::none().with(octet_ai::Modality::Audio),
        );
        let aac = Ok(ToolOutput::new("read=audio").with_media(Media::audio_bytes(
            bytes::Bytes::from_static(b"audio"),
            octet_ai::AudioFormat::Aac,
        )));
        let (message, accepted, text, is_error, _) = lower_tool_result(
            octet_ai::ToolCallId("call".into()),
            &aac,
            &chat,
            4096,
            Vec::new(),
        );
        assert!(accepted.is_empty());
        assert!(is_error);
        assert!(text.contains("accepts WAV or MP3"));
        assert_eq!(message.content.len(), 1);
    }

    #[test]
    fn a_requested_service_tier_is_gated_by_the_route_and_never_silently_dropped() {
        use octet_ai::{ModelCatalog, ModelId, ResponsesRuntimeProfile, ServiceTier};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("service-tier.jsonl")).unwrap();
        let mut model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();

        // A route that does not declare the field refuses the selection with the
        // codec's typed unsupported error instead of dropping it silently.
        let rejection = resolve_service_tier(&model, Some(ServiceTier::Priority)).unwrap_err();
        assert_eq!(
            rejection.to_string(),
            "ai error: Unsupported error: Responses service tier is unsupported on this route"
        );
        // The same route may always clear the selection.
        assert_eq!(resolve_service_tier(&model, None).unwrap(), None);

        // The declared Codex runtime accepts it.
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            ResponsesRuntimeProfile::Codex;
        assert_eq!(
            resolve_service_tier(&model, Some(ServiceTier::Priority)).unwrap(),
            Some(ServiceTier::Priority)
        );

        // A non-Responses protocol could not emit the field at all, so a declared
        // profile bit must not be enough.
        let mut chat = model.clone();
        Arc::make_mut(&mut chat.spec).protocol = Protocol::OpenAiChat;
        assert!(resolve_service_tier(&chat, Some(ServiceTier::Priority)).is_err());

        // The historical no-tier path is untouched: a session whose assistant
        // turn has no route-affine sidecar still builds no Responses options.
        session
            .append(user_message(UserInput::from("legacy prompt")))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("legacy answer".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
            })))
            .unwrap();
        assert!(
            durable_responses_options(&session, &model, "system", None)
                .unwrap()
                .is_none(),
            "no tier and no replay window keeps the canonical no-options request"
        );

        // A requested tier still rides on the request when there is no replay
        // window: the codec then replays canonically exactly as it would with no
        // options, so `/fast` cannot be silently inert.
        let options =
            durable_responses_options(&session, &model, "system", Some(ServiceTier::Flex))
                .unwrap()
                .expect("a requested tier always produces options");
        // A baseline pin is request metadata, not an ordered input update.
        // Legacy sessions without opaque sidecars must keep canonical replay.
        Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_features
            .reasoning_effort_updates = true;
        Arc::make_mut(&mut model.spec)
            .capabilities
            .responses_features
            .reasoning_effort_updates = true;
        let baseline = ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium);
        session
            .append(EntryValue::ResponsesReasoning {
                endpoint: model.endpoint.id.clone(),
                model: model.spec.id.clone(),
                baseline: baseline.clone(),
                update: None,
            })
            .unwrap();
        assert!(
            durable_responses_options(&session, &model, "system", None)
                .unwrap()
                .is_none(),
            "baseline-only reasoning history remains canonically replayable"
        );
        session
            .append(EntryValue::ResponsesReasoning {
                endpoint: model.endpoint.id.clone(),
                model: model.spec.id.clone(),
                baseline: baseline.clone(),
                update: Some(octet_ai::ResponsesConfigurationUpdate {
                    reasoning: ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
                }),
            })
            .unwrap();
        assert!(durable_responses_options(&session, &model, "system", None)
            .unwrap()
            .is_none());
        let effective = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
        assert_eq!(
            request_reasoning_for_replay(&session, &model, None, &baseline).unwrap(),
            effective
        );

        assert_eq!(options.service_tier, Some(ServiceTier::Flex));
        assert_eq!(
            request_reasoning_for_replay(&session, &model, Some(&options), &baseline).unwrap(),
            effective
        );
        assert!(options.input.is_none());
        assert_eq!(options.previous_response_id, None);
        assert!(!options.store);
        assert_eq!(options.context_management, None);
    }

    #[cfg(any(unix, windows))]
    #[derive(Default)]
    struct RecordingCheckpointSink {
        snapshots: Mutex<Vec<String>>,
        refuse: bool,
    }

    #[cfg(any(unix, windows))]
    impl RecordingCheckpointSink {
        fn snapshots(&self) -> Vec<String> {
            self.snapshots.lock().unwrap().clone()
        }
    }

    #[cfg(any(unix, windows))]
    impl PartialOutputCheckpointSink for RecordingCheckpointSink {
        fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
            if self.refuse {
                return Err(ToolError::new("storage fault"));
            }
            self.snapshots.lock().unwrap().push(snapshot.to_owned());
            Ok(())
        }
    }

    #[cfg(any(unix, windows))]
    fn checkpoint_config(
        sink: Arc<RecordingCheckpointSink>,
    ) -> (
        PartialOutputCheckpointConfig,
        Arc<PartialOutputCheckpointTotals>,
    ) {
        let totals = Arc::new(PartialOutputCheckpointTotals::default());
        (
            PartialOutputCheckpointConfig {
                tool: "bash".to_owned(),
                sink: Some(sink),
                interval: BASH_CHECKPOINT_INTERVAL,
                totals: Arc::clone(&totals),
            },
            totals,
        )
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn partial_output_checkpoints_pace_bound_and_never_claim_completion() {
        let sink = Arc::new(RecordingCheckpointSink::default());
        let (config, totals) = checkpoint_config(Arc::clone(&sink));
        let start = std::time::Instant::now();
        let mut live = LivePartialOutput::for_call(&config, "bash").expect("the opt-in names bash");

        // The opt-in is per tool: another tool's calls are not published.
        assert!(LivePartialOutput::for_call(&config, "search").is_none());

        // First observation publishes immediately.
        let first = live
            .observe_output(OutputStream::Stdout, b"alpha\n", start)
            .expect("first observation publishes");
        assert!(first.contains("stdout: 6 bytes seen"), "{first}");
        assert!(first.contains("alpha"), "{first}");

        // Before the interval: paced away, counted, never published.
        assert!(live
            .observe_output(
                OutputStream::Stdout,
                b"beta\n",
                start + Duration::from_millis(1)
            )
            .is_none());
        assert_eq!(sink.snapshots().len(), 1, "one publication so far");
        assert_eq!(totals.stats().paced, 1);

        // Past the interval a changed snapshot publishes again, and an unchanged
        // one is duplicate-suppressed instead of re-published.
        let second = live
            .observe_output(
                OutputStream::Stdout,
                b"gamma\n",
                start + BASH_CHECKPOINT_INTERVAL,
            )
            .expect("interval elapsed with new output");
        assert!(second.contains("alpha\nbeta\ngamma"), "{second}");
        assert!(live
            .observe_output(
                OutputStream::Stderr,
                b"",
                start + BASH_CHECKPOINT_INTERVAL * 2
            )
            .is_none());
        assert_eq!(sink.snapshots().len(), 2);

        // Non-output progress is not checkpointed at all.
        live.observe_progress(
            &ToolProgress::Status("still running".into()),
            start + BASH_CHECKPOINT_INTERVAL * 3,
        );
        assert_eq!(sink.snapshots().len(), 2);

        // A large burst keeps the newest bytes under the row's 50 KiB bound, keeps
        // its header, and stays a checkpoint: no publication may claim the
        // command finished.
        let burst_bytes = 4 * BASH_CHECKPOINT_MAX_BYTES;
        let mut burst = vec![b'x'; burst_bytes];
        burst.extend_from_slice(b"NEWEST-MARKER");
        let bounded = live
            .observe_output(
                OutputStream::Stdout,
                &burst,
                start + BASH_CHECKPOINT_INTERVAL * 4,
            )
            .expect("a changed snapshot publishes");
        assert!(
            bounded.len() <= BASH_CHECKPOINT_MAX_BYTES,
            "snapshot is bounded: {} bytes",
            bounded.len()
        );
        assert!(
            bounded.contains(&format!(
                "stdout: {} bytes seen (earlier bytes elided)",
                6 + 5 + 6 + burst_bytes as u64 + 13
            )),
            "the header survives bounding: {}",
            &bounded[..bounded.len().min(200)]
        );
        // The newest bytes are what a recovery consumer needs, and the oldest are
        // what got elided: the stdout section keeps the burst's tail. The render
        // carries stdout first and the stderr section last, so the marker sits at
        // the end of its own section rather than at the end of the snapshot.
        let stdout_section = bounded
            .split_once("\nstderr: ")
            .map(|(stdout, _)| stdout)
            .expect("the render always carries both stream sections");
        assert!(
            stdout_section.ends_with("NEWEST-MARKER"),
            "the newest bytes are the ones kept: {}",
            &stdout_section[stdout_section.len().saturating_sub(120)..]
        );
        assert!(
            stdout_section.len() <= PARTIAL_STREAM_CAP + PARTIAL_HEADER_RESERVE,
            "a retained stream section stays inside its half of the cap: {} bytes",
            stdout_section.len()
        );
        let stats = totals.stats();
        assert_eq!(stats.published, 3);
        assert!(stats.failures == 0);
        for snapshot in sink.snapshots() {
            assert!(
                !snapshot.contains("complete_stdout=true")
                    && !snapshot.contains("complete_stderr=true"),
                "a checkpoint never claims completion: {snapshot}"
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_refused_checkpoint_is_counted_and_never_becomes_a_tool_result() {
        let sink = Arc::new(RecordingCheckpointSink {
            snapshots: Mutex::new(Vec::new()),
            refuse: true,
        });
        let (config, totals) = checkpoint_config(Arc::clone(&sink));
        let start = std::time::Instant::now();
        let mut live = LivePartialOutput::for_call(&config, "bash").unwrap();

        assert!(
            live.observe_output(OutputStream::Stdout, b"alpha\n", start)
                .is_none(),
            "a storage fault is not a publication"
        );
        assert_eq!(totals.stats().failures, 1);
        assert_eq!(totals.stats().published, 0);
        assert!(sink.snapshots().is_empty());
    }

    /// Row 4.8's run-path consumer: the live panel's *replaceable* state is
    /// paced by [`AdaptivePreviewCoalescer`] while append-only flavors are
    /// forwarded verbatim.
    #[test]
    fn live_preview_pacer_publishes_immediately_collapses_and_settles_the_latest() {
        let start = std::time::Instant::now();
        let decoration = |step: usize| {
            ToolProgressDecoration::new(format!("step {step}"), Some(format!("detail {step}")))
                .expect("bounded decoration")
        };
        let mut pacer = LivePreviewPacer::new();

        // The first replaceable state after idle is published immediately.
        let first = pacer
            .observe(decoration(0), start)
            .expect("the first state is immediate");
        assert_eq!(first.label(), "step 0");

        // Every intermediate state before the deadline collapses into one held
        // slot: no queue grows and no intermediate state is published.
        for step in 1..12 {
            assert!(
                pacer.observe(decoration(step), start).is_none(),
                "step {step} must be paced away"
            );
        }
        assert_eq!(
            pacer.stats(),
            (1, 10),
            "ten intermediates collapsed into the single held state"
        );

        // Append-only flavors bypass the coalescer completely.
        let forwarded = forward_tool_progress(
            ToolProgress::Status("still running".into()),
            &mut pacer,
            start,
        );
        assert!(
            matches!(forwarded, Some(ToolProgress::Status(message)) if message == "still running"),
            "an append-only status is forwarded verbatim"
        );
        let chunk = forward_tool_progress(
            ToolProgress::Output {
                stream: OutputStream::Stdout,
                bytes: bytes::Bytes::from_static(b"verbatim\n"),
            },
            &mut pacer,
            start,
        );
        assert!(
            matches!(chunk, Some(ToolProgress::Output { bytes, .. }) if bytes.as_ref() == b"verbatim\n"),
            "a stdout chunk is never collapsed"
        );
        assert_eq!(
            pacer.stats().0,
            1,
            "verbatim forwarding is not a publication"
        );

        // Nothing is published before the deadline, and the deadline publishes
        // the *latest* state rather than one that was paced away.
        assert!(
            pacer.take_due(start).is_none(),
            "the held state is not due yet"
        );
        let deadline = pacer
            .flush_deadline(start)
            .expect("the held state has one trailing timer");
        assert_eq!(deadline, start + DEFAULT_PREVIEW_MIN_EMIT_INTERVAL);
        let due = pacer
            .take_due(deadline)
            .expect("the deadline publishes the held state");
        assert_eq!(due.label(), "step 11", "the latest state survives collapse");
        assert_eq!(
            pacer.flush_deadline(deadline),
            None,
            "the trailing timer is cancelled by its own publication"
        );
        assert_eq!(pacer.stats(), (2, 10));

        // A terminal boundary forces whatever is still held, exactly once: a
        // finished call can never leave the panel on stale state.
        assert!(pacer.observe(decoration(20), start).is_none());
        let settled = pacer
            .settle(start)
            .expect("the terminal boundary publishes the held state");
        assert_eq!(settled.label(), "step 20");
        assert!(
            pacer.settle(start).is_none(),
            "a settled call publishes nothing twice"
        );
        assert_eq!(pacer.stats(), (3, 10));
    }

    #[test]
    fn exact_responses_replay_estimate_counts_opaque_provider_payloads() {
        use octet_ai::{ModelCatalog, ModelId, ResponsesItem, ResponsesOutput};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        session
            .append(user_message(UserInput::from("small prompt")))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("small answer".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
            })))
            .unwrap();
        session
            .append_responses_turn(
                assistant,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_large",
                    "encrypted_content": "x".repeat(40_000),
                    "unknown": {"phase": "analysis"}
                }))
                .unwrap()]),
            )
            .unwrap();

        let messages = session.context().unwrap();
        let canonical = estimate_request_tokens("system", &messages, &[]);
        let estimate = reconcile_context_estimate(&session, &model, "system", &messages, &[]);
        assert!(
            estimate.structural_tokens > canonical.saturating_add(8_000),
            "opaque replay must drive the structural estimate: canonical={canonical}, replay={}",
            estimate.structural_tokens
        );
        assert_eq!(estimate.provider_tokens, None);

        let options = durable_responses_options(&session, &model, "system", None)
            .unwrap()
            .unwrap();
        assert!(options.input.is_some());
        assert_eq!(options.previous_response_id, None);
        assert!(!options.store);
    }

    #[test]
    fn native_checkpoint_estimate_excludes_compacted_canonical_media() {
        use octet_ai::{ModelCatalog, ModelId, ResponsesItem, ResponsesOutput};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Media(Media::image_bytes(
                    bytes::Bytes::from(vec![7u8; 1024 * 1024]),
                    "image/png".parse().unwrap(),
                ))],
            })))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("seen".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
            })))
            .unwrap();
        session
            .append_responses_turn(
                assistant,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                    "type": "message",
                    "id": "old-output"
                }))
                .unwrap()]),
            )
            .unwrap();
        session
            .append_responses_compaction(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                    "type": "compaction",
                    "id": "small-checkpoint",
                    "encrypted_content": "opaque"
                }))
                .unwrap()]),
            )
            .unwrap();

        let messages = session.context().unwrap();
        let estimate = reconcile_context_estimate(
            &session,
            &model,
            "system must already be compacted",
            &messages,
            &[],
        );
        assert!(
            estimate.structural_tokens < 1_000,
            "compacted-away media leaked into the replay estimate: {estimate:?}"
        );
        let exact =
            exact_responses_replay(&session, &model, "system must already be compacted").unwrap();
        let wire = serde_json::to_string(&exact.input).unwrap();
        assert!(wire.contains("small-checkpoint"));
        assert!(!wire.contains("system must already be compacted"));
    }

    #[test]
    fn exact_responses_estimate_counts_current_media_semantically() {
        use octet_ai::{ModelCatalog, ModelId};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Media(Media::image_bytes(
                    bytes::Bytes::from(vec![7u8; 1024 * 1024]),
                    "image/png".parse().unwrap(),
                ))],
            })))
            .unwrap();

        let messages = session.context().unwrap();
        let estimate = reconcile_context_estimate(&session, &model, "system", &messages, &[]);
        assert!(
            (ESTIMATED_IMAGE_TOKENS..10_000).contains(&estimate.structural_tokens),
            "inline base64 must be replaced by a semantic image estimate: {estimate:?}"
        );
    }

    #[test]
    fn post_checkpoint_instructions_are_included_in_the_exact_estimate() {
        use octet_ai::{ModelCatalog, ModelId, ResponsesItem, ResponsesOutput};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("old".into())],
            })))
            .unwrap();
        session
            .append_responses_compaction(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                    "type": "compaction",
                    "encrypted_content": "small"
                }))
                .unwrap()]),
            )
            .unwrap();

        let messages = session.context().unwrap();
        let short = reconcile_context_estimate(&session, &model, "short", &messages, &[]);
        let long_system = "x".repeat(128 * 1024);
        let long = reconcile_context_estimate(&session, &model, &long_system, &messages, &[]);
        assert!(
            long.structural_tokens > short.structural_tokens.saturating_add(30_000),
            "top-level instructions must participate in capacity checks: short={short:?}, long={long:?}"
        );
    }

    #[test]
    fn switched_responses_route_uses_canonical_options_but_rejects_native_replay() {
        use octet_ai::{
            AssistantMessage, AssistantPart, Message, ModelCatalog, ModelId, Protocol,
            ResponsesItem, ResponsesOutput,
        };

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("switch.jsonl")).unwrap();
        let astra = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let mut luna = astra.clone();
        Arc::make_mut(&mut luna.spec).id = ModelId("gpt-6-luna".into());
        session
            .append(user_message(UserInput::from("first prompt")))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("astra answer".into())],
                model: astra.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
            })))
            .unwrap();
        session
            .append_responses_turn(
                assistant,
                astra.endpoint.id.clone(),
                astra.spec.id.clone(),
                ResponsesOutput::new(vec![ResponsesItem::new(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "astra answer"}]
                }))
                .unwrap()]),
            )
            .unwrap();
        session
            .append(user_message(UserInput::from("second prompt")))
            .unwrap();

        assert!(durable_responses_options(&session, &luna, "system", None)
            .unwrap()
            .is_none());
        assert!(matches!(
            native_responses_options(&session, &luna, "system", None),
            Err(AgentError::InvalidCompactionPolicy(_))
        ));
        assert_eq!(session.context().unwrap().len(), 3);
    }

    #[test]
    fn marked_failed_turn_boundary_keeps_exact_replay_available_after_restart() {
        use octet_ai::{ModelCatalog, ModelId};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("fails".into())],
            })))
            .unwrap();
        close_failed_turn(&mut session, &model).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("try again".into())],
            })))
            .unwrap();
        drop(session);

        let session = Session::open(path).unwrap();
        let replay = session
            .responses_replay_snapshot(&model.endpoint.id, &model.spec.id)
            .unwrap()
            .expect("explicit local provenance must not look like a missing sidecar");
        assert!(matches!(
            replay.get(1),
            Some(ResponsesReplayItem::LocalAssistant(message))
                if matches!(
                    message.content.as_slice(),
                    [AssistantPart::Text(text)] if text == FAILED_TURN_CONTEXT_MARKER
                )
        ));
    }

    #[test]
    fn configured_cost_limit_fails_closed_without_trusted_model_pricing() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("unpriced.jsonl")).unwrap();
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        std::sync::Arc::make_mut(&mut model.spec).pricing = None;

        assert!(matches!(
            reserve_request_cost(&session, &model, 1, 1, Some(10), CacheRetention::Short),
            Err(AgentError::CostUnavailable { limit: 10 })
        ));
    }

    #[tokio::test]
    async fn native_compaction_honors_the_session_cost_limit_before_network() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        session
            .append(user_message(UserInput::from("compact this")))
            .unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let mut agent = Agent::new(AgentConfig {
            client: AiClient::new(),
            model,
            session,
            system: "system".into(),
            sandbox: SandboxConfig::new(directory.path()),
            effect_broker: EffectBroker::default(),
            extensions: ExtensionHost::new(),
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        agent.set_max_session_cost_microdollars(Some(0));

        let error = agent.compact_responses_native().await.unwrap_err();
        // The native Responses compaction endpoint has no output-cap field, so a
        // hard cost ceiling cannot be enforced and admission refuses before any
        // provider request. It must not be reported as a reserve-and-compare
        // cost limit that was never actually enforceable.
        assert!(
            matches!(error, AgentError::OutputLimitUnavailable),
            "{error:?}"
        );
        assert!(
            !matches!(
                agent
                    .session()
                    .head_ref()
                    .and_then(|head| agent.session().entry(head)),
                Some(crate::session::Entry {
                    value: EntryValue::ResponsesCompaction { .. },
                    ..
                })
            ),
            "a rejected native request must not persist a checkpoint"
        );
    }

    fn provider_context_estimate_reference(session: &Session, model: &Model) -> Option<u64> {
        let branch = active_branch_entries(session);
        let boundary = branch
            .iter()
            .rposition(|entry| {
                matches!(
                    entry.value,
                    EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. }
                )
            })
            .map_or(0, |index| index.saturating_add(1));

        for (index, entry) in branch.iter().enumerate().skip(boundary).rev() {
            if !matches!(entry.value, EntryValue::Message(Message::Assistant(_))) {
                continue;
            }
            let Some(record) = session.usage_records().iter().rev().find(|record| {
                matches!(
                    &record.kind,
                    crate::session::UsageRecordKind::AssistantTurn { assistant }
                        if assistant == &entry.id
                ) && record.endpoint.as_ref() == Some(&model.endpoint.id)
                    && record.model.as_ref() == Some(&model.spec.id)
                    && usage_context_tokens(&record.usage) > 0
            }) else {
                continue;
            };
            let trailing = branch[index.saturating_add(1)..]
                .iter()
                .filter_map(|entry| match &entry.value {
                    EntryValue::Message(message) => Some(message),
                    _ => None,
                })
                .fold(0u64, |total, message| {
                    total.saturating_add(estimate_messages_tokens(std::slice::from_ref(message)))
                });
            return Some(usage_context_tokens(&record.usage).saturating_add(trailing));
        }
        None
    }

    fn append_usage_fixture(session: &mut Session, model: &Model, tokens: u64) -> EntryId {
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("measured response".into())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            })))
            .unwrap();
        session
            .record_assistant_usage(
                assistant.clone(),
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    total_tokens: tokens,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        assistant
    }

    #[test]
    fn provider_usage_suffix_matches_reference_across_branches_and_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("suffix.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let check = |session: &Session| {
            assert_eq!(
                provider_context_estimate(session, &model),
                provider_context_estimate_reference(session, &model)
            );
        };
        check(&session);
        let measured = append_usage_fixture(&mut session, &model, 1234);
        check(&session);
        session
            .append(user_message(UserInput::from("trailing λ message")))
            .unwrap();
        session
            .append(EntryValue::Config {
                model: None,
                reasoning: Some("low".into()),
                reasoning_mode: None,
            })
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: octet_ai::ToolCallId("fixture-call".into()),
                    content: vec![ToolResultPart::Text("tool result λ".into())],
                    is_error: false,
                    added_tool_names: None,
                })],
            })))
            .unwrap();
        check(&session);
        append_usage_fixture(&mut session, &model, 0);
        check(&session);
        let mut other_model = model.clone();
        Arc::make_mut(&mut other_model.spec).id = octet_ai::ModelId("other-model".into());
        append_usage_fixture(&mut session, &other_model, 9000);
        check(&session);
        let mut other_endpoint = model.clone();
        Arc::make_mut(&mut other_endpoint.endpoint).id =
            octet_ai::EndpointId("other-endpoint".into());
        append_usage_fixture(&mut session, &other_endpoint, 9000);
        check(&session);
        let abandoned = session.head().unwrap();
        session.checkout(measured.clone()).unwrap();
        session
            .append(user_message(UserInput::from("new branch")))
            .unwrap();
        check(&session);
        append_usage_fixture(&mut session, &model, u64::MAX);
        session
            .append(user_message(UserInput::from("saturating suffix")))
            .unwrap();
        check(&session);
        assert_eq!(provider_context_estimate(&session, &model), Some(u64::MAX));
        session.checkout(abandoned).unwrap();
        session.compact("summary", measured).unwrap();
        check(&session);
        assert_eq!(provider_context_estimate(&session, &model), None);
        append_usage_fixture(&mut session, &model, 99);
        check(&session);
        session
            .append_responses_compaction(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                octet_ai::ResponsesOutput::new(
                    vec![octet_ai::ResponsesItem::new(serde_json::json!({
                "type": "compaction", "id": "native-checkpoint", "encrypted_content": "opaque"
            })).unwrap()],
                ),
            )
            .unwrap();
        check(&session);
        assert_eq!(provider_context_estimate(&session, &model), None);
        append_usage_fixture(&mut session, &model, 101);
        check(&session);
        drop(session);
        let reopened = Session::open_read_only(directory.path().join("suffix.jsonl")).unwrap();
        check(&reopened);
    }

    #[test]
    fn provider_usage_entry_work_is_bounded_by_the_unmeasured_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("work.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        for history in [10, 100, 1000] {
            while session.entries().len() < history {
                session
                    .append(user_message(UserInput::from("settled history")))
                    .unwrap();
            }
            if session.usage_records().is_empty() {
                PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(0));
                assert_eq!(provider_context_estimate(&session, &model), None);
                assert_eq!(PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.get()), 0);
            }
            append_usage_fixture(&mut session, &model, 1000);
            session
                .append(user_message(UserInput::from("fixed tail")))
                .unwrap();
            PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(0));
            let estimate = provider_context_estimate(&session, &model);
            assert_eq!(PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.get()), 3);
            assert_eq!(
                estimate,
                provider_context_estimate_reference(&session, &model)
            );
        }
    }

    #[test]
    fn provider_usage_abandoned_records_remain_an_explicit_scan_cost() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("abandoned.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let anchor = append_usage_fixture(&mut session, &model, 1234);
        let mut other = model.clone();
        Arc::make_mut(&mut other.spec).id = octet_ai::ModelId("other-model".into());
        for records in [5, 20] {
            for _ in 0..records {
                append_usage_fixture(&mut session, &other, 99);
            }
            session.checkout(anchor.clone()).unwrap();
            PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.set(0));
            PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
            assert_eq!(provider_context_estimate(&session, &model), Some(1234));
            assert_eq!(PROVIDER_CONTEXT_ENTRY_VISITS.with(|visits| visits.get()), 1);
            assert_eq!(
                PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()),
                session.usage_records().len()
            );
            assert_eq!(
                provider_context_estimate(&session, &model),
                provider_context_estimate_reference(&session, &model)
            );
        }
    }

    #[test]
    fn provider_usage_unmatched_assistants_scan_the_ledger_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("unmatched.jsonl")).unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let anchor = append_usage_fixture(&mut session, &model, 1234);
        // Preserve newest usable record semantics even with repeated accounting
        // for one assistant and a newer zero-token record.
        for tokens in [2345, 0] {
            session
                .record_assistant_usage(
                    anchor.clone(),
                    model.endpoint.id.clone(),
                    model.spec.id.clone(),
                    Usage {
                        total_tokens: tokens,
                        ..Usage::default()
                    },
                    None,
                )
                .unwrap();
        }
        let mut other = model.clone();
        Arc::make_mut(&mut other.spec).id = octet_ai::ModelId("other-model".into());
        for history in [8, 32, 128] {
            while session.entries().len() < history {
                append_usage_fixture(&mut session, &other, 99);
            }
            PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
            assert_eq!(
                provider_context_estimate(&session, &model),
                provider_context_estimate_reference(&session, &model)
            );
            assert_eq!(
                PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()),
                session.usage_records().len()
            );
            // No matching model at all must also be a single ledger pass.
            let mut absent = model.clone();
            Arc::make_mut(&mut absent.spec).id = octet_ai::ModelId("absent".into());
            PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
            assert_eq!(provider_context_estimate(&session, &absent), None);
            assert_eq!(
                PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()),
                session.usage_records().len()
            );
        }
        session.checkout(anchor).unwrap();
        PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
        assert_eq!(provider_context_estimate(&session, &model), Some(2345));
        append_usage_fixture(&mut session, &model, 3456);
        PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.set(0));
        assert_eq!(provider_context_estimate(&session, &model), Some(3456));
        assert_eq!(PROVIDER_CONTEXT_USAGE_VISITS.with(|visits| visits.get()), 1);
    }

    /// Offline matched microbenchmark, not provider or end-to-end launch latency.
    /// Run with: cargo test --release --offline --locked -p octet-agent --lib
    /// provider_usage_suffix_benchmark -- --ignored --nocapture --test-threads=1
    #[test]
    #[ignore = "manual matched timing experiment"]
    fn provider_usage_suffix_benchmark() {
        let directory = tempfile::tempdir().unwrap();
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let repetitions = 200;
        for history in [100, 1000, 10_000] {
            let mut session =
                Session::create(directory.path().join(format!("benchmark-{history}.jsonl")))
                    .unwrap();
            while session.entries().len() < history {
                session
                    .append(user_message(UserInput::from("settled history")))
                    .unwrap();
            }
            for scenario in ["unmeasured", "suffix"] {
                if scenario == "suffix" {
                    append_usage_fixture(&mut session, &model, 1000);
                    session
                        .append(user_message(UserInput::from("fixed tail")))
                        .unwrap();
                }
                assert_eq!(
                    provider_context_estimate(&session, &model),
                    provider_context_estimate_reference(&session, &model)
                );
                for trial in 0..9 {
                    // Alternate order to avoid systematically favoring warm caches.
                    for candidate in if trial % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        let estimate = if candidate {
                            provider_context_estimate
                        } else {
                            provider_context_estimate_reference
                        };
                        let start = std::time::Instant::now();
                        for _ in 0..repetitions {
                            std::hint::black_box(estimate(
                                std::hint::black_box(&session),
                                std::hint::black_box(&model),
                            ));
                        }
                        println!("provider_usage_suffix scenario={scenario} history={history} trial={trial} candidate={candidate} repetitions={repetitions} elapsed_ns={}", start.elapsed().as_nanos());
                    }
                }
            }
        }
    }

    #[test]
    fn provider_usage_baseline_skips_newer_unusable_records_and_counts_trailing_messages() {
        use octet_ai::{AssistantMessage, AssistantPart, ModelCatalog, ModelId, Protocol};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        session
            .append(user_message(UserInput::from("old prompt")))
            .unwrap();
        let measured = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("old response".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        session
            .record_assistant_usage(
                measured,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    input_tokens: 79_000,
                    output_tokens: 1_000,
                    total_tokens: 80_000,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        session
            .append(user_message(UserInput::from("x".repeat(4_000))))
            .unwrap();
        let unmeasured = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("new response".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        session
            .record_assistant_usage(
                unmeasured,
                model.endpoint.id.clone(),
                ModelId("different-model".into()),
                Usage::default(),
                None,
            )
            .unwrap();

        let estimate = provider_context_estimate(&session, &model).unwrap();
        assert!(estimate > 81_000, "{estimate}");
    }

    #[test]
    fn provider_usage_before_latest_compaction_is_not_reused() {
        use octet_ai::{AssistantMessage, AssistantPart, ModelCatalog, ModelId, Protocol};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        session
            .append(user_message(UserInput::from("old prompt")))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("old response".into())],
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        session
            .record_assistant_usage(
                assistant.clone(),
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    total_tokens: 100_000,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        session.compact("short summary", assistant).unwrap();

        assert_eq!(provider_context_estimate(&session, &model), None);
    }

    #[test]
    fn usage_accumulates_across_turns() {
        let mut total = Usage::default();
        let turn = Usage {
            input_tokens: 10,
            output_tokens: 5,
            reasoning_tokens: 2,
            total_tokens: 15,
            ..Usage::default()
        };
        add_usage(&mut total, &turn);
        add_usage(&mut total, &turn);
        assert_eq!(total.input_tokens, 20);
        assert_eq!(total.output_tokens, 10);
        assert_eq!(total.reasoning_tokens, 4);
        assert_eq!(total.total_tokens, 30);
    }

    #[test]
    fn run_cost_carries_submicrodollar_remainders_across_turns() {
        let mut total = CostAccumulator::default();
        let fractional = Cost {
            total_picodollars_remainder: 600_000,
            ..Cost::default()
        };
        total.add(Some(fractional));
        total.add(Some(fractional));
        assert_eq!(total.microdollars, 1);
        assert_eq!(total.picodollars_remainder, 200_000);
    }

    #[test]
    fn compaction_boundaries_include_each_completed_tool_episode() {
        use octet_ai::{
            AssistantMessage, AssistantPart, ModelId, Protocol, ToolResult, ToolResultPart,
        };

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        session
            .append(user_message(UserInput::from("one task")))
            .unwrap();
        for (index, text) in [("a", "first"), ("b", "second")] {
            session
                .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                    content: vec![AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: octet_ai::ToolCallId(index.into()),
                        name: "read".into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    })],
                    model: ModelId("test".into()),
                    protocol: Protocol::AnthropicMessages,
                })))
                .unwrap();
            session
                .append(EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::ToolResult(ToolResult {
                        tool_call_id: octet_ai::ToolCallId(index.into()),
                        content: vec![ToolResultPart::Text(text.into())],
                        is_error: false,
                        added_tool_names: None,
                    })],
                })))
                .unwrap();
        }
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("done".into())],
                model: ModelId("test".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();

        assert_eq!(turn_starts(&session).len(), 3);
    }

    #[test]
    fn assistant_after_compaction_marker_remains_a_turn_boundary() {
        use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol};

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        session
            .append(user_message(UserInput::from("one task")))
            .unwrap();
        let first_assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("first".into())],
                model: ModelId("test".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();
        session
            .append(user_message(UserInput::from("continue")))
            .unwrap();
        session.compact("summary", first_assistant).unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("after marker".into())],
                model: ModelId("test".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();

        assert_eq!(turn_starts(&session).len(), 2);
    }

    #[test]
    fn hard_token_reservation_rejects_before_a_request_can_cross_the_ceiling() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("token-limit.jsonl")).unwrap();
        let error = reserve_request_tokens(&session, 700, 400, Some(1_000)).unwrap_err();
        assert!(matches!(
            error,
            AgentError::TokenLimit {
                current: 0,
                reserved: 1_100,
                limit: 1_000
            }
        ));
    }

    #[test]
    fn delegated_usage_is_accounting_not_parent_context_token_consumption() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("delegated-ledger.jsonl")).unwrap();
        session
            .record_delegated_agent_usage(DelegatedUsage {
                agent_id: "agent-1".into(),
                turn_count: 2,
                tool_call_count: 1,
                endpoint: octet_ai::EndpointId("test-endpoint".into()),
                model: octet_ai::ModelId("test-model".into()),
                usage: Usage {
                    input_tokens: 40_000,
                    output_tokens: 10_000,
                    total_tokens: 50_000,
                    ..Usage::default()
                },
                cost: None,
            })
            .unwrap();

        assert_eq!(session_total_tokens_for_own_context(&session), 0);
        assert!(reserve_request_tokens(&session, 700, 200, Some(1_000)).is_ok());
        assert_eq!(session.usage_records()[0].usage.total_tokens, 50_000);
    }

    #[test]
    fn delegated_snapshots_use_only_committed_root_usage_and_borrow_cost_remainders() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mirror-deltas.jsonl");
        let mut session = Session::create(&path).unwrap();
        let mut snapshot = DelegatedUsage {
            agent_id: "child-1".into(),
            turn_count: 1,
            tool_call_count: 0,
            endpoint: octet_ai::EndpointId("endpoint".into()),
            model: octet_ai::ModelId("model".into()),
            usage: Usage {
                input_tokens: 10,
                total_tokens: 10,
                ..Usage::default()
            },
            cost: Some(Cost {
                total_picodollars_remainder: 900_000,
                ..Cost::default()
            }),
        };
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        // A failed append must leave the previous committed baseline intact.
        let mut read_only = Session::open_read_only(&path).unwrap();
        snapshot.usage.input_tokens = 20;
        snapshot.usage.total_tokens = 20;
        snapshot.cost = Some(Cost {
            total: 1,
            total_picodollars_remainder: 200_000,
            ..Cost::default()
        });
        assert!(record_delegated_usage_once(&mut read_only, snapshot.clone()).is_err());
        assert_eq!(read_only.usage_records().len(), 1);
        drop(read_only);
        drop(session);

        let mut session = Session::open(&path).unwrap();
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        assert_eq!(session.usage_records().len(), 2);
        assert_eq!(session.usage_records()[1].usage.total_tokens, 10);
        assert_eq!(session.usage_records()[1].cost.unwrap().total, 0);
        assert_eq!(
            session.usage_records()[1]
                .cost
                .unwrap()
                .total_picodollars_remainder,
            300_000
        );
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        assert_eq!(session.usage_records().len(), 2);
        assert_eq!(session.total_cost_microdollars(), 1);
        assert_eq!(session.total_cost_picodollars_remainder(), 200_000);

        // Auxiliary/cost-only updates can arrive with the same turn/tool counts.
        snapshot.cost.as_mut().unwrap().total_picodollars_remainder = 400_000;
        record_delegated_usage_once(&mut session, snapshot.clone()).unwrap();
        assert_eq!(session.usage_records().len(), 3);
        assert_eq!(session.total_cost_picodollars_remainder(), 400_000);
        drop(session);
        let mut session = Session::open(&path).unwrap();
        record_delegated_usage_once(&mut session, snapshot).unwrap();
        assert_eq!(session.usage_records().len(), 3);
        assert_eq!(
            session
                .usage_records()
                .iter()
                .map(|record| record.usage.total_tokens)
                .sum::<u64>(),
            20
        );
    }

    #[test]
    fn repeated_delegated_uncertainty_mirroring_is_idempotent_and_keeps_known_subtotal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mirror.jsonl");
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let mut session = Session::create(&path).unwrap();
        for pass in 0..3 {
            assert_eq!(
                mirror_delegated_uncertainty(&mut session, &model, true).unwrap(),
                pass == 0
            );
            record_delegated_usage_once(
                &mut session,
                DelegatedUsage {
                    agent_id: "child-1".into(),
                    turn_count: 1,
                    tool_call_count: 0,
                    endpoint: model.endpoint.id.clone(),
                    model: model.spec.id.clone(),
                    usage: Usage {
                        total_tokens: 10,
                        input_tokens: 10,
                        ..Usage::default()
                    },
                    cost: Some(octet_ai::Cost {
                        input: 7,
                        total: 7,
                        ..Default::default()
                    }),
                },
            )
            .unwrap();
        }
        assert_eq!(session.usage_uncertainty_records().len(), 1);
        assert_eq!(session.usage_records().len(), 1);
        assert_eq!(session.total_cost_microdollars(), 7);
        drop(session);
        let mut session = Session::open(path).unwrap();
        assert!(!mirror_delegated_uncertainty(&mut session, &model, true).unwrap());
        assert!(session.has_uncertain_usage());
        assert_eq!(session.usage_uncertainty_records().len(), 1);
        assert_eq!(session.total_cost_microdollars(), 7);
    }

    #[tokio::test]
    async fn abort_flag_wakes_waiters_and_stays_set() {
        let flag = Arc::new(AbortFlag::default());
        let waiter = {
            let flag = flag.clone();
            tokio::spawn(async move { flag.wait().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        flag.set();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("waiter must wake")
            .unwrap();
        // Late waiters return immediately.
        tokio::time::timeout(std::time::Duration::from_secs(1), flag.wait())
            .await
            .expect("level-triggered wait");
        assert!(flag.is_set());
    }

    fn active_tool_test_agent(
        directory: &std::path::Path,
        session: Session,
        extensions: ExtensionHost,
    ) -> Agent {
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        Agent::new(AgentConfig {
            client: AiClient::new(),
            model,
            session,
            system: "system".into(),
            sandbox: SandboxConfig::new(directory),
            effect_broker: EffectBroker::default(),
            extensions,
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: None,
        })
        .unwrap()
    }

    #[test]
    fn inactive_delegation_reports_no_active_workers() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("no-delegation.jsonl")).unwrap();
        let agent = active_tool_test_agent(directory.path(), session, ExtensionHost::new());
        assert_eq!(agent.active_delegated_worker_count(), 0);
    }

    fn active_tool_test_extensions(names: &[&'static str]) -> ExtensionHost {
        let mut extensions = ExtensionHost::new();
        for &name in names {
            extensions.tool(PromptTool {
                name,
                snippet: None,
                guidelines: &[],
            });
        }
        extensions
    }

    fn advertised_tool_names(agent: &Agent) -> Vec<String> {
        agent
            .registered_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect()
    }

    fn persisted_tool_result_texts(session: &Session) -> Vec<String> {
        session
            .context()
            .unwrap()
            .iter()
            .filter_map(|message| match message {
                Message::User(user) => Some(user.content.iter()),
                Message::Assistant(_) => None,
            })
            .flatten()
            .filter_map(|part| match part {
                UserPart::ToolResult(result) => Some(result.content.iter()),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                ToolResultPart::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn set_active_tool_names_narrows_the_advertised_surface_and_dispatch() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("active-tools.jsonl")).unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: octet_ai::ToolCallId("call-alpha".into()),
                        name: "alpha".into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    }),
                    AssistantPart::ToolCall(ToolCall {
                        async_execution: false,
                        id: octet_ai::ToolCallId("call-beta".into()),
                        name: "beta".into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    }),
                ],
                model: octet_ai::ModelId("test".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            session,
            active_tool_test_extensions(&["alpha", "beta"]),
        );
        assert_eq!(advertised_tool_names(&agent), ["alpha", "beta"]);
        let revision = agent.extensions.tool_snapshot().0;

        agent
            .set_active_tool_names(Some(BTreeSet::from(["alpha".to_owned()])))
            .unwrap();

        assert_eq!(advertised_tool_names(&agent), ["alpha"]);
        assert!(agent.extensions.tool_snapshot().0 > revision);

        // A persisted call issued before the change resolves against the
        // narrowed dispatch map: the deactivated tool gets the existing
        // unknown-tool result while the active tool still dispatches.
        agent.recover_pending_tools(false).await.unwrap();
        let texts = persisted_tool_result_texts(agent.session());
        assert!(
            texts.iter().any(|text| text.contains("unknown tool: beta")),
            "a deactivated tool must be refused by the dispatch map: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("`alpha` was not replayed")),
            "the still-active tool must remain dispatched: {texts:?}"
        );
    }

    #[test]
    fn set_active_tool_names_refuses_unknown_names_without_state_change() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("active-unknown.jsonl")).unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            session,
            active_tool_test_extensions(&["alpha", "beta"]),
        );
        let revision = agent.extensions.tool_snapshot().0;

        let error = agent
            .set_active_tool_names(Some(BTreeSet::from([
                "alpha".to_owned(),
                "ghost".to_owned(),
            ])))
            .unwrap_err();
        match error {
            AgentError::UnknownActiveTools(refused) => assert_eq!(refused, ["ghost"]),
            other => panic!("expected UnknownActiveTools, got {other:?}"),
        }
        assert_eq!(advertised_tool_names(&agent), ["alpha", "beta"]);
        assert_eq!(agent.extensions.tool_snapshot().0, revision);
    }

    #[test]
    fn set_active_tool_names_cannot_readmit_policy_excluded_tools() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("active-policy.jsonl")).unwrap();
        let mut extensions = active_tool_test_extensions(&["read", "write"]);
        extensions.set_tool_policy(|name| name != "write");
        let mut agent = active_tool_test_agent(directory.path(), session, extensions);

        assert_eq!(advertised_tool_names(&agent), ["read"]);
        assert_eq!(agent.registered_tool_names(), ["read"]);
        let revision = agent.extensions.tool_snapshot().0;

        for requested in [
            BTreeSet::from(["write".to_owned()]),
            BTreeSet::from(["read".to_owned(), "write".to_owned()]),
        ] {
            let error = agent.set_active_tool_names(Some(requested)).unwrap_err();
            match error {
                AgentError::UnknownActiveTools(refused) => assert_eq!(refused, ["write"]),
                other => panic!("expected UnknownActiveTools, got {other:?}"),
            }
        }
        assert_eq!(advertised_tool_names(&agent), ["read"]);
        assert_eq!(agent.extensions.tool_snapshot().0, revision);

        agent
            .set_active_tool_names(Some(BTreeSet::from(["read".to_owned()])))
            .unwrap();
        assert_eq!(advertised_tool_names(&agent), ["read"]);
    }

    #[test]
    fn set_active_tool_names_none_restores_the_host_policed_surface() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("active-restore.jsonl")).unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            session,
            active_tool_test_extensions(&["alpha", "beta"]),
        );

        agent
            .set_active_tool_names(Some(BTreeSet::from(["alpha".to_owned()])))
            .unwrap();
        assert_eq!(advertised_tool_names(&agent), ["alpha"]);
        // Deactivated names stay registered, so they can be requested again.
        assert_eq!(agent.registered_tool_names(), ["alpha", "beta"]);

        agent.set_active_tool_names(None).unwrap();
        assert_eq!(advertised_tool_names(&agent), ["alpha", "beta"]);
    }

    #[test]
    fn set_active_tool_names_bump_the_revision_the_run_host_observes() {
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("active-run-host.jsonl")).unwrap();
        let mut agent = active_tool_test_agent(
            directory.path(),
            session,
            active_tool_test_extensions(&["alpha", "beta"]),
        );
        // `Agent::prompt` hands a clone of the host to the streaming loop.
        let run_host = agent.extensions.clone();
        let before = run_host.tool_snapshot().0;

        agent
            .set_active_tool_names(Some(BTreeSet::from(["beta".to_owned()])))
            .unwrap();

        let (revision, tools) = run_host.tool_snapshot();
        assert!(revision > before);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.definition().name.clone())
                .collect::<Vec<_>>(),
            ["beta"]
        );
    }
}

#[cfg(test)]
mod inference_recovery_tests {
    use super::*;

    fn request() -> Request {
        Request {
            system: Some("system".into()),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(16),
            temperature: None,
            stop: Vec::new(),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            responses: None,
            output_format: OutputFormat::Text,
            output_modalities: OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: CacheRetention::default(),
            session_id: None,
        }
    }

    pub(super) fn model() -> Model {
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Codex;
        model
    }

    #[test]
    fn qualification_uses_host_runtime_and_rejects_indeterminate_remote_options() {
        let mut model = model();
        let mut request = request();
        assert!(qualified_inference_replacement(&model, &request));
        Arc::make_mut(&mut model.spec).capabilities.responses_lite = true;
        assert!(qualified_inference_replacement(&model, &request));
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Default;
        assert!(!qualified_inference_replacement(&model, &request));
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Codex;
        Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiChat;
        assert!(!qualified_inference_replacement(&model, &request));
        Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
        for kind in [
            "web_search_call",
            "computer_call",
            "mcp_call",
            "future_effect",
        ] {
            request.responses = Some(ResponsesOptions::full_replay(
                octet_ai::responses::ResponsesInput::new(vec![
                    octet_ai::responses::ResponsesItem::new(serde_json::json!({"type": kind}))
                        .unwrap(),
                ]),
            ));
            assert!(!qualified_inference_replacement(&model, &request), "{kind}");
        }
        request.responses = Some(ResponsesOptions {
            previous_response_id: Some("remote".into()),
            ..Default::default()
        });
        assert!(!qualified_inference_replacement(&model, &request));
        request.responses = Some(ResponsesOptions {
            context_management: Some(serde_json::json!([])),
            ..Default::default()
        });
        assert!(!qualified_inference_replacement(&model, &request));
        request.responses = Some(ResponsesOptions {
            store: true,
            ..Default::default()
        });
        assert!(!qualified_inference_replacement(&model, &request));
    }

    #[test]
    fn nonresumable_websocket_keeps_host_qualified_replacement_budget() {
        for qualified in [false, true] {
            let recovery = PendingProviderRecovery {
                error: AiError::StreamProtocol(
                    octet_ai::StreamProtocolError::ResponseNotResumable {
                        attempts: 0,
                        visible_output: true,
                        detail: "connection reset".into(),
                    },
                ),
                qualified,
                saw_generation: true,
                opened: true,
            };
            assert_eq!(
                recovery.replacement_limit(),
                if qualified {
                    MAX_INFERENCE_REPLACEMENTS
                } else {
                    0
                }
            );
            assert!(recovery.usage_unknown());
        }
    }

    #[test]
    fn replacement_taxonomy_only_admits_explicit_transient_boundaries() {
        for phase in [
            octet_ai::TransportPhase::Body,
            octet_ai::TransportPhase::ResponseHeaders,
        ] {
            for timeout in [true, false] {
                let error = AiError::Transport(octet_ai::TransportError {
                    phase,
                    timeout,
                    message: "interrupted".into(),
                });
                assert!(interrupted_inference_error(&error));
                let recovery = PendingProviderRecovery {
                    error,
                    qualified: true,
                    saw_generation: true,
                    opened: true,
                };
                assert_eq!(recovery.replacement_limit(), MAX_INFERENCE_REPLACEMENTS);
                assert!(recovery.usage_unknown());
            }
        }
        for error in [
            AiError::Decode(octet_ai::DecodeError::InvalidUtf8),
            AiError::Decode(octet_ai::DecodeError::Json("broken".into())),
            AiError::Decode(octet_ai::DecodeError::ResponseTooLarge),
            AiError::Decode(octet_ai::DecodeError::TooManyStreamEvents),
            AiError::StreamProtocol(octet_ai::StreamProtocolError::UnbalancedPart { index: 0 }),
            AiError::StreamProtocol(octet_ai::StreamProtocolError::UnexpectedEvent("bad".into())),
            AiError::Auth(octet_ai::AuthError::Resolve),
            AiError::Canceled,
            AiError::Provider(octet_ai::ProviderError {
                code: Some("invalid_request_error".into()),
                kind: None,
                message: "please try again".into(),
                request_id: None,
            }),
        ] {
            assert!(!interrupted_inference_error(&error), "{error:?}");
            let recovery = PendingProviderRecovery {
                error,
                qualified: true,
                saw_generation: true,
                opened: true,
            };
            assert_eq!(recovery.replacement_limit(), 0);
        }
        let failure = || {
            AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Body,
                timeout: false,
                message: "reset".into(),
            })
        };
        for saw_generation in [false, true] {
            let recovery = PendingProviderRecovery {
                error: failure(),
                qualified: false,
                saw_generation,
                opened: true,
            };
            assert_eq!(recovery.replacement_limit(), 0);
        }
    }

    #[test]
    fn only_provider_stream_json_gets_qualified_parse_recovery() {
        let wrap = |inner| AiError::StreamFailure {
            inner: Box::new(inner),
            progress: octet_ai::StreamProgress {
                provider_events: 1,
                decoded_events: 0,
                content_bytes: 0,
                buffered_bytes: 0,
                first_body_seen: true,
                elapsed_ms: 1,
                last_event_ms: Some(1),
            },
        };
        let malformed = wrap(AiError::Decode(octet_ai::DecodeError::Json(
            "malformed provider frame".into(),
        )));
        assert!(interrupted_inference_error(&malformed));
        assert!(interrupted_inference_error(&wrap(AiError::Decode(
            octet_ai::DecodeError::InvalidUtf8
        ))));
        assert!(!interrupted_inference_error(&AiError::Decode(
            octet_ai::DecodeError::InvalidUtf8
        )));
        assert_eq!(
            PendingProviderRecovery {
                error: malformed,
                qualified: false,
                saw_generation: true,
                opened: true
            }
            .replacement_limit(),
            0
        );
        for error in [
            AiError::Decode(octet_ai::DecodeError::InvalidProviderField(
                "usage overflow".into(),
            )),
            AiError::Decode(octet_ai::DecodeError::TooManyStreamEvents),
            AiError::Decode(octet_ai::DecodeError::ResponseTooLarge),
            AiError::StreamProtocol(octet_ai::StreamProtocolError::UnbalancedPart { index: 0 }),
        ] {
            assert!(!interrupted_inference_error(&wrap(error)));
        }
        assert!(!interrupted_inference_error(&AiError::Decode(
            octet_ai::DecodeError::Json("local request serialization".into())
        )));
    }

    #[test]
    fn durable_uncertainty_blocks_later_token_and_cost_ceilings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("uncertain.jsonl");
        let model = model();
        let mut session = Session::create(&path).unwrap();
        session
            .record_usage_uncertainty(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                "assistant_turn",
            )
            .unwrap();
        drop(session);
        let session = Session::open(&path).unwrap();
        assert!(matches!(
            reserve_request_tokens(&session, 1, 1, Some(u64::MAX)),
            Err(AgentError::UsageUncertain)
        ));
        assert!(matches!(
            reserve_request_cost(
                &session,
                &model,
                1,
                1,
                Some(u64::MAX),
                CacheRetention::Short
            ),
            Err(AgentError::UsageUncertain)
        ));
        assert!(reserve_request_tokens(&session, 1, 1, None).is_ok());
        assert!(reserve_request_cost(&session, &model, 1, 1, None, CacheRetention::Short).is_ok());
        for code in [
            "usage_not_included",
            "insufficient_quota",
            "billing_hard_limit_reached",
        ] {
            let error = AiError::Provider(octet_ai::ProviderError {
                code: Some(code.into()),
                kind: Some("rate_limit_exceeded".into()),
                message: "try again in 1s".into(),
                request_id: None,
            });
            assert!(!interrupted_inference_error(&error));
        }
    }

    #[test]
    fn permanent_codes_and_generic_connect_errors_never_authorize_outage_waiting() {
        let error = AiError::Provider(octet_ai::ProviderError {
            code: Some("invalid_request_error".into()),
            kind: Some("server_error".into()),
            message: "please try again after timeout".into(),
            request_id: None,
        });
        assert!(!retryable_stream_start(&error));
        assert!(!interrupted_inference_error(&error));
        assert!(!looks_like_context_error(&AiError::Decode(
            octet_ai::DecodeError::Json("context_length_exceeded".into())
        )));
        let recovery = PendingProviderRecovery {
            error: AiError::Transport(octet_ai::TransportError {
                phase: octet_ai::TransportPhase::Connect,
                timeout: false,
                message: "invalid certificate".into(),
            }),
            qualified: true,
            saw_generation: false,
            opened: false,
        };
        assert!(!recovery.waiting_for_network());
    }

    #[test]
    fn presend_credential_unavailability_has_no_unknown_billable_usage() {
        for qualified in [false, true] {
            let recovery = PendingProviderRecovery {
                error: AiError::Auth(octet_ai::AuthError::Unavailable),
                qualified,
                saw_generation: false,
                opened: false,
            };
            assert_eq!(
                recovery.replacement_limit(),
                if qualified { MAX_NETWORK_RETRIES } else { 0 }
            );
            assert!(!recovery.usage_unknown());
        }
    }

    #[test]
    fn retry_after_is_not_shortened_even_through_stream_failure_wrapper() {
        let error = AiError::Http(octet_ai::HttpError {
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            request_id: None,
            retry_after: Some(Duration::from_secs(120)),
            provider_code: None,
            body_snippet: None,
            retryable: true,
        });
        assert_eq!(retry_after(&error, 0), Duration::from_secs(120));
    }

    struct StopRecovery;
    #[async_trait::async_trait]
    impl ProviderRetryHook for StopRecovery {
        async fn provider_retry(&self, context: &ProviderRetryContext) -> ProviderRetryAdvice {
            assert_eq!(context.kind, ProviderRetryKind::InterruptedInference);
            ProviderRetryAdvice::Stop
        }
    }

    #[tokio::test]
    async fn hard_token_ceiling_and_hook_veto_stop_interrupted_inference_before_replacement() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        for hard_token_limit in [true, false] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("responses"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                    .set_body_string("data: {\"type\":\"response.created\",\"response\":{\"id\":\"failed\"}}\n\ndata: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"interrupted\"}\n\n"))
                .mount(&server).await;
            let directory = tempfile::tempdir().unwrap();
            let mut model = model();
            Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::Http;
            if hard_token_limit {
                // HTTP uncertainty coverage needs a genuinely capped route;
                // uncapped Codex hard ceilings now refuse before dispatch.
                Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
                    octet_ai::ResponsesRuntimeProfile::Default;
            }
            Arc::make_mut(&mut model.endpoint).base_url =
                url::Url::parse(&format!("{}/", server.uri())).unwrap();
            Arc::make_mut(&mut model.endpoint).auth = octet_ai::Auth::bearer("synthetic");
            let mut extensions = ExtensionHost::new();
            if !hard_token_limit {
                extensions.provider_retry_hook(StopRecovery);
            }
            let mut agent = Agent::new(AgentConfig {
                client: AiClient::new(),
                model,
                session: Session::create(directory.path().join("session.jsonl")).unwrap(),
                system: "system".into(),
                sandbox: SandboxConfig::new(directory.path()),
                effect_broker: EffectBroker::default(),
                extensions,
                max_turns: Some(1),
                reasoning: ReasoningConfig::Off,
                reasoning_mode: ReasoningMode::Standard,
                cache_retention: CacheRetention::Short,
                session_id: None,
            })
            .unwrap();
            if hard_token_limit {
                agent.set_max_session_tokens(Some(u64::MAX));
            }
            let error = agent.complete("finish").await.unwrap_err();
            if hard_token_limit {
                assert!(
                    matches!(
                        error,
                        AgentError::ProviderRecovery {
                            retries: 0,
                            usage_unknown: true,
                            ..
                        }
                    ),
                    "{error:?}"
                );
            }
            let requests = server.received_requests().await.unwrap();
            assert_eq!(
                requests.len(),
                1,
                "hard={hard_token_limit}: {:?}",
                requests
                    .iter()
                    .map(|request| (&request.method, &request.url))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[cfg(test)]
mod sustained_network_recovery_tests {
    use super::*;
    // The /fast tier-reservation test builds the same Codex model as the
    // inference-recovery tests; share that one constructor rather than
    // letting two copies drift.
    use super::inference_recovery_tests::model;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct OfflineTransport {
        calls: AtomicUsize,
        fail_until: usize,
    }
    #[async_trait::async_trait]
    impl octet_ai::HostStreamTransport for OfflineTransport {
        async fn stream(
            &self,
            model: octet_ai::HostStreamModel,
            _request: Request,
            _: Vec<octet_ai::Diagnostic>,
        ) -> Result<octet_ai::ResponseStream, AiError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.fail_until {
                return Err(AiError::Auth(octet_ai::AuthError::Unavailable));
            }
            Ok(Box::pin(futures_util::stream::iter([
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::Finished(octet_ai::Response {
                    message: AssistantMessage {
                        content: vec![AssistantPart::Text("recovered".into())],
                        model: model.id.clone(),
                        protocol: model.protocol,
                    },
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    cost: None,
                    response_id: None,
                    responses_output: None,
                    deferred: None,
                    diagnostics: Vec::new(),
                })),
            ])))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn qualified_presend_outage_waits_beyond_finite_budget_and_is_cancellable() {
        let directory = tempfile::tempdir().unwrap();
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Codex;
        let client = AiClient::new();
        let transport = Arc::new(OfflineTransport {
            calls: AtomicUsize::new(0),
            fail_until: usize::MAX,
        });
        client.register_host_stream_transport(model.endpoint.id.clone(), transport.clone());
        let mut agent = Agent::new(AgentConfig {
            client,
            model,
            session: Session::create(directory.path().join("session.jsonl")).unwrap(),
            system: "system".into(),
            sandbox: SandboxConfig::new(directory.path()),
            effect_broker: EffectBroker::default(),
            extensions: ExtensionHost::new(),
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        agent.set_max_session_tokens(Some(u64::MAX));
        assert!(matches!(
            agent.complete("bounded uncapped route").await,
            Err(AgentError::OutputLimitUnavailable)
        ));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        // The original long-outage behavior remains available only without a
        // hard ceiling on this cap-omitting Codex route.
        agent.set_max_session_tokens(None);
        let mut run = agent.prompt("wait through the outage").await.unwrap();
        let control = run.control();
        let mut waits = 0;
        let mut terminals = 0;
        let started = tokio::time::Instant::now();
        while let Some(event) = run.next().await {
            match event {
                AgentEvent::ProviderWaitingForNetwork { attempt, delay, .. } => {
                    waits += 1;
                    assert_eq!(attempt, waits);
                    assert!((Duration::from_secs(4)..=Duration::from_secs(60)).contains(&delay));
                    if waits == 20_200 {
                        control.abort();
                    }
                }
                AgentEvent::ProviderRetry { .. } => {
                    panic!("pre-send waiting spent inference replacements")
                }
                AgentEvent::RunFinished { reason, .. } => {
                    terminals += 1;
                    assert!(matches!(reason, FinishReason::Aborted));
                }
                _ => {}
            }
        }
        drop(run);
        assert_eq!(terminals, 1);
        assert_eq!(transport.calls.load(Ordering::SeqCst), waits);
        assert!(started.elapsed() > Duration::from_secs(14 * 24 * 60 * 60));
        // The refused hard-ceiling admission leaves the prompt's own user entry
        // plus the durable failed-turn marker, and the aborted prompt adds one
        // user entry. No assistant answer and no usage may be invented for
        // either of them.
        let entries = agent.session().entries();
        assert_eq!(entries.len(), 3, "{entries:#?}");
        assert!(matches!(
            &entries[0].value,
            EntryValue::Message(Message::User(user))
                if matches!(user.content.as_slice(), [UserPart::Text(text)] if text == "bounded uncapped route")
        ));
        assert!(matches!(
            &entries[1].value,
            EntryValue::Message(Message::Assistant(assistant))
                if assistant.content.iter().all(|part| matches!(part, AssistantPart::Text(_)))
        ));
        assert!(matches!(
            &entries[2].value,
            EntryValue::Message(Message::User(user))
                if matches!(user.content.as_slice(), [UserPart::Text(text)] if text == "wait through the outage")
        ));
        assert!(agent.session().usage_records().is_empty());
        assert!(!agent.session().has_uncertain_usage());
    }

    #[tokio::test(start_paused = true)]
    async fn auxiliary_recovery_is_scoped_bounded_and_budget_conservative() {
        use crate::events::ProviderOperation;
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        for operation in [
            ProviderOperation::LocalCompaction,
            ProviderOperation::NativeCompaction,
            ProviderOperation::TerminalGate,
        ] {
            for hard_budget in [false, true] {
                let (events, mut receiver) = mpsc::unbounded_channel();
                let abort = AbortFlag::default();
                let calls = AtomicUsize::new(0);
                let directory = tempfile::tempdir().unwrap();
                let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
                let result = recover_auxiliary(
                    AuxiliaryRecovery {
                        dispatch: AuxiliaryDispatch::default(),
                        session: &mut session,
                        run_id: "auxiliary-test",
                        resource_owner: "auxiliary-test",
                        retry_hooks: &[],
                        max_network_wait: None,
                        model: &model,
                        qualified: true,
                        enabled: true,
                        hard_budget,
                        abort: &abort,
                        events: &events,
                        operation,
                        session_id: "auxiliary-test",
                    },
                    |_deadline, _dispatch| {
                        let call = calls.fetch_add(1, Ordering::SeqCst);
                        async move {
                            if call == 0 {
                                Err(AiError::StreamProtocol(
                                    octet_ai::StreamProtocolError::PrematureEof,
                                )
                                .into())
                            } else {
                                Ok(())
                            }
                        }
                    },
                    |_, _| Ok(()),
                )
                .await;
                assert!(matches!(
                    receiver.try_recv(),
                    Ok(AgentEvent::ProviderUsageUncertain)
                ));
                if hard_budget {
                    assert!(matches!(
                        result,
                        Err(AgentError::ProviderRecovery {
                            retries: 0,
                            usage_unknown: true,
                            ..
                        })
                    ));
                    assert!(receiver.try_recv().is_err());
                } else {
                    assert!(result.is_ok());
                    assert!(
                        matches!(receiver.try_recv().unwrap(), AgentEvent::ProviderOperationRetry { operation: observed, max_attempts: Some(11), .. } if observed == operation)
                    );
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn auxiliary_network_wait_obeys_host_outage_limit() {
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let (events, mut receiver) = mpsc::unbounded_channel();
        let abort = AbortFlag::default();
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let result: Result<(), _> = recover_auxiliary(
            AuxiliaryRecovery {
                dispatch: AuxiliaryDispatch::default(),
                session: &mut session,
                run_id: "auxiliary-test",
                resource_owner: "auxiliary-test",
                retry_hooks: &[],
                max_network_wait: Some(Duration::from_secs(1)),
                model: &model,
                qualified: true,
                enabled: true,
                hard_budget: true,
                abort: &abort,
                events: &events,
                operation: crate::events::ProviderOperation::LocalCompaction,
                session_id: "bounded",
            },
            |_deadline, _dispatch| async {
                Err(AiError::Auth(octet_ai::AuthError::Unavailable).into())
            },
            |_, _| Ok(()),
        )
        .await;
        assert!(matches!(result, Err(AgentError::NetworkWaitLimit { .. })));
        assert!(matches!(
            receiver.try_recv(),
            Ok(AgentEvent::ProviderOperationRetry { .. })
        ));
    }

    #[test]
    fn network_backoff_is_bounded_and_jittered_even_after_counter_saturation() {
        for attempt in [0, 1, 2, 3, 100, usize::MAX] {
            let delay = network_wait_delay("run-a", attempt);
            assert!((Duration::from_secs(4)..=Duration::from_secs(60)).contains(&delay));
        }
        assert_ne!(
            network_wait_delay("run-a", 0),
            network_wait_delay("run-b", 0)
        );
    }

    #[test]
    fn typed_unknown_failed_and_all_5xx_require_qualified_replacement_authority() {
        for qualified in [false, true] {
            let provider = || octet_ai::ProviderError {
                code: Some("future_unknown_reason".into()),
                kind: None,
                message: "unknown".into(),
                request_id: None,
            };
            for (error, expected) in [
                (
                    AiError::ResponsesFailed(provider()),
                    if qualified { 11 } else { 0 },
                ),
                (AiError::Provider(provider()), 0),
                (
                    AiError::Http(octet_ai::HttpError {
                        status: http::StatusCode::from_u16(520).unwrap(),
                        request_id: None,
                        retry_after: None,
                        provider_code: None,
                        body_snippet: None,
                        retryable: false,
                    }),
                    if qualified { 29 } else { 0 },
                ),
            ] {
                let recovery = PendingProviderRecovery {
                    error,
                    qualified,
                    saw_generation: false,
                    opened: true,
                };
                assert_eq!(recovery.replacement_limit(), expected);
                assert!(recovery.usage_unknown());
            }
        }
    }

    #[test]
    fn permanent_response_codes_veto_context_sounding_text_and_preserve_rate_hints() {
        for code in [
            "cyber_policy",
            "bio_policy",
            "invalid_prompt",
            "misalignment_policy_violation",
            "server_is_overloaded",
            "slow_down",
            "invalid_api_key",
            "insufficient_quota",
        ] {
            let error = AiError::ResponsesFailed(octet_ai::ProviderError {
                code: Some(code.into()),
                kind: Some("context_length_exceeded".into()),
                message: "context length exceeded; try again in 1s".into(),
                request_id: None,
            });
            assert!(!looks_like_context_error(&error), "{code}");
            assert!(!interrupted_inference_error(&error), "{code}");
            assert!(!retryable_stream_start(&error), "{code}");
        }
        let error = AiError::ResponsesFailed(octet_ai::ProviderError {
            code: Some("rate_limit_exceeded".into()),
            kind: None,
            message: "try again in 11054ms".into(),
            request_id: None,
        });
        assert_eq!(retry_after(&error, 0), Duration::from_millis(11054));
    }
    #[test]
    fn hard_cost_reservation_covers_pricier_anthropic_server_fallbacks() {
        use octet_ai::declarations::{
            AnthropicCompatPreset, AnthropicFallbackCost, AnthropicFallbackModel,
        };
        use octet_ai::{Pricing, TokenRate};

        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("claude-sonnet-4-5".into()))
            .unwrap();
        Arc::make_mut(&mut model.spec).pricing = Some(Pricing {
            input: TokenRate(1_000_000),
            output: TokenRate(1_000_000),
            cache_read: TokenRate(1_000_000),
            cache_write_5m: TokenRate(1_000_000),
            cache_write_1h: None,
            reasoning: None,
            tiers: vec![],
        });
        let base = worst_case_request_cost(&model, 1_000_000, 1_000_000, None).unwrap();
        assert_eq!(base, 3_000_000);
        Arc::make_mut(&mut model.spec).preset.anthropic_compat = Some(AnthropicCompatPreset {
            allowed_fallback_models: vec![AnthropicFallbackModel {
                provider: "anthropic".into(),
                model: "dearer".into(),
                cost: Some(AnthropicFallbackCost {
                    input: 5.0,
                    output: 20.0,
                    cache_read: 0.5,
                    cache_write: 6.0,
                }),
            }],
            ..Default::default()
        });
        assert_eq!(
            worst_case_request_cost(&model, 1_000_000, 1_000_000, None),
            Some(30_000_000)
        );
        let directory = tempfile::tempdir().unwrap();
        let session = Session::create(directory.path().join("cost.jsonl")).unwrap();
        assert!(matches!(
            reserve_request_cost(
                &session,
                &model,
                1_000_000,
                1_000_000,
                Some(base + 1),
                CacheRetention::Short
            ),
            Err(AgentError::CostLimit { .. })
        ));
        assert!(model.spec.cache.supports_long_retention);
        assert!(reserve_request_cost(
            &session,
            &model,
            1,
            1,
            Some(u64::MAX),
            CacheRetention::Short,
        )
        .is_ok());
        assert!(matches!(
            reserve_request_cost(&session, &model, 1, 1, Some(u64::MAX), CacheRetention::Long),
            Err(AgentError::CostUnavailable { .. })
        ));
        assert!(reserve_request_cost(&session, &model, 1, 1, None, CacheRetention::Long,).is_ok());
        Arc::make_mut(&mut model.spec)
            .preset
            .anthropic_compat
            .as_mut()
            .unwrap()
            .allowed_fallback_models[0]
            .cost = None;
        assert!(matches!(
            reserve_request_cost(
                &session,
                &model,
                1,
                1,
                Some(u64::MAX),
                CacheRetention::Short
            ),
            Err(AgentError::CostUnavailable { .. })
        ));
    }

    #[test]
    fn tier_reservation_uses_the_declared_tariff_and_blocks_unpriced_history() {
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Codex;
        Arc::make_mut(&mut model.spec).api_name = "gpt-5.5".into();
        let base = worst_case_request_cost(&model, 1_000_000, 1_000_000, None).unwrap();
        let priority =
            worst_case_request_cost(&model, 1_000_000, 1_000_000, Some(ServiceTier::Priority))
                .unwrap();
        assert!(priority >= base.saturating_mul(5) / 2);
        assert!(worst_case_request_cost(&model, 1, 1, Some(ServiceTier::Auto)).is_none());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unpriced.jsonl");
        let mut session = Session::create(&path).unwrap();
        assert!(matches!(
            reserve_request_cost_with_tier(
                &session,
                &model,
                1_000_000,
                1_000_000,
                Some(base + 1),
                Some(ServiceTier::Priority),
                CacheRetention::Short,
            ),
            Err(AgentError::CostLimit { .. })
        ));
        session
            .record_compaction_usage(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    input_tokens: 1,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(path).unwrap();
        assert!(matches!(
            reserve_request_cost(
                &reopened,
                &model,
                1,
                1,
                Some(u64::MAX),
                CacheRetention::Short
            ),
            Err(AgentError::CostUnavailable { .. })
        ));
        assert!(reserve_request_cost(&reopened, &model, 1, 1, None, CacheRetention::Short).is_ok());
    }

    #[test]
    fn tariff_reservations_bound_exact_cost_across_usage_buckets_and_context_tiers() {
        use octet_ai::{Pricing, PricingTier, ResponsesRuntimeProfile, TokenRate};
        let mut model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-5.4-mini-responses".into()))
            .unwrap();
        let mut checked = 0usize;
        for one_hour in [None, Some(TokenRate(31_000_000))] {
            let pricing = Pricing {
                input: TokenRate(2_000_000),
                output: TokenRate(10_000_000),
                cache_read: TokenRate(700_000),
                cache_write_5m: TokenRate(3_000_000),
                cache_write_1h: one_hour,
                reasoning: Some(TokenRate(13_000_000)),
                tiers: vec![PricingTier {
                    min_input_tokens: 200_000,
                    input: Some(TokenRate(12_000_000)),
                    output: Some(TokenRate(20_000_000)),
                    cache_read: None,
                    cache_write_5m: Some(TokenRate(2_000_000)),
                    cache_write_1h: None,
                    reasoning: Some(TokenRate(30_000_000)),
                }],
            };
            Arc::make_mut(&mut model.spec).pricing = Some(pricing.clone());
            for profile in [
                ResponsesRuntimeProfile::Default,
                ResponsesRuntimeProfile::Codex,
            ] {
                Arc::make_mut(&mut model.endpoint).runtime.responses_profile = profile;
                for api_name in ["gpt-5.4", "gpt-5.5"] {
                    Arc::make_mut(&mut model.spec).api_name = api_name.into();
                    for tier in [
                        None,
                        Some(ServiceTier::Default),
                        Some(ServiceTier::Flex),
                        Some(ServiceTier::Priority),
                    ] {
                        if profile == ResponsesRuntimeProfile::Default
                            && matches!(tier, Some(ServiceTier::Flex | ServiceTier::Priority))
                        {
                            assert!(worst_case_request_cost(&model, 200_001, 8192, tier).is_none());
                            continue;
                        }
                        for input in [0, 1, 7, 199_999, 200_000, 200_001] {
                            for output in [0, 1, 29, 8192] {
                                let reserved =
                                    worst_case_request_cost(&model, input, output, tier).unwrap();
                                for bucket in 0..5 {
                                    let mut usage = Usage {
                                        output_tokens: output,
                                        total_tokens: input + output,
                                        ..Usage::default()
                                    };
                                    match bucket {
                                        0 => usage.input_tokens = input,
                                        1 => usage.cache_read_tokens = input,
                                        2 => usage.cache_write_tokens = input,
                                        3 => {
                                            usage.cache_write_tokens = input;
                                            usage.cache_write_1h_tokens = input;
                                        }
                                        _ => {
                                            usage.input_tokens = input / 3;
                                            usage.cache_read_tokens = input / 3;
                                            usage.cache_write_tokens = input
                                                - usage.input_tokens
                                                - usage.cache_read_tokens;
                                            usage.cache_write_1h_tokens =
                                                usage.cache_write_tokens / 2;
                                        }
                                    }
                                    for reasoning in [0, output / 2, output] {
                                        usage.reasoning_tokens = reasoning;
                                        let actual = octet_ai::responses_cost_of(
                                            &pricing, &usage, profile, api_name, tier, None,
                                        )
                                        .unwrap()
                                        .unwrap();
                                        let picodollars = u128::from(actual.total)
                                            * u128::from(PICODOLLARS_PER_MICRODOLLAR)
                                            + u128::from(actual.total_picodollars_remainder);
                                        assert!(picodollars <= u128::from(reserved) * u128::from(PICODOLLARS_PER_MICRODOLLAR),
                                            "under-reservation: profile={profile:?} model={api_name} tier={tier:?} usage={usage:?} actual={actual:?} reserved={reserved}");
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                    assert!(worst_case_request_cost(
                        &model,
                        200_001,
                        8192,
                        Some(ServiceTier::Auto)
                    )
                    .is_none());
                }
            }
        }
        assert_eq!(checked, 8640);
    }

    /// Row `/fast`: the worst-case reservation must hold across a restart, a
    /// durable exact auxiliary cost, and a hard budget boundary. A selected
    /// priority tier raises the pre-request reservation above the untiered
    /// value; a cheaper provider echo cannot lower it because the reservation
    /// helper has no echo input at all.
    #[test]
    fn tier_reservations_hold_across_restart_and_bound_the_hard_budget() {
        use octet_ai::{Cost, Pricing, TokenRate};
        let mut model = model();
        Arc::make_mut(&mut model.spec).api_name = "gpt-5.5".into();
        assert_eq!(
            model.endpoint.runtime.responses_profile,
            octet_ai::ResponsesRuntimeProfile::Codex
        );
        let pricing = Pricing {
            input: TokenRate(2_000_000),
            output: TokenRate(10_000_000),
            cache_read: TokenRate(700_000),
            cache_write_5m: TokenRate(3_000_000),
            cache_write_1h: None,
            reasoning: Some(TokenRate(13_000_000)),
            tiers: Vec::new(),
        };
        let mut model = model;
        Arc::make_mut(&mut model.spec).pricing = Some(pricing);
        let input = 12_345u64;
        let output = 4_096u64;
        let base = worst_case_request_cost(&model, input, output, None).unwrap();
        let priority =
            worst_case_request_cost(&model, input, output, Some(ServiceTier::Priority)).unwrap();
        assert!(
            priority >= base.saturating_mul(2),
            "the gpt-5.5 priority tariff must bound the reservation: base={base} priority={priority}"
        );
        // `auto` cannot be reserved at all; unknown metadata is never priced.
        assert!(worst_case_request_cost(&model, input, output, Some(ServiceTier::Auto)).is_none());

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("restart-budget.jsonl");
        let mut session = Session::create(&path).unwrap();
        // One durable, exactly priced auxiliary operation: local summaries do
        // not carry the main request tier, and their known cost must still
        // count against a later main-request reservation after a restart.
        let durable_cost = Cost {
            input: 400,
            output: 600,
            total: 1_000,
            ..Cost::default()
        };
        session
            .record_compaction_usage(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    input_tokens: 20,
                    output_tokens: 30,
                    total_tokens: 50,
                    ..Usage::default()
                },
                Some(durable_cost),
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.total_cost_microdollars(), 1_000);
        assert!(!reopened.has_unpriced_usage());

        // Exact boundary: current + the tier-aware reservation is allowed.
        assert!(reserve_request_cost_with_tier(
            &reopened,
            &model,
            input,
            output,
            Some(1_000 + priority),
            Some(ServiceTier::Priority),
            CacheRetention::Short,
        )
        .is_ok());
        // One microdollar tighter is refused with the same reservation.
        assert!(matches!(
            reserve_request_cost_with_tier(
                &reopened,
                &model,
                input,
                output,
                Some(1_000 + priority - 1),
                Some(ServiceTier::Priority),
                CacheRetention::Short,
            ),
            Err(AgentError::CostLimit {
                current: 1_000,
                reserved,
                ..
            }) if reserved == priority
        ));
        // The selected tier is load-bearing: the same budget admits the
        // untiered reservation used by auxiliary operations, which is exactly
        // why a priority main request must reserve the tier-aware amount.
        assert!(reserve_request_cost(
            &reopened,
            &model,
            input,
            output,
            Some(1_000 + priority - 1),
            CacheRetention::Short
        )
        .is_ok());
        // A cheap provider echo cannot reduce the reservation: the helper has
        // no echo input and always prices the requested tier.
        let reserved_again =
            worst_case_request_cost(&model, input, output, Some(ServiceTier::Priority)).unwrap();
        assert_eq!(reserved_again, priority);
        // Restart does not erase exposure either: an unpriced durable record
        // blocks the same hard budget on a reopened session.
        let mut session = reopened;
        session
            .record_terminal_gate_usage(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    input_tokens: 1,
                    total_tokens: 1,
                    ..Usage::default()
                },
                None,
                Some(true),
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(matches!(
            reserve_request_cost_with_tier(
                &reopened,
                &model,
                input,
                output,
                Some(u64::MAX),
                Some(ServiceTier::Priority),
                CacheRetention::Short,
            ),
            Err(AgentError::CostUnavailable { .. })
        ));
    }
}

#[cfg(test)]
mod auxiliary_settlement_tests;
