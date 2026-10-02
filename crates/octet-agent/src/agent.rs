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
    UsageRecordKind, UsageUncertaintyBound,
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

mod budget;
mod compaction;
mod context_estimate;
mod control;
mod deferred;
mod delegation_setup;
mod error;
mod images;
mod live_output;
mod parallel_reads;
mod recovery;
mod run_handle;
mod terminal_gate;
mod tool_admission;
mod tool_results;
mod turn_loop;

use self::budget::*;
use self::compaction::*;
use self::context_estimate::*;
pub use self::control::PreparedSteering;
pub use self::control::RunControl;
pub use self::control::SteeringReceipt;
use self::control::*;
pub use self::deferred::deferred_model_identity;
pub use self::deferred::AiDeferredPollSource;
pub use self::deferred::DeferredPollReply;
pub use self::deferred::DeferredPollSource;
pub use self::deferred::DeferredRunOutcome;
use self::deferred::*;
pub use self::error::public_error_diagnostic;
pub use self::error::AgentError;
use self::error::*;
use self::images::*;
#[cfg(any(unix, windows))]
pub use self::live_output::PartialOutputCheckpointStats;
use self::live_output::*;
use self::parallel_reads::*;
use self::recovery::*;
pub use self::run_handle::RequestContextEstimate;
pub use self::run_handle::Run;
pub use self::run_handle::RunOutput;
use self::terminal_gate::*;
use self::tool_admission::*;
use self::tool_results::*;

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
    /// Most independent observations one ordered read wave runs at once. Set
    /// through [`Agent::set_parallel_read_wave_width`].
    parallel_read_wave_width: usize,
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
/// Smallest default read-wave width: the width on machines with fewer CPUs.
const MIN_PARALLEL_READ_WAVE_WIDTH: usize = 4;

/// Default width of one ordered read wave: one call per CPU this process may
/// use, between [`MIN_PARALLEL_READ_WAVE_WIDTH`] and [`MAX_TOOL_CALLS_PER_TURN`].
///
/// The width bounds resources, not safety. A call joins a wave only when its
/// exact host classification makes it an independent observation, and every
/// other call is a barrier, so a wave of any width admits only calls that may
/// overlap. Results are still committed in emitted order.
fn default_parallel_read_wave_width() -> usize {
    std::thread::available_parallelism()
        .map_or(MIN_PARALLEL_READ_WAVE_WIDTH, std::num::NonZeroUsize::get)
        .clamp(MIN_PARALLEL_READ_WAVE_WIDTH, MAX_TOOL_CALLS_PER_TURN)
}
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

#[cfg(test)]
thread_local! {
    static TERMINAL_GATE_TEXT_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TERMINAL_GATE_INITIAL_SUMMARIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

#[cfg(test)]
thread_local! {
    static PROVIDER_CONTEXT_ENTRY_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PROVIDER_CONTEXT_USAGE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
            parallel_read_wave_width: default_parallel_read_wave_width(),
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
        let system = self.model_visible_system(true);
        let responses = match self.auto_compaction_mode {
            AgentCompactionMode::NativeResponses
                if !self.model.responses_features().reasoning_effort_updates =>
            {
                Some(native_responses_options(
                    &self.session,
                    &self.model,
                    &system,
                    self.service_tier,
                )?)
            }
            AgentCompactionMode::NativeResponses
            | AgentCompactionMode::Local
            | AgentCompactionMode::Disabled => {
                durable_responses_options(&self.session, &self.model, &system, self.service_tier)?
            }
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
            system: (!system.is_empty()).then_some(system),
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
            request_uncertainty_bound(&self.model, input_tokens, 1, None, CacheRetention::Short),
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

    pub(crate) fn append_system_instructions(&mut self, instructions: String) {
        if !self.system.is_empty() {
            self.system.push_str("\n\n");
        }
        self.system.push_str(&instructions);
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

    /// Provider-advertised output ceiling for the active model.
    pub fn max_output_tokens(&self) -> u64 {
        self.max_output_tokens
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

    /// Set how many independent observations one ordered read wave may run at
    /// once, clamped to `1..=32`. A width of one runs every call in turn.
    ///
    /// The default follows the CPUs this process may use, never below four.
    /// Classification, not the width, decides which calls may overlap.
    pub fn set_parallel_read_wave_width(&mut self, width: usize) {
        self.parallel_read_wave_width = width.clamp(1, MAX_TOOL_CALLS_PER_TURN);
    }

    /// How many independent observations one ordered read wave may run at once.
    pub fn parallel_read_wave_width(&self) -> usize {
        self.parallel_read_wave_width
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
        self.prompt_with_tools(input.into(), true, false).await
    }

    /// Begins a run with best-effort Responses WebSocket prewarming before
    /// its first admitted provider request. Only the pre-submission durable
    /// context is warmed, without generating or persisting an assistant turn.
    ///
    /// Prewarming is driven by the run stream, shares its cancellation, and
    /// is bounded to thirty seconds (or the shorter endpoint timeout). Errors
    /// do not prevent ordinary inference or its HTTP/SSE fallback. Non-WebSocket
    /// routes are unchanged; an already-live connection needs no new warmup.
    pub async fn prompt_with_responses_prewarm(
        &mut self,
        input: impl Into<UserInput>,
    ) -> Result<Run<'_>, AgentError> {
        self.prompt_with_tools(input.into(), true, true).await
    }

    /// Begins a run whose provider requests expose no tools. This is used for
    /// explicit answer-now flows that must synthesize from existing evidence.
    pub async fn prompt_without_tools(
        &mut self,
        input: impl Into<UserInput>,
    ) -> Result<Run<'_>, AgentError> {
        self.prompt_with_tools(input.into(), false, false).await
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
mod tests;

#[cfg(test)]
mod inference_recovery_tests;

#[cfg(test)]
mod sustained_network_recovery_tests;

#[cfg(test)]
mod auxiliary_settlement_tests;
