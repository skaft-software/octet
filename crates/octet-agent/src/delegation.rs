//! Bounded V2 collaboration runtime for delegated coding agents.
//!
//! The model capability only advertises that collaboration is useful. This
//! module owns the host-side semantics: isolated child sessions, lifecycle and
//! message routing, bounded concurrency/depth, cancellation, and durable
//! provenance.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use octet_ai::{AssistantPart, Cost, ToolDef, Usage, PICODOLLARS_PER_MICRODOLLAR};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch, Notify, OwnedSemaphorePermit, Semaphore};

use crate::agent::{Agent, AgentCompactionMode, AgentConfig, AgentError, CompletionPolicy};
use crate::effect::ToolEffect;
use crate::events::{
    AgentEvent, DelegationOrchestrationProvenance, DelegationPolicySource,
    DelegationTelemetryChild, DelegationTelemetrySnapshot, FinishReason,
};
use crate::extension::ExtensionHost;
use crate::sandbox::EffectiveToolPolicy;
use crate::secure_fs::{self, SecureFileError};
use crate::session::{Session, SessionError, UsageUncertaintyBound};
use crate::telemetry::{
    schema::{DelegationSpan, EmptyAttributes},
    spans::TelemetryContext,
};
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};

const ROOT_AGENT_ID: &str = "root";
const ROOT_AGENT_PATH: &str = "/root";
const COMMAND_CHANNEL_CAPACITY: usize = 32;
const MAX_TELEMETRY_FAILURE_BYTES: usize = 4 * 1024;
/// Coalesce streaming progress so a chatty provider cannot flood the root UI.
const STREAMED_OUTPUT_UPDATE_INTERVAL: Duration = Duration::from_millis(100);
/// Rolling per-child tool-activity entries retained for owner inspection.
const MAX_CHILD_TOOL_ACTIVITY: usize = 6;
/// Bounded single-line summary of one child tool call's arguments.
const MAX_TOOL_ARGS_SUMMARY_BYTES: usize = 160;
const MAX_PROVENANCE_TEXT_BYTES: usize = 128 * 1024;
const MAX_MAILBOX_MESSAGES: usize = 64;
const MAX_MAILBOX_BYTES: usize = 1024 * 1024;
// A running worker can become idle after up to one full command channel was
// accepted as steering. Preserve those already-persisted messages for its next
// task without allowing an unbounded per-agent queue.
const MAX_PENDING_MESSAGES: usize = COMMAND_CHANNEL_CAPACITY + 64;
const MAX_PENDING_MESSAGE_BYTES: usize = (COMMAND_CHANNEL_CAPACITY + 1) * MAX_PROVENANCE_TEXT_BYTES;
const MAX_QUEUED_FOLLOW_UPS: usize = COMMAND_CHANNEL_CAPACITY;
const MAX_QUEUED_FOLLOW_UP_BYTES: usize =
    (COMMAND_CHANNEL_CAPACITY + 1) * MAX_PROVENANCE_TEXT_BYTES;
/// A task that cannot be durably appended is retried only a few times. This is
/// deliberately not an exactly-once side-effect guarantee: child-session
/// persistence remains the authority for whether an input was accepted.
const MAX_UNDELIVERED_TASK_ATTEMPTS: u8 = 3;
const MAX_TOOL_TIMEOUT_MS: u64 = 3_600_000;
/// Extension children share one host permit pool; eight active workers leave
/// headroom for root-side native children as well.
const MAX_EXTENSION_ACTIVE_CHILDREN: usize = 8;
/// Bounded history retained per extension resource owner.
const MAX_EXTENSION_OWNED_CHILDREN: usize = 32;
/// Per-run turn budgets may be set up to 256 turns; `None` inherits the
/// parent session limit exactly (unlimited parents stay unlimited).
const MAX_EXTENSION_TURNS: u64 = 256;
/// Explicit child cost ceilings may be set up to $50; `None` removes the
/// child-specific ceiling while the parent session ceiling still applies.
const MAX_EXTENSION_COST_MICRODOLLARS: u64 = 50_000_000;
/// Optional hard worker wall clock up to 24 hours; `None` runs without a
/// wall-clock kill (workers can still be interrupted or stopped).
const MAX_EXTENSION_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;
/// Standard tools an extension child may hold. Read/search workers are the
/// conservative default; edit/write/bash workers inherit the parent session's
/// approval policy through the shared effect broker.
const EXTENSION_CHILD_TOOLS: [&str; 5] = ["read", "search", "edit", "write", "bash"];
/// Host-reserved names installed by V2 collaboration overlays.
pub const COLLABORATION_TOOL_NAMES: [&str; 6] = [
    "spawn_agent",
    "followup_task",
    "send_message",
    "wait_agent",
    "list_agents",
    "interrupt_agent",
];

fn short_sha256(value: &str, bytes: usize) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..bytes]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn extension_owner_task_prefix(principal: &str, resource_owner: &str) -> String {
    format!(
        "ext-{}-{}",
        short_sha256(principal, 6),
        short_sha256(resource_owner, 4)
    )
}

/// Returns whether a retained delegated-session path has the exact filename
/// shape generated for an extension principal and resource owner.
///
/// This is a defense-in-depth check for hosts that reconstruct extension child
/// authorization from durable provenance after a restart. Callers must still
/// validate that provenance and bind the parent session independently.
pub fn extension_delegated_session_matches_owner(
    principal: &str,
    resource_owner: &str,
    session_path: &Path,
) -> bool {
    let Some(stem) = session_path.file_stem().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some((sequence, task_name)) = stem.split_once('-') else {
        return false;
    };
    if sequence.len() != 4 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let task_prefix = format!(
        "{}-task-",
        extension_owner_task_prefix(principal, resource_owner)
    );
    task_name.strip_prefix(&task_prefix).is_some_and(|digest| {
        digest.len() == 12
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Returns a path-free opaque reference for one host-owned delegated session.
///
/// The reference is derived only from the cryptographically random private team
/// directory name and the host-generated child filename. It is safe to expose
/// to extension presentation and can be resolved only by a host that can
/// securely inventory its private delegation directory.
pub fn delegated_session_reference(session_path: &Path) -> Option<String> {
    let team = session_path.parent()?.file_name()?.to_str()?;
    let child = session_path.file_name()?.to_str()?;
    if !team.starts_with("team-")
        || team.len() > 128
        || !team
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || child.len() > 256
        || !child.ends_with(".jsonl")
        || !child
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(team.as_bytes());
    hasher.update(b"/");
    hasher.update(child.as_bytes());
    let digest = hasher.finalize();
    Some(format!(
        "agent-session:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// Returns whether this target build implements the advertised collaboration version.
pub fn delegation_runtime_supports(version: octet_ai::AgentDelegation) -> bool {
    matches!(version, octet_ai::AgentDelegation::V2)
        && cfg!(any(target_os = "linux", target_os = "macos", windows))
}

/// Whether delegated agents are merely available or should be used
/// proactively when parallel work would materially improve the result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DelegationMode {
    /// Expose collaboration tools without instructing the model to delegate.
    #[default]
    Available,
    /// Instruct the model to delegate suitable independent work proactively.
    Proactive,
}

/// Hard host-side limits for one delegation team.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DelegationLimits {
    /// Maximum number of agents executing at once, including the root agent.
    pub max_concurrent_agents: usize,
    /// Maximum child depth below the root (`1` permits children only).
    pub max_depth: usize,
    /// Maximum agents created during one owning run.
    pub max_total_agents: usize,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 10,
            max_depth: 2,
            max_total_agents: 32,
        }
    }
}

/// Configuration for a durable V2 delegation team.
#[derive(Clone, Debug)]
pub struct DelegationConfig {
    /// Parent directory under which a private team directory is created.
    pub session_directory: PathBuf,
    /// Host-side bounds applied independently of model behavior.
    pub limits: DelegationLimits,
    /// Whether the root receives proactive delegation guidance.
    pub mode: DelegationMode,
}

impl DelegationConfig {
    /// Creates an available-on-demand delegation configuration.
    pub fn new(session_directory: impl Into<PathBuf>) -> Self {
        Self {
            session_directory: session_directory.into(),
            limits: DelegationLimits::default(),
            mode: DelegationMode::Available,
        }
    }

    /// Enables proactive delegation guidance.
    pub fn proactive(mut self) -> Self {
        self.mode = DelegationMode::Proactive;
        self
    }

    fn validate(&self) -> Result<(), DelegationError> {
        if self.limits.max_concurrent_agents < 2 {
            return Err(DelegationError::InvalidConfig(
                "max_concurrent_agents must be at least 2 (root plus one child)".into(),
            ));
        }
        if self.limits.max_concurrent_agents > 32 {
            return Err(DelegationError::InvalidConfig(
                "max_concurrent_agents must not exceed 32".into(),
            ));
        }
        if self.limits.max_depth == 0 || self.limits.max_depth > 8 {
            return Err(DelegationError::InvalidConfig(
                "max_depth must be between 1 and 8".into(),
            ));
        }
        if self.limits.max_total_agents < self.limits.max_concurrent_agents
            || self.limits.max_total_agents > 256
        {
            return Err(DelegationError::InvalidConfig(
                "max_total_agents must be at least max_concurrent_agents and at most 256".into(),
            ));
        }
        Ok(())
    }
}

/// Failure while configuring or operating the delegation runtime.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    /// The supplied host limits are invalid.
    #[error("invalid delegation configuration: {0}")]
    InvalidConfig(String),
    /// Delegation was already attached to this agent.
    #[error("delegation is already enabled for this agent")]
    AlreadyEnabled,
    /// A collaboration tool name collides with an existing host tool.
    #[error("delegation tool name is already registered: {0}")]
    DuplicateTool(String),
    /// A descriptor-bound private filesystem operation failed.
    #[error("delegation persistence failed: {0}")]
    SecureFile(#[from] SecureFileError),
    /// A filesystem operation failed.
    #[error("delegation persistence failed: {0}")]
    Io(#[from] io::Error),
    /// Child session persistence failed.
    #[error("delegated session failed: {0}")]
    Session(#[from] SessionError),
    /// A child agent could not be initialized.
    #[error("delegated agent failed: {0}")]
    Agent(#[from] Box<AgentError>),
    /// A session-owned worker could not be handed over as a launchable
    /// interactive session.
    #[error("delegated session is not launchable: {0}")]
    Unlaunchable(String),
    /// Delegation activation failed and secure rollback also could not finish.
    #[error("delegation activation failed ({activation}); rollback failed ({rollback})")]
    ActivationRollback {
        /// Original activation failure.
        activation: String,
        /// Descriptor-bound cleanup failure.
        rollback: String,
    },
}

impl From<AgentError> for DelegationError {
    fn from(error: AgentError) -> Self {
        Self::Agent(Box::new(error))
    }
}

/// Durable status exposed by `list_agents` and `wait_agent`.
///
/// Lifetime is session-scoped: a worker survives the parent turn that spawned
/// it. Retirement at run end is no longer the default; instead the record
/// moves to [`DelegatedAgentStatus::Detached`] (recoverable) or
/// [`DelegatedAgentStatus::AwaitingApproval`] (parked on new authority), and
/// only an explicit session/process teardown reaches
/// [`DelegatedAgentStatus::Shutdown`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DelegatedAgentStatus {
    /// The worker exists but has not started its task yet.
    Pending,
    /// The worker is attached and waiting for an explicit follow-up task.
    Idle,
    /// The worker is executing a task.
    Running,
    /// The latest task completed successfully.
    Completed {
        /// Bounded visible output from the completed run.
        output: String,
    },
    /// The latest task exhausted its host-owned turn budget.
    LimitReached {
        /// Bounded visible output accumulated before the turn budget ended.
        output: String,
        /// Number of turns completed when the budget was exhausted.
        turn_count: u64,
        /// Host-owned maximum number of turns for the run.
        turn_limit: u64,
    },
    /// The latest task was interrupted.
    Interrupted,
    /// The latest task failed.
    Failed {
        /// Bounded failure diagnostic.
        error: String,
    },
    /// The worker exceeded its host-owned wall deadline.
    TimedOut,
    /// The worker is owned by the session, not the run, and has no live task
    /// in this process. It survives the parent turn and is recoverable: a
    /// later turn (or a restarted process holding the durable record) can
    /// reattach and continue, steer, or stop it. It is never silently
    /// forgotten.
    Detached,
    /// A detached worker parked on an effect that requires new authority.
    ///
    /// The worker neither proceeds nor blocks forever: it settled without
    /// acting because no approval authority was attached to this run. A later
    /// turn that reattaches can supply the decision and resume it.
    AwaitingApproval {
        /// Bounded diagnostic describing the parked decision.
        reason: String,
    },
    /// The worker was shut down and cannot accept more work.
    Shutdown,
}

impl DelegatedAgentStatus {
    fn is_running(&self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }

    /// Whether a durable record in this state can be reattached and resumed.
    fn is_recoverable(&self) -> bool {
        matches!(self, Self::Detached | Self::AwaitingApproval { .. })
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed { .. } => "completed",
            Self::LimitReached { .. } => "limit_reached",
            Self::Interrupted => "interrupted",
            Self::Failed { .. } => "failed",
            Self::TimedOut => "timed_out",
            Self::Detached => "detached",
            Self::AwaitingApproval { .. } => "awaiting_approval",
            Self::Shutdown => "shutdown",
        }
    }
}

#[derive(Clone)]
pub(crate) struct DelegationBinding {
    manager: Arc<DelegationManager>,
    identity: AgentIdentity,
    system_instructions: Arc<str>,
}

impl DelegationBinding {
    /// Installs the owning agent's explicit span observer for child runs.
    pub(crate) fn set_span_context(&self, context: TelemetryContext) {
        self.manager.set_span_context(context);
    }
}

/// Secret-free requested or host-confirmed worker selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentModelSelection {
    /// Configured provider identifier.
    pub provider: String,
    /// Configured model identifier or resolved host model.
    pub model: String,
    /// Reasoning selection or supported choices.
    pub reasoning: String,
}
impl Default for AgentModelSelection {
    fn default() -> Self {
        Self {
            provider: "inherit".into(),
            model: "inherit".into(),
            reasoning: "inherit".into(),
        }
    }
}
/// Bounded public configured-model discovery record. Never include transport or credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentModelDescriptor {
    /// Configured model identifier or resolved host model.
    pub model: String,
    /// Configured provider identifier.
    pub provider: String,
    /// Optional human-facing label.
    pub display_name: Option<String>,
    /// Reasoning selection or supported choices.
    pub reasoning: Vec<String>,
    /// Context capacity in tokens.
    pub context_window: u64,
    /// Output capacity in tokens.
    pub max_output_tokens: u64,
}
/// Host-only resolved transport and effective public selection.
pub struct ResolvedAgentModel {
    /// Configured model identifier or resolved host model.
    pub model: octet_ai::Model,
    /// Reasoning selection or supported choices.
    pub reasoning: octet_ai::ReasoningConfig,
    /// Canonical effective nonsecret selection.
    pub metadata: AgentModelSelection,
}
/// Product-owned configured catalog and reasoning policy. Errors must be secret-free.
pub trait AgentModelResolver: Send + Sync {
    /// Resolve against configured inventory and normalize with product reasoning policy.
    fn resolve(
        &self,
        selection: &AgentModelSelection,
        parent_model: &octet_ai::Model,
        parent_reasoning: &octet_ai::ReasoningConfig,
    ) -> Result<ResolvedAgentModel, String>;
    /// Return at most limit matches; the host requests one extra row for truncation.
    fn models(
        &self,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentModelDescriptor>, String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExtensionAgentSessionPolicy {
    #[serde(default)]
    pub(crate) model_selection: Option<AgentModelSelection>,
    #[serde(default)]
    pub(crate) resolved_model: Option<AgentModelSelection>,
    #[serde(default)]
    pub(crate) resolved_reasoning: Option<octet_ai::ReasoningConfig>,
    /// Child tool scope: a non-empty duplicate-free subset of the standard
    /// tools `read`, `search`, `edit`, `write`, and `bash`.
    pub(crate) tools: Vec<String>,
    /// Child depth relative to the root; extension children are exactly one.
    pub(crate) max_depth: usize,
    /// Maximum active children this owner may hold; host cap is eight.
    pub(crate) max_concurrent_children: usize,
    /// Child turn budget. `None` inherits the parent session limit exactly
    /// (unlimited parents stay unlimited).
    #[serde(default)]
    pub(crate) max_turns: Option<u64>,
    /// Optional child token ceiling. `None` inherits the parent session
    /// limit exactly (unlimited parents stay unlimited).
    #[serde(default)]
    pub(crate) max_tokens: Option<u64>,
    /// Optional hard whole-microdollar child cost ceiling. `None` imposes no
    /// child-specific ceiling; the parent session ceiling still applies.
    #[serde(default)]
    pub(crate) max_cost_microdollars: Option<u64>,
    /// Bounded worker result size in UTF-8 bytes.
    pub(crate) max_output_bytes: usize,
    /// Optional hard worker wall clock in milliseconds. `None` runs without a
    /// wall-clock kill; the worker can still be interrupted or stopped.
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
}

fn resolved_model_json(policy: Option<&ExtensionAgentSessionPolicy>) -> Value {
    match policy.and_then(|p| p.resolved_model.as_ref().map(|m| (p, m))) {
        Some((policy, model)) => {
            json!({"provider": model.provider, "model": model.model, "reasoning": policy.resolved_reasoning})
        }
        None => Value::Null,
    }
}

/// Project effective policy without exposing the internal recovery encoding.
fn public_policy_json(policy: Option<&ExtensionAgentSessionPolicy>) -> Value {
    let Some(policy) = policy else {
        return Value::Null;
    };
    let mut value = serde_json::to_value(policy).expect("policy is JSON serializable");
    let object = value.as_object_mut().expect("policy serializes as object");
    object.remove("resolved_reasoning");
    object.insert("resolved_model".into(), resolved_model_json(Some(policy)));
    value
}

fn child_orchestration_provenance(
    extension_policy: Option<&ExtensionAgentSessionPolicy>,
) -> DelegationOrchestrationProvenance {
    let mut provenance =
        DelegationOrchestrationProvenance::all(DelegationPolicySource::ParentInherited);
    if extension_policy.is_some() {
        // Extension-owned children receive a host-validated standard-tool
        // snapshot and explicit child-run limits. The sandbox, broker,
        // environment, cwd, and executable trust stay parent-owned.
        provenance.tool_scope = DelegationPolicySource::ChildOverride;
        provenance.execution_limits = DelegationPolicySource::ChildOverride;
    }
    provenance
}

impl ExtensionAgentSessionPolicy {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(selection) = &self.model_selection {
            if selection.provider != "inherit" && selection.model == "inherit" {
                return Err("unsupported_model: provider requires explicit model".into());
            }
            if [&selection.provider, &selection.model, &selection.reasoning]
                .iter()
                .any(|value| {
                    value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
                })
            {
                return Err("unsupported_model: invalid selection identifier".into());
            }
        }
        if self.tools.is_empty() || self.tools.len() > EXTENSION_CHILD_TOOLS.len() {
            return Err(
                "child tools must be a non-empty subset of the standard child tool names".into(),
            );
        }
        let tools = self.tools.iter().collect::<BTreeSet<_>>();
        if tools.len() != self.tools.len()
            || self
                .tools
                .iter()
                .any(|tool| !EXTENSION_CHILD_TOOLS.contains(&tool.as_str()))
        {
            return Err(
                "child tools must be a duplicate-free subset of read, search, edit, write, and bash"
                    .into(),
            );
        }
        if self.max_depth != 1 {
            return Err("extension child max_depth must be exactly 1".into());
        }
        if self.max_concurrent_children == 0
            || self.max_concurrent_children > MAX_EXTENSION_ACTIVE_CHILDREN
        {
            return Err(format!(
                "extension child concurrency must be between 1 and {MAX_EXTENSION_ACTIVE_CHILDREN}"
            ));
        }
        if self
            .max_turns
            .is_some_and(|max_turns| !(1..=MAX_EXTENSION_TURNS).contains(&max_turns))
        {
            return Err(format!(
                "extension child max_turns must be null or between 1 and {MAX_EXTENSION_TURNS}"
            ));
        }
        if self
            .max_tokens
            .is_some_and(|max_tokens| !(1_000..=64_000).contains(&max_tokens))
        {
            return Err("extension child max_tokens must be null or between 1000 and 64000".into());
        }
        if self
            .max_cost_microdollars
            .is_some_and(|max_cost| !(1..=MAX_EXTENSION_COST_MICRODOLLARS).contains(&max_cost))
        {
            return Err(format!(
                "extension child max_cost_microdollars must be null or between 1 and {}",
                MAX_EXTENSION_COST_MICRODOLLARS
            ));
        }
        if !(512..=16 * 1024).contains(&self.max_output_bytes) {
            return Err("extension child max_output_bytes must be between 512 and 16384".into());
        }
        if self
            .timeout_ms
            .is_some_and(|timeout| !(5_000..=MAX_EXTENSION_TIMEOUT_MS).contains(&timeout))
        {
            return Err(format!(
                "extension child timeout_ms must be null or between 5000 and {MAX_EXTENSION_TIMEOUT_MS}"
            ));
        }
        Ok(())
    }
}

pub(crate) struct ExtensionDelegationSpawnRequest {
    pub(crate) task_name: String,
    pub(crate) profile: Option<String>,
    pub(crate) fingerprint: Option<String>,
    pub(crate) message: String,
    pub(crate) idempotency_key: String,
    pub(crate) policy: ExtensionAgentSessionPolicy,
}

#[derive(Clone)]
pub(crate) struct ExtensionDelegationService {
    manager: Weak<DelegationManager>,
    principal: Arc<str>,
    parent_session_id: Arc<str>,
    state: Arc<Mutex<ExtensionDelegationState>>,
}

#[derive(Default)]
struct ExtensionDelegationState {
    owners: BTreeMap<String, ExtensionDelegationOwnerState>,
}

#[derive(Default)]
struct ExtensionDelegationOwnerState {
    owned_agents: BTreeSet<String>,
    idempotent_spawns: BTreeMap<String, IdempotentExtensionSpawn>,
}

struct IdempotentExtensionSpawn {
    task_name: String,
    profile: Option<String>,
    fingerprint: Option<String>,
    message_sha256: String,
    policy: ExtensionAgentSessionPolicy,
    result: Value,
}

/// A session-owned worker re-discovered by a durable extension spawn
/// idempotency key after the owning run (or process) ended.
struct ExtensionDurableSpawn {
    task_name: String,
    profile: Option<String>,
    fingerprint: Option<String>,
    policy: Option<ExtensionAgentSessionPolicy>,
    resource_owner: Option<String>,
    message_sha256: Option<String>,
    result: Value,
}

impl DelegationBinding {
    pub(crate) fn team_directory(&self) -> &Path {
        &self.manager.team_directory
    }

    pub(crate) fn open_session_reference(
        &self,
        extension_principal: &str,
        reference: &str,
    ) -> Result<Option<Session>, AgentError> {
        if !reference.starts_with("agent-session:") {
            return Ok(None);
        }
        let path = {
            let state = self
                .manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .records
                .values()
                .find(|record| {
                    record.extension_principal.as_deref() == Some(extension_principal)
                        && delegated_session_reference(&record.session_path).as_deref()
                            == Some(reference)
                })
                .map(|record| record.session_path.clone())
        };
        let Some(path) = path else {
            return Ok(None);
        };
        let file = secure_fs::open_private_file_for_read(&path)
            .map_err(|error| AgentError::Delegation(error.to_string()))?;
        Session::open_read_only_with_file(path, file)
            .map(Some)
            .map_err(AgentError::Session)
    }

    pub(crate) fn system_instructions(&self) -> &str {
        &self.system_instructions
    }

    pub(crate) fn is_root(&self) -> bool {
        self.identity.id == ROOT_AGENT_ID
    }

    /// Attach the owning root run to the manager's loss-tolerant latest
    /// telemetry stream. Child runs deliberately do not receive this stream.
    pub(crate) fn telemetry_receiver(
        &self,
    ) -> Option<watch::Receiver<Option<DelegationTelemetrySnapshot>>> {
        self.is_root().then(|| self.manager.attach_telemetry())
    }

    pub(crate) fn detach_telemetry(&self) {
        self.manager.detach_telemetry();
    }

    /// Count worker tasks that still own host state, independent of telemetry
    /// or extension presentation (including idle and unwinding workers).
    pub(crate) fn active_worker_count(&self) -> usize {
        self.manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .values()
            .filter(|record| record.live_task)
            .count()
    }

    pub(crate) fn request_shutdown(&self) {
        self.manager.request_shutdown_descendants(&self.identity.id);
    }

    /// Session-scoped detachment at the owning run boundary.
    ///
    /// Replaces run-scoped retirement: the fleet survives the turn, the durable
    /// roster is refreshed, and an explicit `run_detached` provenance record is
    /// written instead of a silent vanish.
    pub(crate) fn detach_run(&self) {
        self.manager.detach_run(&self.identity);
    }

    /// Session-owned launchable-handle resolver for the root owner.
    pub(crate) fn session_handle(&self) -> SessionDelegationHandle {
        SessionDelegationHandle {
            manager: Arc::clone(&self.manager),
        }
    }

    pub(crate) fn delegated_usage_records(&self) -> Vec<DelegatedUsageRecord> {
        self.manager.extension_usage_records(&self.identity.id)
    }

    pub(crate) fn prepare_owning_run(&self) -> Result<(), AgentError> {
        self.manager
            .prepare_owning_run(&self.identity)
            .map_err(AgentError::Delegation)
    }

    pub(crate) fn set_model_resolver(&self, resolver: Arc<dyn AgentModelResolver>) {
        *self
            .manager
            .template
            .model_resolver
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(resolver);
    }

    pub(crate) fn update_base_system(&self, system: String) {
        if self.identity.id == ROOT_AGENT_ID {
            *self
                .manager
                .template
                .base_system
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = system;
        }
    }
    pub(crate) fn update_reasoning(&self, reasoning: octet_ai::ReasoningConfig) {
        if self.identity.id == ROOT_AGENT_ID {
            *self
                .manager
                .template
                .reasoning
                .write()
                .unwrap_or_else(|p| p.into_inner()) = reasoning;
        }
    }

    pub(crate) fn update_runtime_settings(&self, settings: DelegationRuntimeSettings) {
        if self.identity.id == ROOT_AGENT_ID {
            *self
                .manager
                .template
                .runtime
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
        }
    }

    pub(crate) fn extension_service(
        &self,
        principal: impl Into<String>,
        parent_session_id: impl Into<String>,
        root_resource_owner: impl Into<String>,
    ) -> Result<ExtensionDelegationService, String> {
        if self.identity.id != ROOT_AGENT_ID {
            return Err("extension delegation service requires the root binding".into());
        }
        let principal = principal.into();
        if principal.trim().is_empty() || principal.len() > 256 {
            return Err("extension delegation principal must be 1..=256 bytes".into());
        }
        let parent_session_id = parent_session_id.into();
        if parent_session_id.trim().is_empty()
            || parent_session_id.len() > 256
            || parent_session_id.chars().any(char::is_whitespace)
        {
            return Err(
                "extension delegation parent session must be a bounded stable identifier".into(),
            );
        }
        let root_resource_owner = root_resource_owner.into();
        ExtensionDelegationService::validate_resource_owner(&root_resource_owner)?;
        let mut service_state = ExtensionDelegationState::default();
        {
            let mut state = self
                .manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.root_resource_owner = Some(root_resource_owner.clone());
            // Durable records, not a replay of spawn requests, reconstruct the
            // inspection/control index. Both the extension principal and the
            // parent resource-owner fence must match the restored tree.
            for record in state.records.values() {
                if record.extension_principal.as_deref() != Some(principal.as_str()) {
                    continue;
                }
                let Some(owner) = record.extension_resource_owner.as_deref() else {
                    continue;
                };
                if ExtensionDelegationService::validate_resource_owner(owner).is_err() {
                    continue;
                }
                let parent_matches = if owner == root_resource_owner {
                    record.parent_id == ROOT_AGENT_ID
                } else {
                    state
                        .records
                        .get(&record.parent_id)
                        .is_some_and(|parent| parent.resource_owner.as_deref() == Some(owner))
                };
                if parent_matches {
                    service_state
                        .owners
                        .entry(owner.to_owned())
                        .or_default()
                        .owned_agents
                        .insert(record.identity.id.clone());
                }
            }
        }
        Ok(ExtensionDelegationService {
            manager: Arc::downgrade(&self.manager),
            principal: Arc::from(principal),
            parent_session_id: Arc::from(parent_session_id),
            state: Arc::new(Mutex::new(service_state)),
        })
    }
}

impl ExtensionDelegationService {
    fn manager(&self) -> Result<Arc<DelegationManager>, String> {
        self.manager
            .upgrade()
            .ok_or_else(|| "delegation service is no longer available".to_owned())
    }

    fn root_identity() -> AgentIdentity {
        AgentIdentity {
            id: ROOT_AGENT_ID.into(),
            path: ROOT_AGENT_PATH.into(),
            depth: 0,
        }
    }

    fn owner_identity(
        &self,
        manager: &DelegationManager,
        resource_owner: &str,
    ) -> Result<AgentIdentity, String> {
        Self::validate_resource_owner(resource_owner)?;
        let state = manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.root_resource_owner.as_deref() == Some(resource_owner) {
            return Ok(Self::root_identity());
        }
        state
            .records
            .values()
            .find(|record| record.resource_owner.as_deref() == Some(resource_owner))
            .map(|record| record.identity.clone())
            .ok_or_else(|| "extension resource owner is not an active model session".to_owned())
    }

    fn resolve_owned_target(
        &self,
        manager: &DelegationManager,
        resource_owner: &str,
        target: &str,
    ) -> Result<String, String> {
        let owned = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .get(resource_owner)
            .map(|owner| owner.owned_agents.clone())
            .unwrap_or_default();
        if owned.is_empty() {
            return Err("extension resource owner has no child sessions".into());
        }
        let state = manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owned_paths = owned
            .iter()
            .filter_map(|id| {
                state
                    .records
                    .get(id)
                    .map(|record| record.identity.path.clone())
            })
            .collect::<Vec<_>>();
        let target_id = DelegationManager::resolve_id_locked(&state, target)
            .ok_or_else(|| format!("unknown extension delegation target: {target}"))?;
        let target_path = state
            .records
            .get(&target_id)
            .map(|record| record.identity.path.as_str())
            .ok_or_else(|| format!("unknown extension delegation target: {target}"))?;
        if !owned.contains(&target_id)
            && !owned_paths
                .iter()
                .any(|root| is_descendant_path(target_path, root))
        {
            return Err("extension principal may access only its own child-session trees".into());
        }
        Ok(target_id)
    }

    fn owner_task_prefix(&self, resource_owner: &str) -> String {
        extension_owner_task_prefix(&self.principal, resource_owner)
    }

    fn validate_resource_owner(resource_owner: &str) -> Result<(), String> {
        if resource_owner.trim().is_empty() || resource_owner.len() > 512 {
            return Err("extension resource owner must be 1..=512 bytes".into());
        }
        Ok(())
    }

    pub(crate) fn shutdown_owned(&self) {
        let roots = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .values()
            .flat_map(|owner| owner.owned_agents.iter().cloned())
            .collect::<BTreeSet<_>>();
        if let Some(manager) = self.manager.upgrade() {
            manager.request_shutdown_agent_trees(&roots);
        }
    }

    pub(crate) fn models(
        &self,
        resource_owner: &str,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Value, String> {
        if !(1..=100).contains(&limit)
            || query.is_some_and(|q| q.len() > 128 || q.chars().any(char::is_control))
        {
            return Err("invalid model discovery bounds".into());
        }
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        if owner.depth != 0 {
            return Err("model discovery is root-owner only".into());
        }
        let resolver = manager
            .template
            .model_resolver
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let mut models = if let Some(resolver) = resolver.as_ref() {
            resolver.models(query, limit + 1)?
        } else {
            let resolved = manager.template.resolve_model(None)?;
            let m = &resolved.model.spec;
            vec![AgentModelDescriptor {
                model: resolved.metadata.model,
                provider: resolved.metadata.provider,
                display_name: m.display_name.clone(),
                reasoning: vec![resolved.metadata.reasoning],
                context_window: m.limits.context_window,
                max_output_tokens: m.limits.max_output_tokens,
            }]
        };
        if models.len() > limit + 1
            || models.iter().any(|m| {
                [&m.model, &m.provider]
                    .iter()
                    .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
                    || m.display_name
                        .as_ref()
                        .is_some_and(|name| name.len() > 512 || name.chars().any(char::is_control))
                    || m.reasoning.len() > 32
                    || m.reasoning
                        .iter()
                        .any(|r| r.is_empty() || r.len() > 256 || r.chars().any(char::is_control))
            })
        {
            return Err("configured model discovery exceeded public metadata bounds".into());
        }
        if let Some(query) = query {
            let query = query.to_lowercase();
            models.retain(|m| {
                format!(
                    "{} {} {}",
                    m.model,
                    m.provider,
                    m.display_name.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(&query)
            });
        }
        let truncated = models.len() > limit;
        models.truncate(limit);
        Ok(json!({"models": models, "truncated": truncated}))
    }

    pub(crate) fn spawn(
        &self,
        resource_owner: &str,
        request: ExtensionDelegationSpawnRequest,
    ) -> Result<Value, String> {
        let ExtensionDelegationSpawnRequest {
            task_name,
            profile,
            fingerprint,
            message,
            idempotency_key,
            policy,
        } = request;
        Self::validate_resource_owner(resource_owner)?;
        validate_task_name(&task_name)?;
        if let Some(profile) = profile.as_deref() {
            validate_task_name(profile)
                .map_err(|_| "profile must be a bounded lowercase stable identifier".to_owned())?;
        }
        if fingerprint.as_deref().is_some_and(|fingerprint| {
            fingerprint.len() != 64
                || !fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err("fingerprint must be a lowercase SHA-256 digest".into());
        }
        policy.validate()?;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err("spawn idempotency_key must be 1..=256 bytes".into());
        }
        let message_sha256 = format!("{:x}", Sha256::digest(message.as_bytes()));
        let manager = self.manager()?;
        let reject_spawn = |error: String| {
            manager.publish_external_failure("spawn_rejected", &error);
            error
        };
        let mut service_state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owner_state = service_state
            .owners
            .entry(resource_owner.to_owned())
            .or_default();
        {
            let manager_state = manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            owner_state
                .owned_agents
                .retain(|id| manager_state.records.contains_key(id));
            owner_state.idempotent_spawns.retain(|_, spawn| {
                spawn
                    .result
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| manager_state.records.contains_key(id))
            });
        }
        if let Some(existing) = owner_state.idempotent_spawns.get(&idempotency_key) {
            if existing.task_name != task_name
                || existing.profile != profile
                || existing.fingerprint != fingerprint
                || existing.message_sha256 != message_sha256
                || existing.policy != policy
            {
                return Err(reject_spawn(
                    "spawn idempotency_key was reused with different input".into(),
                ));
            }
            return Ok(existing.result.clone());
        }
        // Durable idempotency: the worker survived the owning run (or a
        // restart) as a session-owned record. Re-issue its original result
        // instead of spawning a duplicate worker, and re-arm the fast path.
        if let Some(durable) =
            manager.extension_owned_record(&self.principal, resource_owner, &idempotency_key)
        {
            if durable.task_name != task_name
                || durable.profile != profile
                || durable.fingerprint != fingerprint
                || durable.policy.as_ref() != Some(&policy)
                || durable.resource_owner.as_deref() != Some(resource_owner)
                || durable.message_sha256.as_deref() != Some(message_sha256.as_str())
            {
                return Err(reject_spawn(
                    "spawn idempotency_key was reused with different input".into(),
                ));
            }
            let mut result = durable.result;
            result["task_name"] = Value::String(task_name.clone());
            result["principal"] = Value::String(self.principal.to_string());
            result["resource_owner"] = Value::String(resource_owner.to_owned());
            if let Some(agent_id) = result.get("agent_id").and_then(Value::as_str) {
                owner_state.owned_agents.insert(agent_id.to_owned());
            }
            owner_state.idempotent_spawns.insert(
                idempotency_key,
                IdempotentExtensionSpawn {
                    task_name,
                    profile,
                    fingerprint,
                    message_sha256,
                    policy,
                    result: result.clone(),
                },
            );
            return Ok(result);
        }
        let internal_digest = Sha256::digest(format!("{task_name}\0{idempotency_key}").as_bytes());
        let internal_task_name = format!(
            "{}-task-{}",
            self.owner_task_prefix(resource_owner),
            internal_digest[..6]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let owner = self.owner_identity(&manager, resource_owner)?;
        let rejection = {
            let manager_state = manager
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let active_count = owner_state
                .owned_agents
                .iter()
                .filter_map(|id| manager_state.records.get(id))
                .filter(|record| record.status.is_running())
                .count();
            if active_count >= policy.max_concurrent_children {
                Some(format!(
                    "extension child concurrency limit reached ({})",
                    policy.max_concurrent_children
                ))
            } else if owner_state.owned_agents.len() >= MAX_EXTENSION_OWNED_CHILDREN {
                Some(format!(
                    "extension child total limit reached ({MAX_EXTENSION_OWNED_CHILDREN})"
                ))
            } else {
                None
            }
        };
        if let Some(error) = rejection {
            return Err(reject_spawn(error));
        }
        let mut result = match manager.spawn(
            &owner,
            SpawnRequest {
                task_name: internal_task_name,
                display_task_name: Some(task_name.clone()),
                message,
                extension_policy: Some(policy.clone()),
                extension_provenance: Some(ExtensionSpawnProvenance {
                    parent_session_id: self.parent_session_id.to_string(),
                    principal: self.principal.to_string(),
                    resource_owner: resource_owner.to_owned(),
                    profile: profile.clone(),
                    idempotency_key: idempotency_key.clone(),
                    fingerprint: fingerprint.clone(),
                }),
            },
        ) {
            Ok(result) => result,
            Err(error) => {
                manager.publish_external_failure("spawn_rejected", &error);
                return Err(error);
            }
        };
        let agent_id = result
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "delegation spawn omitted agent_id".to_owned())?;
        let agent_id = agent_id.to_owned();
        result["task_name"] = Value::String(task_name.clone());
        result["principal"] = Value::String(self.principal.to_string());
        result["resource_owner"] = Value::String(resource_owner.to_owned());
        owner_state.owned_agents.insert(agent_id);
        owner_state.idempotent_spawns.insert(
            idempotency_key,
            IdempotentExtensionSpawn {
                task_name,
                profile,
                fingerprint,
                message_sha256,
                policy,
                result: result.clone(),
            },
        );
        Ok(result)
    }

    pub(crate) async fn send_message(
        &self,
        resource_owner: &str,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager.send_message(&owner, &target, message).await
    }

    pub(crate) async fn follow_up(
        &self,
        resource_owner: &str,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager
            .follow_up(&owner, FollowUpRequest { target, message })
            .await
    }

    pub(crate) async fn interrupt(
        &self,
        resource_owner: &str,
        target: &str,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let owner = self.owner_identity(&manager, resource_owner)?;
        let target = self.resolve_owned_target(&manager, resource_owner, target)?;
        manager.interrupt(&owner, &target).await
    }

    pub(crate) fn list(&self, resource_owner: &str) -> Result<Value, String> {
        Self::validate_resource_owner(resource_owner)?;
        let manager = self.manager()?;
        self.owner_identity(&manager, resource_owner)?;
        let owned = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owners
            .get(resource_owner)
            .map(|owner| owner.owned_agents.clone())
            .unwrap_or_default();
        let state = manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owned_paths = owned
            .iter()
            .filter_map(|id| {
                state
                    .records
                    .get(id)
                    .map(|record| record.identity.path.clone())
            })
            .collect::<Vec<_>>();
        let agents = state
            .records
            .values()
            .filter(|record| {
                owned.contains(&record.identity.id)
                    || owned_paths
                        .iter()
                        .any(|root| is_descendant_path(&record.identity.path, root))
            })
            .map(|record| {
                let mut value = agent_record_value(record);
                value["session"] = delegated_session_reference(&record.session_path)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                value["provenance"] = json!({
                    "kind": "extension_agent_session",
                    "principal": self.principal.as_ref(),
                    "resource_owner": resource_owner,
                });
                value
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "principal": self.principal.as_ref(),
            "resource_owner": resource_owner,
            "agents": agents,
            "persistence_error": state.persistence_error,
        }))
    }

    pub(crate) async fn wait(
        &self,
        resource_owner: &str,
        timeout: Duration,
        cancellation: &crate::CancellationToken,
    ) -> Result<Value, String> {
        let manager = self.manager()?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = manager.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let snapshot = self.list(resource_owner)?;
            let any_running = snapshot["agents"].as_array().is_some_and(|agents| {
                agents.iter().any(|agent| {
                    matches!(
                        agent["status"]["state"].as_str(),
                        Some("pending" | "running")
                    )
                })
            });
            if !any_running {
                return Ok(json!({"timed_out": false, "snapshot": snapshot}));
            }
            tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err("extension delegation wait cancelled".into())
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(json!({"timed_out": true, "snapshot": self.list(resource_owner)?}))
                }
                _ = &mut changed => {}
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AgentIdentity {
    id: String,
    path: String,
    depth: usize,
}

#[derive(Clone)]
pub(crate) struct DelegationRuntimeSettings {
    pub(crate) compaction_model: Option<octet_ai::Model>,
    pub(crate) auto_compaction_mode: AgentCompactionMode,
    pub(crate) auto_compaction_threshold: f64,
    pub(crate) compaction_keep_recent_tokens: u64,
    pub(crate) completion_policy: CompletionPolicy,
    pub(crate) output_modalities: octet_ai::OutputModalities,
    pub(crate) max_output_tokens: u64,
    pub(crate) tool_schema_budget_bytes: usize,
    pub(crate) max_session_tokens: Option<u64>,
    pub(crate) max_session_cost_microdollars: Option<u64>,
    pub(crate) provider_retries_enabled: bool,
    pub(crate) max_network_wait: Option<Duration>,
}

pub(crate) struct DelegationTemplate {
    pub(crate) model_resolver: RwLock<Option<Arc<dyn AgentModelResolver>>>,
    pub(crate) client: octet_ai::AiClient,
    pub(crate) model: octet_ai::Model,
    pub(crate) base_system: RwLock<String>,
    pub(crate) sandbox: crate::SandboxConfig,
    pub(crate) effect_broker: crate::EffectBroker,
    pub(crate) extensions: ExtensionHost,
    pub(crate) max_turns: Option<u64>,
    pub(crate) reasoning: RwLock<octet_ai::ReasoningConfig>,
    pub(crate) reasoning_mode: octet_ai::ReasoningMode,
    pub(crate) cache_retention: octet_ai::CacheRetention,
    pub(crate) runtime: RwLock<DelegationRuntimeSettings>,
}

fn lower_child_reasoning(mut resolved: ResolvedAgentModel) -> ResolvedAgentModel {
    // Astra's host-side Ultra tier enables V2 collaboration, but the
    // observed child-run wire contract is xhigh. Keep root and generic
    // Ultra lowering unchanged by translating only this child boundary.
    let reasoning = if resolved.model.spec.api_name == "gpt-6-astra"
        && resolved.model.spec.capabilities.agent_delegation == Some(octet_ai::AgentDelegation::V2)
        && resolved
            .model
            .spec
            .capabilities
            .reasoning
            .as_ref()
            .is_some_and(|reasoning| reasoning.max_effort == octet_ai::ReasoningEffort::Ultra)
        && resolved.reasoning == octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Ultra)
    {
        octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Xhigh)
    } else {
        resolved.reasoning.clone()
    };
    if reasoning != resolved.reasoning {
        resolved.reasoning = reasoning;
        resolved.metadata.reasoning = "xhigh".into();
    }
    resolved
}

impl DelegationTemplate {
    fn resolve_model(
        &self,
        policy: Option<&ExtensionAgentSessionPolicy>,
    ) -> Result<ResolvedAgentModel, String> {
        // Existing workers keep their host-pinned effort across parent changes.
        // Only a new inherited worker reads the parent's current selection.
        let reasoning = policy
            .and_then(|p| p.resolved_reasoning.clone())
            .unwrap_or_else(|| {
                self.reasoning
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            });
        let reasoning_label = match &reasoning {
            octet_ai::ReasoningConfig::Off => "off".into(),
            octet_ai::ReasoningConfig::On => "on".into(),
            octet_ai::ReasoningConfig::Effort(effort) => format!("{effort:?}").to_lowercase(),
            octet_ai::ReasoningConfig::Budget(n) => format!("budget={n}"),
        };
        if policy
            .and_then(|p| p.resolved_model.as_ref())
            .is_some_and(|pinned| pinned.reasoning != reasoning_label)
        {
            return Err("unsupported_reasoning: saved worker reasoning changed".into());
        }
        let mut selection = policy
            .and_then(|p| p.resolved_model.as_ref().or(p.model_selection.as_ref()))
            .cloned()
            .unwrap_or_default();
        // A newly inherited worker must retain the parent's exact binding, not
        // re-select updated catalog metadata merely because admission pinned IDs.
        if policy.is_some_and(|p| {
            p.model_selection
                .as_ref()
                .is_none_or(|s| s == &AgentModelSelection::default())
                && p.resolved_model
                    .as_ref()
                    .is_some_and(|m| m.model == self.model.spec.id.0)
                && p.resolved_reasoning.as_ref() == Some(&reasoning)
        }) {
            selection = AgentModelSelection::default();
        }
        if let Some(resolver) = self
            .model_resolver
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let resolved =
                lower_child_reasoning(resolver.resolve(&selection, &self.model, &reasoning)?);
            if let Some(policy) = policy {
                if policy
                    .resolved_model
                    .as_ref()
                    .is_some_and(|pinned| pinned != &resolved.metadata)
                    || policy
                        .resolved_reasoning
                        .as_ref()
                        .is_some_and(|pinned| pinned != &resolved.reasoning)
                {
                    return Err(
                        "unsupported_model: saved worker selection no longer resolves exactly"
                            .into(),
                    );
                }
            }
            return Ok(resolved);
        }
        let metadata = AgentModelSelection {
            provider: self.model.spec.endpoint.0.clone(),
            model: self.model.spec.id.0.clone(),
            reasoning: reasoning_label,
        };
        let resolved = lower_child_reasoning(ResolvedAgentModel {
            model: self.model.clone(),
            reasoning: reasoning.clone(),
            metadata,
        });
        if (selection.provider != "inherit" && selection.provider != resolved.metadata.provider)
            || (selection.model != "inherit" && selection.model != resolved.metadata.model)
        {
            return Err("unsupported_model: no configured model resolver".into());
        }
        if selection.reasoning != "inherit" && selection.reasoning != resolved.metadata.reasoning {
            return Err("unsupported_reasoning: no configured reasoning resolver".into());
        }
        if policy
            .and_then(|p| p.resolved_reasoning.as_ref())
            .is_some_and(|pinned| pinned != &resolved.reasoning)
        {
            return Err("unsupported_reasoning: saved worker reasoning changed".into());
        }
        Ok(resolved)
    }
}

pub(crate) struct DelegationManager {
    config: DelegationConfig,
    root_tools: bool,
    team_directory: PathBuf,
    team_storage: Option<Arc<secure_fs::PrivateDirectory>>,
    journal: ProvenanceJournal,
    template: DelegationTemplate,
    state: Mutex<ManagerState>,
    /// Serializes each provenance-journaled operation's
    /// decide → append → commit window so that journal record order always
    /// matches state mutation order, while the state lock itself is released
    /// across the journal's durable `sync_data`. Lock order:
    /// permits (RwLock) → journal_order → state.
    journal_order: Mutex<()>,
    permits: RwLock<Arc<Semaphore>>,
    changed: Notify,
    /// Lock order when both are needed: `state` → `telemetry`.
    telemetry: Mutex<DelegationTelemetryState>,
    /// Explicit span observer owned by the root agent, copied in when
    /// delegation is enabled. Inert unless the host installed one; it never
    /// participates in worker accounting, admission or budgeting.
    span_context: RwLock<TelemetryContext>,
    /// Session-scoped durable fleet roster. Without one, mutating admission
    /// fails closed. When present it is the authoritative record that lets a
    /// later turn or a restarted process reconstruct workers that outlived
    /// the run that spawned them.
    roster_path: Option<PathBuf>,
    /// Root session path recorded in the roster so a foreign roster file is
    /// rejected instead of being trusted.
    root_session: PathBuf,
    /// Durable fleet claim held by this manager. `None` means the lease could
    /// not be proven, so restored workers must not be started and the roster
    /// must not be overwritten.
    lease: RwLock<Option<FleetLease>>,
    /// Bounded reason the lease is not held, surfaced with every refusal.
    lease_refusal: RwLock<Option<String>>,
}

#[derive(Default)]
struct DelegationTelemetryState {
    revision: u64,
    // A slow frontend needs only the newest roster. `watch` coalesces updates
    // instead of retaining an unbounded queue of increasingly large snapshots.
    sender: Option<watch::Sender<Option<DelegationTelemetrySnapshot>>>,
}

struct ManagerState {
    next_agent_number: u64,
    next_mailbox_delivery: u64,
    total_agents: usize,
    active_waiters: usize,
    records: BTreeMap<String, AgentRecord>,
    root_mailbox: VecDeque<MailboxMessage>,
    root_mailbox_delivery: Option<MailboxDeliveryPlan>,
    persistence_error: Option<String>,
    shutting_down: bool,
    /// Set when the owning session released its root agent while workers were
    /// live. Workers that observe their shutdown token while this is set park
    /// as recoverable [`DelegatedAgentStatus::Detached`] records instead of
    /// retiring, so the next session owner can reattach and continue them.
    session_owner_released: bool,
    root_active: bool,
    root_resource_owner: Option<String>,
}

impl Default for ManagerState {
    /// The state of a freshly assembled manager.
    ///
    /// This is the only place the manager's starting state is expressed, so a
    /// new field cannot be missing from one of the construction sites.
    fn default() -> Self {
        Self {
            // The root owns the first of the bounded agent slots, and the first
            // child takes `agent-1`.
            next_agent_number: 1,
            next_mailbox_delivery: 1,
            total_agents: 1,
            active_waiters: 0,
            records: BTreeMap::new(),
            root_mailbox: VecDeque::new(),
            root_mailbox_delivery: None,
            persistence_error: None,
            shutting_down: false,
            session_owner_released: false,
            root_active: true,
            root_resource_owner: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DelegatedUsageRecord {
    pub(crate) agent_id: String,
    pub(crate) usage: Usage,
    pub(crate) usage_uncertain: bool,
    pub(crate) usage_exposure: Option<UsageUncertaintyBound>,
    pub(crate) cost: Option<Cost>,
    pub(crate) turn_count: u64,
    pub(crate) tool_call_count: u64,
}

/// Bounded host-observed record of one child tool call, retained for
/// owner-scoped inspection in `agent/list`. Arguments are reduced to a
/// bounded single-line summary; results are never captured here because
/// completed tool results are already persisted in the child session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ChildToolActivity {
    /// Tool name.
    name: String,
    /// Bounded single-line argument summary (`key=value` pairs).
    args_summary: String,
    /// Host capture time of the tool start in Unix milliseconds.
    started_at_ms: u64,
    /// Host capture time of the tool finish, when observed.
    finished_at_ms: Option<u64>,
    /// Whether the call finished as an error result.
    error: bool,
    /// Provider-assigned call ID used to match start/finish; never serialized.
    #[serde(skip)]
    call_id: String,
}

struct AgentRecord {
    identity: AgentIdentity,
    task_name: String,
    display_task_name: Option<String>,
    parent_id: String,
    session_path: PathBuf,
    status: DelegatedAgentStatus,
    command_tx: mpsc::Sender<WorkerCommand>,
    shutdown: crate::CancellationToken,
    interrupt_requested: bool,
    /// Accepted messages stay in the durable queue while a process-local
    /// channel or prompt holds a delivery attempt.
    pending_messages: VecDeque<DirectedMessage>,
    inflight_message_ids: BTreeSet<String>,
    reserved_messages: QueueUsage,
    queued_follow_ups: QueueUsage,
    /// Accepted follow-ups remain here until the child session confirms their
    /// durable delivery. The channel is only a process-local wakeup path.
    pending_follow_ups: VecDeque<QueuedFollowUp>,
    pending_initial_task: Option<QueuedInitialTask>,
    mailbox: VecDeque<MailboxMessage>,
    mailbox_delivery: Option<MailboxDeliveryPlan>,
    resource_owner: Option<String>,
    extension_policy: Option<ExtensionAgentSessionPolicy>,
    effective_tool_policy: EffectiveToolPolicy,
    orchestration_provenance: DelegationOrchestrationProvenance,
    extension_principal: Option<String>,
    extension_profile: Option<String>,
    extension_idempotency_key: Option<String>,
    extension_resource_owner: Option<String>,
    extension_message_sha256: Option<String>,
    extension_requested_policy: Option<ExtensionAgentSessionPolicy>,
    extension_fingerprint: Option<String>,
    created_at_ms: u64,
    started_at_ms: Option<u64>,
    completed_at_ms: Option<u64>,
    turn_count: u64,
    tool_call_count: u64,
    active_tools: BTreeMap<String, String>,
    recent_tools: VecDeque<ChildToolActivity>,
    usage: Usage,
    /// Process-local provisional generation; never part of durable/billable usage.
    streamed_output_bytes: u64,
    usage_uncertain: bool,
    usage_exposure: Option<UsageUncertaintyBound>,
    cost: Option<Cost>,
    cost_microdollars: Option<u64>,
    deadline_at_ms: Option<u64>,
    /// Effective per-run turn ceiling retained for terminal evidence.
    turn_limit: Option<u64>,
    /// Session-scoped lifetime marker: `true` once the owning run ended and
    /// the worker left the run that spawned it. Detached workers keep running
    /// while they need no new authority, and park in
    /// [`DelegatedAgentStatus::AwaitingApproval`] when they do.
    detached: bool,
    /// Process-local liveness of this record's worker task. `true` while a task
    /// in this process owns the child session; cleared by every exit path of
    /// `run_worker`. It is deliberately not durable: after a restart no task is
    /// live, which is exactly what a launchable handle needs to know.
    live_task: bool,
    /// Process-local incarnation; a fleet claim may be reused by many starts.
    worker_generation: u64,
    /// Command receiver parked for a detached record restored from the
    /// durable roster. Keeping it alive buffers a later turn's steering or
    /// follow-up until reattachment; it is taken exactly once.
    detached_commands: Option<mpsc::Receiver<WorkerCommand>>,
    /// Bounded diagnostic retained when a durable record could not be
    /// reattached (unknown state), so it fails closed visibly.
    durable_diagnostic: Option<String>,
    /// Durable claim under which this worker may execute. It is stamped when a
    /// worker is spawned or reattached and checked before any restored worker
    /// starts, so a stale or duplicate session owner cannot run it twice.
    claim: Option<DurableFleetClaim>,
}

/// Bounded durable snapshot of one session-owned worker.
///
/// Persisted beside the delegation directory so the owning session can
/// reconstruct its fleet after the owning run ends and after a process
/// restart. It never carries process-local handles: on load a record without a
/// live task surfaces as an explicit [`DelegatedAgentStatus::Detached`]
/// diagnostic instead of a silently-forgotten worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableFleetRecord {
    agent_id: String,
    agent_path: String,
    parent_id: String,
    depth: usize,
    task_name: String,
    display_task_name: Option<String>,
    session_path: PathBuf,
    status: DelegatedAgentStatus,
    detached: bool,
    created_at_ms: u64,
    started_at_ms: Option<u64>,
    completed_at_ms: Option<u64>,
    turn_count: u64,
    tool_call_count: u64,
    usage: Usage,
    usage_uncertain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage_exposure: Option<UsageUncertaintyBound>,
    cost: Option<Cost>,
    cost_microdollars: Option<u64>,
    deadline_at_ms: Option<u64>,
    turn_limit: Option<u64>,
    extension_principal: Option<String>,
    extension_profile: Option<String>,
    extension_idempotency_key: Option<String>,
    #[serde(default)]
    extension_resource_owner: Option<String>,
    #[serde(default)]
    extension_message_sha256: Option<String>,
    #[serde(default)]
    extension_requested_policy: Option<ExtensionAgentSessionPolicy>,
    extension_fingerprint: Option<String>,
    extension_policy: Option<ExtensionAgentSessionPolicy>,
    #[serde(default)]
    resource_owner: Option<String>,
    #[serde(default)]
    durable_diagnostic: Option<String>,
    #[serde(default)]
    claim: Option<DurableFleetClaim>,
    #[serde(default)]
    pending_messages: VecDeque<DirectedMessage>,
    #[serde(default)]
    queued_follow_ups: VecDeque<QueuedFollowUp>,
    #[serde(default)]
    pending_initial_task: Option<QueuedInitialTask>,
    #[serde(default)]
    mailbox: VecDeque<DurableMailboxMessage>,
    #[serde(default)]
    mailbox_delivery: Option<MailboxDeliveryPlan>,
}

impl Default for DurableFleetRecord {
    /// Base for fixtures: an unattached worker with no durable history.
    ///
    /// Fixtures spread this value (`..DurableFleetRecord::default()`) and state
    /// only the fields they mean, so a new durable field is added in exactly one
    /// place and cannot be silently missing from a fixture.
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            agent_path: String::new(),
            parent_id: String::new(),
            depth: 0,
            task_name: String::new(),
            display_task_name: None,
            session_path: PathBuf::new(),
            status: DelegatedAgentStatus::Pending,
            detached: false,
            created_at_ms: 0,
            started_at_ms: None,
            completed_at_ms: None,
            turn_count: 0,
            tool_call_count: 0,
            usage: Usage::default(),
            usage_uncertain: false,
            usage_exposure: None,
            cost: None,
            cost_microdollars: None,
            deadline_at_ms: None,
            turn_limit: None,
            extension_principal: None,
            extension_profile: None,
            extension_idempotency_key: None,
            extension_resource_owner: None,
            extension_message_sha256: None,
            extension_requested_policy: None,
            extension_fingerprint: None,
            extension_policy: None,
            resource_owner: None,
            durable_diagnostic: None,
            claim: None,
            pending_messages: VecDeque::new(),
            queued_follow_ups: VecDeque::new(),
            pending_initial_task: None,
            mailbox: VecDeque::new(),
            mailbox_delivery: None,
        }
    }
}

/// Versioned durable fleet file written to the session-scoped delegation
/// directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableFleet {
    version: u32,
    root_session: PathBuf,
    records: Vec<DurableFleetRecord>,
    #[serde(default = "default_mailbox_delivery_id")]
    next_mailbox_delivery: u64,
    #[serde(default)]
    root_mailbox: VecDeque<DurableMailboxMessage>,
    #[serde(default)]
    root_mailbox_delivery: Option<MailboxDeliveryPlan>,
}

fn default_mailbox_delivery_id() -> u64 {
    1
}

/// Fits every admitted fleet (at most 256 agents), including JSON's worst-case
/// six-byte escaping of both bounded text fields (status and diagnostic), plus
/// bounded identities, paths and policy metadata. Writers and readers share
/// this aggregate bound; admitted terminal summaries must not poison a fleet.
const MAX_FLEET_ROSTER_BYTES: usize =
    256 * (12 * MAX_PROVENANCE_TEXT_BYTES + 64 * 1024) + 64 * 1024;
const ROSTER_PROJECTION_BYTES: usize = 256 * 1024;

/// The roster is an index into the durable child sessions, not another copy
/// of every answer. Preserve full live/provenance output; only its restart
/// projection shares the remaining roster byte budget. Both native and extension
/// workers accumulate Completed/LimitReached text solely from TurnFinished, which
/// the agent emits only after append_assistant_turn_with_metadata succeeds.
/// Failure and approval diagnostics are metadata, never output-budget candidates.
fn encode_durable_fleet(mut fleet: DurableFleet) -> io::Result<Vec<u8>> {
    let mut texts = Vec::new();
    for (index, record) in fleet.records.iter_mut().enumerate() {
        if let Some(text) = roster_status_text(&mut record.status) {
            if !text.is_empty() {
                texts.push((index, std::mem::take(text)));
            }
        }
    }
    let base_bytes = serde_json::to_vec(&fleet).map_err(io::Error::other)?.len();
    let available = ROSTER_PROJECTION_BYTES
        .saturating_sub(base_bytes)
        .max(texts.len() * json_text_bytes(ROSTER_OUTPUT_SUFFIX));
    let requested: usize = texts.iter().map(|(_, text)| json_text_bytes(text)).sum();
    let budget = available / texts.len().max(1);
    // Metadata still fails closed. Never turn a status into an unmarked empty
    // answer just to make an overfull roster fit.
    if base_bytes.saturating_add(available) > MAX_FLEET_ROSTER_BYTES
        || (requested > available && budget < json_text_bytes(ROSTER_OUTPUT_SUFFIX))
    {
        return Err(io::Error::other(
            "durable fleet metadata exceeded its bounded size",
        ));
    }
    for (index, text) in texts {
        *roster_status_text(&mut fleet.records[index].status).expect("saved status text") =
            if requested <= available {
                text
            } else {
                bound_roster_text(&text, budget)
            };
    }
    let encoded = serde_json::to_vec(&fleet).map_err(io::Error::other)?;
    debug_assert!(encoded.len() <= MAX_FLEET_ROSTER_BYTES);
    Ok(encoded)
}

const ROSTER_OUTPUT_SUFFIX: &str = "\n...[truncated in fleet roster; inspect child session]";

fn roster_status_text(status: &mut DelegatedAgentStatus) -> Option<&mut String> {
    match status {
        DelegatedAgentStatus::Completed { output }
        | DelegatedAgentStatus::LimitReached { output, .. } => Some(output),
        _ => None,
    }
}

// serde_json's escaped string content size (excluding the two quotes). A raw
// UTF-8 budget alone is insufficient: control bytes expand by up to six times.
fn json_char_bytes(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\u{0}'..='\u{1f}' => 6,
        _ => ch.len_utf8(),
    }
}

fn json_text_bytes(text: &str) -> usize {
    text.chars().map(json_char_bytes).sum()
}

fn bound_roster_text(text: &str, budget: usize) -> String {
    if json_text_bytes(text) <= budget {
        return text.to_owned();
    }
    let mut remaining = budget - json_text_bytes(ROSTER_OUTPUT_SUFFIX);
    let mut end = 0;
    for ch in text.chars() {
        let size = json_char_bytes(ch);
        if size > remaining {
            break;
        }
        remaining -= size;
        end += ch.len_utf8();
    }
    format!("{}{ROSTER_OUTPUT_SUFFIX}", &text[..end])
}

// Version 2 pins per-worker routes; v1 readers must not inherit a wrong parent route.
const FLEET_ROSTER_VERSION: u32 = 2;
const FLEET_ROSTER_FILE: &str = "fleet.json";
/// Versioned durable claim that fences execution of one session's fleet to a
/// single live owner. A larger or malformed file fails closed instead of being
/// partially trusted.
const FLEET_LEASE_VERSION: u32 = 1;
const FLEET_LEASE_LIMIT: usize = 16 * 1024;

/// Bounded durable claim naming the owner that may execute a session's
/// workers.
///
/// `generation` is monotonic per durable store: every successful acquisition
/// takes the next generation, so a stale owner holding an older generation is
/// fenced even if it still has the roster in memory. `instance` is unique per
/// owning manager, so a duplicate session open cannot prove the current claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DurableFleetClaim {
    generation: u64,
    instance: String,
}

/// On-disk claim written beside the roster by the manager that owns the
/// session's durable fleet.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableFleetLease {
    version: u32,
    root_session: PathBuf,
    generation: u64,
    instance: String,
    claimed_at_ms: u64,
}

/// Exclusive owner of one session's durable fleet.
///
/// The claim is fenced twice: an owner-only advisory lock proves no *other live
/// process* owns the fleet, and the durable generation/instance pair proves
/// this manager still holds the claim it wrote. Both are required before a
/// restored worker may be started, so a stale process or a duplicate session
/// open fails closed instead of running the same worker twice.
struct FleetLease {
    file: std::fs::File,
    lock_path: PathBuf,
    lock_identity: secure_fs::PrivateLockIdentity,
    claim_path: PathBuf,
    claim: DurableFleetClaim,
    root_session: PathBuf,
}

impl std::fmt::Debug for FleetLease {
    /// The lease owns an open lock file and a private lock identity, neither of
    /// which belongs in diagnostics. A manual impl keeps `unwrap_err`/`expect`
    /// usable on `Result<FleetLease, String>` without printing them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetLease")
            .field("lock_path", &self.lock_path)
            .field("claim_path", &self.claim_path)
            .field("root_session", &self.root_session)
            .field("claim", &self.claim)
            .finish_non_exhaustive()
    }
}

/// Manager-local instance token. Unique per manager, ASCII, bounded.
fn fleet_instance_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        timestamp_ms()
    )
}

/// Lease lock and claim paths, scoped to one root session so two different
/// sessions sharing a workspace delegation directory never contend.
fn fleet_lease_paths(session_directory: &Path, root_session: &Path) -> (PathBuf, PathBuf) {
    let mut hasher = Sha256::new();
    hasher.update(root_session.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let stem = format!("fleet-{}", &digest[..16]);
    (
        session_directory.join(format!("{stem}.lock")),
        session_directory.join(format!("{stem}.lease")),
    )
}

fn fleet_roster_path(session_directory: &Path, root_session: &Path) -> PathBuf {
    // Use exactly the lease's owner identity, not the shared workspace directory.
    fleet_lease_paths(session_directory, root_session)
        .0
        .with_extension("json")
}

fn read_fleet_lease(claim_path: &Path) -> Option<DurableFleetLease> {
    let bytes = secure_fs::read_private_file_bounded(claim_path, FLEET_LEASE_LIMIT).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn lease_contention(error: &io::Error) -> bool {
    // `fs2` reports contention with the platform's lock error (EWOULDBLOCK on
    // Unix, ERROR_LOCK_VIOLATION on Windows), so compare against its own
    // contended error instead of assuming one `ErrorKind`.
    let contended = fs2::lock_contended_error();
    error.raw_os_error() == contended.raw_os_error()
        || matches!(error.kind(), io::ErrorKind::WouldBlock)
}

impl FleetLease {
    fn try_acquire(session_directory: &Path, root_session: &Path) -> Result<Self, String> {
        let (lock_path, claim_path) = fleet_lease_paths(session_directory, root_session);
        let file = secure_fs::open_private_lock_file(&lock_path)
            .map_err(|error| format!("session fleet lease lock is unavailable: {error}"))?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if lease_contention(&error) => {
                return Err(match read_fleet_lease(&claim_path) {
                    Some(holder) => format!(
                        "another live session owner holds the durable fleet lease (instance {}, generation {})",
                        holder.instance, holder.generation
                    ),
                    None => "another live session owner holds the durable fleet lease".to_owned(),
                });
            }
            Err(error) => return Err(format!("session fleet lease lock is unavailable: {error}")),
        }
        let lock_identity = secure_fs::validate_private_lock_after_acquire(&lock_path, &file)
            .map_err(|error| format!("session fleet lease lock failed validation: {error}"))?;
        let existing = secure_fs::read_private_file_bounded(&claim_path, FLEET_LEASE_LIMIT).ok();
        let previous = existing
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<DurableFleetLease>(bytes).ok());
        let generation = previous
            .as_ref()
            .map(|previous| previous.generation.saturating_add(1))
            .unwrap_or(1);
        let instance = fleet_instance_token();
        let durable = DurableFleetLease {
            version: FLEET_LEASE_VERSION,
            root_session: root_session.to_path_buf(),
            generation,
            instance: instance.clone(),
            claimed_at_ms: u64::try_from(timestamp_ms()).unwrap_or(u64::MAX),
        };
        let encoded = serde_json::to_vec(&durable)
            .map_err(|error| format!("session fleet lease could not be encoded: {error}"))?;
        secure_fs::write_private_atomic_if_unchanged(
            &claim_path,
            existing.as_deref(),
            &encoded,
            FLEET_LEASE_LIMIT,
        )
        .map_err(|error| format!("session fleet lease could not be claimed: {error}"))?;
        Ok(Self {
            file,
            lock_path,
            lock_identity,
            claim_path,
            claim: DurableFleetClaim {
                generation,
                instance,
            },
            root_session: root_session.to_path_buf(),
        })
    }

    fn claim(&self) -> DurableFleetClaim {
        self.claim.clone()
    }

    /// Re-proves the claim immediately before starting restored work. Any
    /// mismatch or unreadable claim fails closed rather than being trusted.
    fn is_current(&self) -> Result<(), String> {
        let bytes = secure_fs::read_private_file_bounded(&self.claim_path, FLEET_LEASE_LIMIT)
            .map_err(|error| format!("session fleet lease could not be verified: {error}"))?;
        let current: DurableFleetLease = serde_json::from_slice(&bytes)
            .map_err(|error| format!("session fleet lease could not be verified: {error}"))?;
        if current.version != FLEET_LEASE_VERSION || current.root_session != self.root_session {
            return Err("session fleet lease was replaced by another durable owner".into());
        }
        if current.generation != self.claim.generation || current.instance != self.claim.instance {
            return Err(format!(
                "session fleet lease moved to a newer owner (instance {}, generation {})",
                current.instance, current.generation
            ));
        }
        Ok(())
    }
}

impl Drop for FleetLease {
    fn drop(&mut self) {
        let revalidated = secure_fs::revalidate_private_lock_before_release(
            &self.lock_path,
            &self.file,
            &self.lock_identity,
        );
        let unlocked = fs2::FileExt::unlock(&self.file);
        let _ = (revalidated, unlocked);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct QueueUsage {
    messages: usize,
    bytes: usize,
}

impl QueueUsage {
    fn add(&mut self, bytes: usize) {
        self.messages = self.messages.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn add_usage(&mut self, other: Self) {
        self.messages = self.messages.saturating_add(other.messages);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    fn remove(&mut self, usage: Self) {
        self.messages = self.messages.saturating_sub(usage.messages);
        self.bytes = self.bytes.saturating_sub(usage.bytes);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DirectedMessage {
    /// Cryptographically random host delivery identity, persisted until the
    /// child session records this exact envelope.
    #[serde(default)]
    delivery_id: String,
    from: String,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
struct MailboxMessage {
    kind: &'static str,
    from: String,
    task_name: Option<String>,
    message: String,
    #[serde(skip)]
    evictable: bool,
    #[serde(skip)]
    continued: bool,
    #[serde(skip)]
    leased: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableMailboxMessage {
    kind: String,
    from: String,
    task_name: Option<String>,
    message: String,
    evictable: bool,
    continued: bool,
    leased: bool,
}

impl From<&MailboxMessage> for DurableMailboxMessage {
    fn from(message: &MailboxMessage) -> Self {
        Self {
            kind: message.kind.into(),
            from: message.from.clone(),
            task_name: message.task_name.clone(),
            message: message.message.clone(),
            evictable: message.evictable,
            continued: message.continued,
            leased: message.leased,
        }
    }
}

impl From<DurableMailboxMessage> for MailboxMessage {
    fn from(message: DurableMailboxMessage) -> Self {
        // Mailbox kinds are host-generated constants. Unknown persisted values
        // are rendered as a bounded diagnostic rather than becoming a trusted
        // static string.
        let kind = match message.kind.as_str() {
            "message" => "message",
            "task_status" => "task_status",
            _ => "diagnostic",
        };
        Self {
            kind,
            from: message.from,
            task_name: message.task_name,
            message: message.message,
            evictable: message.evictable,
            continued: message.continued,
            leased: message.leased,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct MailboxDeliveryPlan {
    id: u64,
    complete_messages: usize,
    partial_bytes: usize,
    touched_messages: usize,
}

struct WaitOutput {
    value: Value,
    delivery_id: Option<u64>,
}

struct SpawnRequest {
    task_name: String,
    display_task_name: Option<String>,
    message: String,
    extension_policy: Option<ExtensionAgentSessionPolicy>,
    extension_provenance: Option<ExtensionSpawnProvenance>,
}

struct ExtensionSpawnProvenance {
    parent_session_id: String,
    principal: String,
    resource_owner: String,
    profile: Option<String>,
    idempotency_key: String,
    fingerprint: Option<String>,
}

struct FollowUpRequest {
    target: String,
    message: String,
}

struct WorkerCommand {
    kind: WorkerCommandKind,
}

struct WorkerStartup {
    generation: u64,
    identity: AgentIdentity,
    session: Session,
    commands: mpsc::Receiver<WorkerCommand>,
    shutdown: crate::CancellationToken,
    initial_permit: OwnedSemaphorePermit,
    extension_policy: Option<ExtensionAgentSessionPolicy>,
    deadline: Option<tokio::time::Instant>,
    deadline_ms: Option<u64>,
}

/// Clears a record's process-local liveness flag when its worker task ends.
struct WorkerLiveness {
    manager: Arc<DelegationManager>,
    id: String,
    generation: u64,
}

/// One record this manager is about to start from its durable snapshot.
struct ReattachPlan {
    id: String,
    session: Session,
    // A transcript with no undelivered task is settled without using a slot.
    initial_permit: Option<OwnedSemaphorePermit>,
    claim: DurableFleetClaim,
}

impl WorkerLiveness {
    fn new(manager: &Arc<DelegationManager>, id: String, generation: u64) -> Self {
        Self {
            manager: Arc::clone(manager),
            id,
            generation,
        }
    }
}

impl Drop for WorkerLiveness {
    fn drop(&mut self) {
        self.manager.mark_worker_stopped(&self.id, self.generation);
    }
}

struct ChildRunContext<'a> {
    queued_delivery_ids: BTreeSet<String>,
    identity: &'a AgentIdentity,
    commands: &'a mut mpsc::Receiver<WorkerCommand>,
    shutdown: &'a crate::CancellationToken,
    extension_policy: Option<&'a ExtensionAgentSessionPolicy>,
    deadline: Option<tokio::time::Instant>,
}

impl WorkerCommand {
    fn message(message: DirectedMessage) -> Self {
        Self {
            kind: WorkerCommandKind::Message(message),
        }
    }

    fn follow_up() -> Self {
        Self {
            kind: WorkerCommandKind::FollowUp,
        }
    }

    fn shutdown() -> Self {
        Self {
            kind: WorkerCommandKind::Shutdown,
        }
    }
}

enum WorkerCommandKind {
    Message(DirectedMessage),
    FollowUp,
    Shutdown,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum ProvenanceEvent<'a> {
    TeamStarted {
        timestamp_ms: u128,
        root_session: &'a Path,
        limits: &'a DelegationLimits,
        mode: &'static str,
    },
    AgentSpawned {
        timestamp_ms: u128,
        agent_id: &'a str,
        agent_path: &'a str,
        parent_id: &'a str,
        task_name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        display_task_name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_parent_session_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_principal: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_resource_owner: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_profile: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_idempotency_key: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_fingerprint: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        task: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<&'a Path>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_reference: Option<&'a str>,
        effective_tool_policy: &'a EffectiveToolPolicy,
        orchestration_provenance: &'a DelegationOrchestrationProvenance,
    },
    AgentStatus {
        timestamp_ms: u128,
        agent_id: &'a str,
        status: &'a DelegatedAgentStatus,
    },
    Message {
        timestamp_ms: u128,
        from: &'a str,
        to: &'a str,
        kind: &'a str,
        message: &'a str,
    },
    InterruptRequested {
        timestamp_ms: u128,
        from: &'a str,
        to: &'a str,
    },
    /// Explicit session-scoped detachment boundary: the owning run ended but
    /// these workers survive it. This replaces run-scoped retirement as the
    /// default, keeping the end-of-run boundary visible instead of a silent
    /// vanish.
    RunDetached {
        timestamp_ms: u128,
        agent_ids: Vec<String>,
    },
    /// Explicit session-scoped reattachment boundary: a later owning run
    /// resumed these workers from the durable record.
    RunReattached {
        timestamp_ms: u128,
        agent_ids: Vec<String>,
    },
    /// A worker parked at the approval boundary was rediscovered by
    /// reattachment and deliberately not resumed.
    ReattachParked {
        timestamp_ms: u128,
        agent_id: &'a str,
        reason: &'a str,
    },
    /// A worker could not be reattached, with the bounded reason. The record
    /// keeps its durable state and the reason instead of disappearing.
    ReattachRefused {
        timestamp_ms: u128,
        agent_id: &'a str,
        reason: &'a str,
    },
    TeamShutdown {
        timestamp_ms: u128,
    },
}

struct ProvenanceJournal {
    file: Mutex<File>,
}

impl ProvenanceJournal {
    fn create(directory: &secure_fs::PrivateDirectory) -> Result<Self, SecureFileError> {
        let path = directory.path().join("provenance.jsonl");
        let file = directory.create_regular_file_for_append(&path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn append(&self, event: &ProvenanceEvent<'_>) -> io::Result<()> {
        let encoded = serde_json::to_vec(event).map_err(io::Error::other)?;
        self.append_encoded(&encoded)
    }

    fn append_encoded(&self, encoded: &[u8]) -> io::Result<()> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.write_all(encoded)?;
        file.write_all(b"\n")?;
        file.sync_data()
    }
}

impl DelegationManager {
    fn attach_telemetry(&self) -> watch::Receiver<Option<DelegationTelemetrySnapshot>> {
        let (sender, receiver) = watch::channel(None);
        {
            let mut telemetry = self
                .telemetry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            telemetry.sender = Some(sender);
        }
        self.publish_telemetry(None, None);
        receiver
    }

    fn publish_external_failure(&self, class: &str, reason: &str) {
        self.publish_telemetry(
            Some(class.to_owned()),
            Some(bounded_text_to(reason, MAX_TELEMETRY_FAILURE_BYTES)),
        );
    }

    fn publish_telemetry(&self, failure_class: Option<String>, failure_reason: Option<String>) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let captured_at_ms = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
        let children = state
            .records
            .values()
            .map(|record| {
                let elapsed_end = record.completed_at_ms.unwrap_or(captured_at_ms);
                let started = record.started_at_ms.unwrap_or(record.created_at_ms);
                let (failure_class_for_child, failure_reason_for_child) = match &record.status {
                    DelegatedAgentStatus::Failed { error } => (
                        Some(classify_delegation_failure(error).to_owned()),
                        Some(bounded_text_to(error, MAX_TELEMETRY_FAILURE_BYTES)),
                    ),
                    DelegatedAgentStatus::TimedOut => (
                        Some("timeout".to_owned()),
                        Some("worker exceeded its host-owned wall-time deadline".to_owned()),
                    ),
                    DelegatedAgentStatus::Interrupted => (
                        Some("cancellation".to_owned()),
                        Some("worker was interrupted by the owner".to_owned()),
                    ),
                    DelegatedAgentStatus::LimitReached {
                        turn_count,
                        turn_limit,
                        ..
                    } => (
                        Some("limit".to_owned()),
                        Some(format!(
                            "worker exhausted its turn budget ({turn_count}/{turn_limit} turns)"
                        )),
                    ),
                    DelegatedAgentStatus::Shutdown => (
                        Some("cancellation".to_owned()),
                        Some("worker was shut down by its owning run".to_owned()),
                    ),
                    DelegatedAgentStatus::AwaitingApproval { reason } => (
                        Some("approval".to_owned()),
                        Some(bounded_text_to(reason, MAX_TELEMETRY_FAILURE_BYTES)),
                    ),
                    DelegatedAgentStatus::Detached => (
                        Some("detached".to_owned()),
                        Some("worker outlived its owning run and awaits reattachment".to_owned()),
                    ),
                    _ => (None, None),
                };
                DelegationTelemetryChild {
                    child_id: record.identity.id.clone(),
                    task_name: record
                        .display_task_name
                        .clone()
                        .unwrap_or_else(|| record.task_name.clone()),
                    profile: record.extension_profile.clone(),
                    model: record
                        .extension_policy
                        .as_ref()
                        .and_then(|p| p.resolved_model.as_ref())
                        .map(|m| m.model.clone())
                        .unwrap_or_else(|| self.template.model.spec.id.0.clone()),
                    state: record.status.label().to_owned(),
                    phase: if !record.active_tools.is_empty() {
                        "using_tool".to_owned()
                    } else {
                        match &record.status {
                            DelegatedAgentStatus::Pending => "queued",
                            DelegatedAgentStatus::Idle => "idle",
                            DelegatedAgentStatus::Running => "thinking",
                            DelegatedAgentStatus::Completed { .. } => "completed",
                            DelegatedAgentStatus::LimitReached { .. } => "limit_reached",
                            DelegatedAgentStatus::Interrupted => "interrupted",
                            DelegatedAgentStatus::Failed { .. } => "failed",
                            DelegatedAgentStatus::TimedOut => "timed_out",
                            DelegatedAgentStatus::Detached => "detached",
                            DelegatedAgentStatus::AwaitingApproval { .. } => "awaiting_approval",
                            DelegatedAgentStatus::Shutdown => "shutdown",
                        }
                        .to_owned()
                    },
                    current_tool: record.active_tools.values().next_back().cloned(),
                    tool_use_count: record.tool_call_count,
                    input_tokens: record.usage.input_tokens,
                    cache_read_tokens: record.usage.cache_read_tokens,
                    cache_write_tokens: record.usage.cache_write_tokens,
                    output_tokens: record.usage.output_tokens,
                    estimated_output_tokens: (record.streamed_output_bytes > 0).then(|| {
                        record
                            .usage
                            .output_tokens
                            .saturating_add(record.streamed_output_bytes.div_ceil(4))
                    }),
                    reasoning_tokens: record.usage.reasoning_tokens,
                    total_tokens: record.usage.total_tokens,
                    cost: if record.usage_uncertain {
                        None
                    } else {
                        record.cost
                    },
                    cost_microdollars: record.cost_microdollars,
                    elapsed_ms: elapsed_end.saturating_sub(started),
                    failure_class: failure_class_for_child,
                    failure_reason: failure_reason_for_child,
                    effective_tool_policy: record.effective_tool_policy.clone(),
                    orchestration_provenance: record.orchestration_provenance.clone(),
                    session: delegated_session_reference(&record.session_path),
                }
            })
            .collect::<Vec<_>>();

        let total_cost_microdollars = children
            .iter()
            .map(|child| child.cost_microdollars)
            .try_fold(0u64, |total, cost| Some(total.saturating_add(cost?)));
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        telemetry.revision = telemetry.revision.saturating_add(1);
        let snapshot = DelegationTelemetrySnapshot {
            revision: telemetry.revision,
            captured_at_ms,
            children,
            total_cost_microdollars,
            failure_reason,
            failure_class,
        };
        if let Some(sender) = telemetry.sender.clone() {
            if sender.send(Some(snapshot)).is_err() {
                telemetry.sender = None;
            }
        }
        // Keep the state snapshot and telemetry revision/send in one ordered
        // critical section. Otherwise an older captured roster can be
        // published after a newer one and become the watch channel's latest.
        drop(telemetry);
        drop(state);
    }

    fn detach_telemetry(&self) {
        self.telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sender = None;
    }

    /// Single construction point for a manager.
    ///
    /// Every field is set here exactly once, and the state/telemetry structs
    /// come from their `Default`, so adding a field cannot leave a construction
    /// site (production or fixture) silently incomplete.
    fn assemble(
        config: DelegationConfig,
        root_tools: bool,
        team_directory: PathBuf,
        team_storage: Option<Arc<secure_fs::PrivateDirectory>>,
        journal: ProvenanceJournal,
        template: DelegationTemplate,
        root_session: PathBuf,
    ) -> Arc<Self> {
        let roster_path = Some(fleet_roster_path(&config.session_directory, &root_session));
        let child_slots = config.limits.max_concurrent_agents.saturating_sub(1);
        Arc::new(Self {
            config,
            root_tools,
            team_directory,
            team_storage,
            journal,
            template,
            state: Mutex::new(ManagerState::default()),
            journal_order: Mutex::new(()),
            permits: RwLock::new(Arc::new(Semaphore::new(child_slots))),
            changed: Notify::new(),
            telemetry: Mutex::new(DelegationTelemetryState::default()),
            span_context: RwLock::new(TelemetryContext::default()),
            roster_path,
            root_session,
            lease: RwLock::new(None),
            lease_refusal: RwLock::new(None),
        })
    }

    fn create(
        config: DelegationConfig,
        template: DelegationTemplate,
        root_session: &Path,
        root_tools: bool,
    ) -> Result<Arc<Self>, DelegationError> {
        Self::create_with_journal(config, template, root_session, root_tools, |directory| {
            Ok(ProvenanceJournal::create(directory)?)
        })
    }

    fn create_with_journal(
        config: DelegationConfig,
        template: DelegationTemplate,
        root_session: &Path,
        root_tools: bool,
        create_journal: impl FnOnce(
            &secure_fs::PrivateDirectory,
        ) -> Result<ProvenanceJournal, DelegationError>,
    ) -> Result<Arc<Self>, DelegationError> {
        config.validate()?;
        let team_storage = create_private_team_directory(&config.session_directory)?;
        let team_directory = team_storage.path().to_path_buf();
        let session_directory = config.session_directory.clone();
        let activation = (|| {
            let journal = create_journal(&team_storage)?;
            let manager = Self::assemble(
                config,
                root_tools,
                team_directory.clone(),
                Some(Arc::clone(&team_storage)),
                journal,
                template,
                root_session.to_path_buf(),
            );
            manager.journal.append(&ProvenanceEvent::TeamStarted {
                timestamp_ms: timestamp_ms(),
                root_session,
                limits: &manager.config.limits,
                mode: match manager.config.mode {
                    DelegationMode::Available => "available",
                    DelegationMode::Proactive => "proactive",
                },
            })?;
            // Claim the session's durable fleet now that activation has durably
            // begun, so a failed activation leaves no lease artifacts behind. A
            // refusal is recorded, not fatal: this manager can still observe the
            // fleet, but it fails closed on reattach and never overwrites the
            // real owner's roster.
            let (lease, lease_refusal) =
                match FleetLease::try_acquire(&session_directory, root_session) {
                    Ok(lease) => (Some(lease), None),
                    Err(reason) => (None, Some(reason)),
                };
            *manager
                .lease
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = lease;
            *manager
                .lease_refusal
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = lease_refusal;
            // Reconstruct the session-owned fleet persisted before this
            // process (or this run) started. Records without a live task
            // surface as explicit `detached` diagnostics; the owning session
            // reattaches them on a later turn.
            manager.restore_durable_fleet();
            Ok(manager)
        })();
        match activation {
            Ok(manager) => Ok(manager),
            Err(error) => match cleanup_failed_team_activation(&team_storage) {
                Ok(()) => Err(error),
                Err(rollback) => Err(DelegationError::ActivationRollback {
                    activation: error.to_string(),
                    rollback,
                }),
            },
        }
    }

    fn root_binding(self: &Arc<Self>) -> DelegationBinding {
        DelegationBinding {
            manager: Arc::clone(self),
            identity: AgentIdentity {
                id: ROOT_AGENT_ID.into(),
                path: ROOT_AGENT_PATH.into(),
                depth: 0,
            },
            system_instructions: Arc::from(if self.root_tools {
                root_instructions(&self.config)
            } else {
                String::new()
            }),
        }
    }

    /// Installs the owning agent's explicit span observer for child runs.
    pub(crate) fn set_span_context(&self, context: TelemetryContext) {
        *self
            .span_context
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = context;
    }

    /// Returns the installed span observer (inert by default).
    fn span_context(&self) -> TelemetryContext {
        self.span_context
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn create_team_file(&self, path: &Path) -> Result<File, SecureFileError> {
        match &self.team_storage {
            Some(directory) => directory.create_regular_file_for_append(path),
            None => secure_fs::create_regular_file_for_append(path),
        }
    }

    fn open_team_file_for_append(&self, path: &Path) -> Result<File, SecureFileError> {
        match &self.team_storage {
            Some(directory) => directory.open_regular_file_for_append(path),
            None => secure_fs::open_regular_file_for_append(path),
        }
    }

    fn remove_team_file_if_exists(&self, path: &Path) -> Result<bool, SecureFileError> {
        match &self.team_storage {
            Some(directory) => directory.remove_regular_file_if_exists(path),
            None => secure_fs::remove_regular_file_if_exists(path),
        }
    }

    fn reopen_child_session(&self, path: &Path) -> Result<Session, DelegationError> {
        let file = match &self.team_storage {
            Some(directory) if path.parent() == Some(directory.path()) => {
                self.open_team_file_for_append(path)?
            }
            // A record restored from an earlier process points at its original
            // team directory, which is retained on disk. Open it by absolute
            // path instead of through the current team capability.
            _ => secure_fs::open_regular_file_for_append(path)?,
        };
        Ok(Session::open_with_file(path, file)?)
    }

    /// Whether this manager holds the session's durable fleet lease.
    fn lease_held(&self) -> bool {
        self.lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// Verify an existing claim without acquiring ownership while state is locked.
    fn verify_held_fleet_lease(&self) -> Result<(), String> {
        self.lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .ok_or_else(|| {
                self.lease_refusal_reason()
                    .unwrap_or_else(|| "this session does not hold the durable fleet lease".into())
            })?
            .is_current()
    }

    /// Claim this manager holds, when it holds one.
    fn current_claim(&self) -> Option<DurableFleetClaim> {
        self.lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(FleetLease::claim)
    }

    /// Bounded refusal reason when the lease is not held.
    fn lease_refusal_reason(&self) -> Option<String> {
        self.lease_refusal
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Acquire the lease once, at the first boundary where this session owns a
    /// run. Retrying here is what lets a rebuilt or restarted session take the
    /// fleet over from a previous owner that has since released it.
    fn ensure_fleet_lease(&self) -> Result<(), String> {
        if let Some(lease) = self
            .lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            return lease.is_current();
        }
        let Some(roster_path) = self.roster_path.clone() else {
            return Err("this session has no durable fleet roster or lease".into());
        };
        let session_directory = roster_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.team_directory.clone());
        let mut lease = self
            .lease
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(held) = lease.as_ref() {
            return held.is_current();
        }
        match FleetLease::try_acquire(&session_directory, &self.root_session) {
            Ok(acquired) => {
                // The observer's construction-time snapshot can be arbitrarily
                // old. Validate and load the owner's latest roster while the
                // newly acquired lock excludes every other writer.
                let fleet = match self.read_durable_fleet() {
                    Ok(Some(fleet)) => fleet,
                    Ok(None) => DurableFleet {
                        version: FLEET_ROSTER_VERSION,
                        root_session: self.root_session.clone(),
                        records: Vec::new(),
                        next_mailbox_delivery: 1,
                        root_mailbox: VecDeque::new(),
                        root_mailbox_delivery: None,
                    },
                    Err(reason) => {
                        drop(acquired);
                        drop(lease);
                        *self
                            .lease_refusal
                            .write()
                            .unwrap_or_else(|p| p.into_inner()) = Some(reason.clone());
                        return Err(reason);
                    }
                };
                *lease = Some(acquired);
                drop(lease);
                *self
                    .lease_refusal
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                self.restore_durable_fleet_from(fleet, true);
                if let Some(error) = &self
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .persistence_error
                {
                    return Err(format!("could not persist reclaimed fleet: {error}"));
                }
                Ok(())
            }
            Err(reason) => {
                drop(lease);
                *self
                    .lease_refusal
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason.clone());
                Err(reason)
            }
        }
    }

    /// Fresh claim for starting a restored or newly spawned worker.
    ///
    /// Fails closed whenever the lease is absent, stale, or already superseded
    /// by a newer generation than the record was written under.
    fn fresh_claim_for(&self, record: &AgentRecord) -> Result<DurableFleetClaim, String> {
        let lease = self
            .lease
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = lease.as_ref() else {
            return Err(self
                .lease_refusal_reason()
                .unwrap_or_else(|| "this session does not hold the durable fleet lease".into()));
        };
        lease.is_current()?;
        let claim = lease.claim();
        if let Some(existing) = &record.claim {
            if existing.generation > claim.generation
                || (existing.generation == claim.generation && existing.instance != claim.instance)
            {
                return Err(format!(
                    "worker record is claimed by a newer session fleet owner (instance {}, generation {}); this owner holds generation {}",
                    existing.instance, existing.generation, claim.generation
                ));
            }
        }
        Ok(claim)
    }

    /// Whether the owning session released its root agent while workers lived.
    fn session_owner_released(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .session_owner_released
    }

    /// Persist the session-owned fleet roster.
    ///
    /// Fail-closed in two directions: serialization or write failure marks the
    /// team unusable through the same `persistence_error` path as the
    /// provenance journal, and a manager that does not hold the session fleet
    /// lease never writes the roster, so a duplicate session open cannot
    /// overwrite the real owner's durable records.
    fn persist_durable_fleet_locked(&self, state: &mut ManagerState) {
        let Some(path) = self.roster_path.clone() else {
            return;
        };
        if state.persistence_error.is_some() || !self.lease_held() {
            return;
        }
        if let Err(reason) = self.verify_held_fleet_lease() {
            self.fail_persistence_locked(state, &io::Error::other(reason));
            return;
        }
        let fleet = DurableFleet {
            version: FLEET_ROSTER_VERSION,
            root_session: self.root_session.clone(),
            records: state.records.values().map(durable_fleet_record).collect(),
            next_mailbox_delivery: state.next_mailbox_delivery,
            root_mailbox: state.root_mailbox.iter().map(Into::into).collect(),
            root_mailbox_delivery: state.root_mailbox_delivery,
        };
        let encoded = match encode_durable_fleet(fleet) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.fail_persistence_locked(state, &io::Error::other(error));
                return;
            }
        };
        if encoded.len() > MAX_FLEET_ROSTER_BYTES {
            self.fail_persistence_locked(
                state,
                &io::Error::other("durable fleet roster exceeded its bounded size"),
            );
            return;
        }
        if let Err(error) = secure_fs::write_private_atomic(&path, &encoded, MAX_FLEET_ROSTER_BYTES)
        {
            self.fail_persistence_locked(state, &io::Error::other(error));
        }
    }

    /// Maps one durable record into the in-memory worker record.
    ///
    /// This is the only place the durable fields are threaded into
    /// [`AgentRecord`], so the roster restore and every test fixture share one
    /// mapping and a new durable field cannot be missed by a fixture. Fixtures
    /// state attachment explicitly afterwards, because `detached: true` here is
    /// the roster-restore default (a restored record has no live task).
    fn agent_record_from_durable(
        durable: DurableFleetRecord,
        effective_tool_policy: EffectiveToolPolicy,
        refusal: Option<&str>,
        command_tx: mpsc::Sender<WorkerCommand>,
        command_rx: Option<mpsc::Receiver<WorkerCommand>>,
    ) -> AgentRecord {
        let status = match durable.status {
            DelegatedAgentStatus::Pending
            | DelegatedAgentStatus::Idle
            | DelegatedAgentStatus::Running => DelegatedAgentStatus::Detached,
            other => other,
        };
        // A record restored from an earlier owner is only runnable when the
        // restoring manager can prove a fresh fleet claim. Without one the
        // refusal is visible on the record instead of a silent stall.
        let resumable = !matches!(status, DelegatedAgentStatus::Shutdown);
        let claim_reason = match (refusal, resumable) {
            (Some(reason), true) => Some(format!("not reattached: {reason}")),
            _ => None,
        };
        let extension_policy = durable.extension_policy.clone();
        let orchestration_provenance = child_orchestration_provenance(extension_policy.as_ref());
        AgentRecord {
            identity: AgentIdentity {
                id: durable.agent_id,
                path: durable.agent_path,
                depth: durable.depth,
            },
            task_name: durable.task_name,
            display_task_name: durable.display_task_name,
            parent_id: durable.parent_id,
            session_path: durable.session_path,
            status,
            command_tx,
            shutdown: crate::CancellationToken::default(),
            interrupt_requested: false,
            pending_messages: durable.pending_messages,
            inflight_message_ids: BTreeSet::new(),
            reserved_messages: QueueUsage::default(),
            queued_follow_ups: durable.queued_follow_ups.iter().fold(
                QueueUsage::default(),
                |mut usage, follow_up| {
                    usage.add_usage(follow_up.usage());
                    usage
                },
            ),
            pending_follow_ups: durable.queued_follow_ups,
            pending_initial_task: durable.pending_initial_task,
            mailbox: durable.mailbox.into_iter().map(Into::into).collect(),
            mailbox_delivery: durable.mailbox_delivery,
            resource_owner: durable.resource_owner,
            extension_policy,
            effective_tool_policy,
            orchestration_provenance,
            extension_principal: durable.extension_principal,
            extension_profile: durable.extension_profile,
            extension_idempotency_key: durable.extension_idempotency_key,
            extension_resource_owner: durable.extension_resource_owner,
            extension_message_sha256: durable.extension_message_sha256,
            extension_requested_policy: durable.extension_requested_policy,
            extension_fingerprint: durable.extension_fingerprint,
            created_at_ms: durable.created_at_ms,
            started_at_ms: durable.started_at_ms,
            completed_at_ms: durable.completed_at_ms,
            turn_count: durable.turn_count,
            tool_call_count: durable.tool_call_count,
            active_tools: BTreeMap::new(),
            recent_tools: VecDeque::new(),
            usage: durable.usage,
            streamed_output_bytes: 0,
            usage_uncertain: durable.usage_uncertain,
            usage_exposure: durable.usage_exposure,
            cost: durable.cost,
            cost_microdollars: durable.cost_microdollars,
            deadline_at_ms: durable.deadline_at_ms,
            turn_limit: durable.turn_limit,
            detached: true,
            live_task: false,
            worker_generation: 0,
            // A restored record keeps its command receiver for every state that
            // an explicit follow-up can resume, so a later turn is never told
            // "no longer available" for a worker the session still owns.
            detached_commands: resumable.then_some(command_rx).flatten(),
            durable_diagnostic: claim_reason.or(durable.durable_diagnostic),
            claim: durable.claim,
        }
    }

    fn discard_session_delivered_inputs(record: &mut AgentRecord) -> Result<(), String> {
        let file = secure_fs::open_private_file_for_read(&record.session_path)
            .map_err(|error| format!("could not open child session: {error}"))?;
        let session = Session::open_read_only_with_file(record.session_path.clone(), file)
            .map_err(|error| format!("could not read child session: {error}"))?;
        Self::discard_inputs_delivered_to_session(record, &session)
    }

    fn discard_inputs_delivered_to_session(
        record: &mut AgentRecord,
        session: &Session,
    ) -> Result<(), String> {
        // Reconcile only the active head's ancestry. Historical entries on an
        // abandoned branch are not authority for a later resumed worker.
        let mut delivered = BTreeSet::new();
        let mut cursor = session.head();
        while let Some(id) = cursor {
            let entry = session
                .entry(&id)
                .ok_or_else(|| "child session has an invalid active ancestry".to_owned())?;
            if let crate::session::EntryValue::Message(octet_ai::Message::User(message)) =
                &entry.value
            {
                for text in message.content.iter().filter_map(|part| match part {
                    octet_ai::UserPart::Text(text) => Some(text.as_str()),
                    _ => None,
                }) {
                    delivered.extend(delivery_ids_in_envelopes(text));
                }
            }
            cursor = entry.parent.clone();
        }
        if record
            .pending_initial_task
            .as_ref()
            .is_some_and(|task| delivered.contains(&task.delivery_id))
        {
            record.pending_initial_task = None;
        }
        // Legacy empty IDs have no safe identity evidence and are retained.
        record.pending_messages.retain(|message| {
            message.delivery_id.is_empty() || !delivered.contains(&message.delivery_id)
        });
        record.pending_follow_ups.retain(|follow_up| {
            follow_up.delivery_id.is_empty() || !delivered.contains(&follow_up.delivery_id)
        });
        record.queued_follow_ups =
            record
                .pending_follow_ups
                .iter()
                .fold(QueueUsage::default(), |mut usage, follow_up| {
                    usage.add_usage(follow_up.usage());
                    usage
                });
        Ok(())
    }

    /// Reconstruct the session-owned fleet persisted by an earlier run or
    /// process.
    ///
    /// Records that were live when the snapshot was written have no task in
    /// this process, so they surface explicitly as
    /// [`DelegatedAgentStatus::Detached`] with a bounded diagnostic — never as
    /// a silently-forgotten worker. A malformed, oversized, foreign, or
    /// unreadable roster is ignored entirely (fail closed) rather than
    /// partially trusted.
    fn read_durable_fleet(&self) -> Result<Option<DurableFleet>, String> {
        let Some(path) = self.roster_path.as_ref() else {
            return Ok(None);
        };
        let (bytes, from_legacy) =
            match secure_fs::read_private_file_bounded(path, MAX_FLEET_ROSTER_BYTES) {
                Ok(bytes) => (bytes, false),
                // Legacy rosters are read-only migration sources. New scoped
                // snapshots always take precedence, even when unreadable.
                Err(SecureFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    let legacy = self.config.session_directory.join(FLEET_ROSTER_FILE);
                    match secure_fs::read_private_file_bounded(&legacy, MAX_FLEET_ROSTER_BYTES) {
                        Ok(bytes) => (bytes, true),
                        Err(SecureFileError::Io(error))
                            if error.kind() == io::ErrorKind::NotFound =>
                        {
                            return Ok(None);
                        }
                        Err(error) => {
                            return Err(format!("durable fleet roster is unavailable: {error}"));
                        }
                    }
                }
                Err(error) => return Err(format!("durable fleet roster is unavailable: {error}")),
            };
        let fleet: DurableFleet = serde_json::from_slice(&bytes)
            .map_err(|error| format!("durable fleet roster is invalid: {error}"))?;
        if from_legacy && fleet.root_session != self.root_session {
            return Ok(None);
        }
        if !matches!(fleet.version, 1 | FLEET_ROSTER_VERSION)
            || fleet.root_session != self.root_session
        {
            return Err("durable fleet roster has an incompatible version or owner".into());
        }
        Ok(Some(fleet))
    }

    fn restore_durable_fleet(&self) {
        match self.read_durable_fleet() {
            Ok(Some(fleet)) => self.restore_durable_fleet_from(fleet, false),
            Ok(None) => {}
            Err(reason) if self.lease_held() => {
                // A claimed but unreadable roster is not an empty fleet. Never
                // admit a new worker that would overwrite unknown accepted work.
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                self.fail_persistence_locked(&mut state, &io::Error::other(reason));
            }
            Err(_) => {} // A lease-refused observer has no authority to repair it.
        }
    }

    /// A new lease must replace the observer's old projection, including
    /// counters and mailboxes, before any admission or roster write.
    fn restore_durable_fleet_from(&self, fleet: DurableFleet, replace: bool) {
        let effective_tool_policy = self
            .template
            .sandbox
            .effective_tool_policy(self.template.effect_broker.policy());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.shutting_down || state.persistence_error.is_some() {
            return;
        }
        if replace {
            state.records.clear();
            state.total_agents = 1;
            state.next_agent_number = 1;
            state.root_mailbox.clear();
            state.root_mailbox_delivery = None;
        }
        state.next_mailbox_delivery = fleet.next_mailbox_delivery.max(1);
        state.root_mailbox = fleet.root_mailbox.into_iter().map(Into::into).collect();
        state.root_mailbox_delivery = fleet.root_mailbox_delivery;
        let refusal = self.lease_refusal_reason();
        for durable in fleet.records {
            if state.records.contains_key(&durable.agent_id) {
                continue;
            }
            let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
            let mut record = Self::agent_record_from_durable(
                durable,
                effective_tool_policy.clone(),
                refusal.as_deref(),
                command_tx,
                Some(command_rx),
            );
            // The child session is authoritative for prompt delivery. A roster
            // snapshot can lag a successful append, so never replay a queued
            // input that is already present in that durable session. An unreadable
            // authority fails closed rather than guessing from queue payloads.
            if let Err(error) = Self::discard_session_delivered_inputs(&mut record) {
                // Unavailable authority is not evidence of delivery. Preserve
                // every accepted payload and attempt count until a successful
                // reopen can reconcile them, even across another restart.
                record.status = DelegatedAgentStatus::Detached;
                record.detached = true;
                record.durable_diagnostic = Some(bounded_text(&format!(
                    "child session authority could not be read; queued inputs retained without replay: {error}"
                )));
            }
            if let Some(number) = agent_number_from_id(&record.identity.id) {
                state.next_agent_number = state.next_agent_number.max(number.saturating_add(1));
            }
            state.records.insert(record.identity.id.clone(), record);
            state.total_agents = state.total_agents.saturating_add(1);
        }
        // Upgrade legacy records and acknowledge any child-session delivery
        // observations before another process can reconstruct this roster.
        self.persist_durable_fleet_locked(&mut state);
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    /// Session-scoped detachment: leave the fleet alive when the owning run
    /// ends and record that boundary explicitly.
    ///
    /// Nothing is cancelled or cleared. Workers that need no new authority
    /// keep running; workers that do park in
    /// [`DelegatedAgentStatus::AwaitingApproval`]. The durable roster is
    /// refreshed so a later turn or a restarted process can reconstruct the
    /// fleet.
    fn detach_run(&self, owner: &AgentIdentity) {
        if owner.id != ROOT_AGENT_ID {
            return;
        }
        let detached_ids = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.shutting_down || state.persistence_error.is_some() {
                return;
            }
            let mut detached_ids = Vec::new();
            for record in state.records.values_mut() {
                if record.status.is_running() || record.status.is_recoverable() {
                    record.detached = true;
                    detached_ids.push(record.identity.id.clone());
                }
            }
            self.persist_durable_fleet_locked(&mut state);
            detached_ids
        };
        if !detached_ids.is_empty() {
            let event = ProvenanceEvent::RunDetached {
                timestamp_ms: timestamp_ms(),
                agent_ids: detached_ids,
            };
            if let Ok(encoded) = serde_json::to_vec(&event) {
                let _ = self.journal.append_encoded(&encoded);
            }
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    /// Reattach every detached record the owning session can prove it may run.
    ///
    /// Reattachment runs only undelivered durable tasks. If reconciliation
    /// leaves no task, the worker settles as interrupted with its session
    /// retained for an explicit follow-up; it must not idle forever as pending
    /// or replay already delivered work. Each start acquires a permit so the
    /// concurrency cap still holds across the turn boundary.
    ///
    /// Every start requires a fresh durable fleet claim: a manager whose lease
    /// was refused, was superseded, or cannot be re-proved refuses the record
    /// and keeps the bounded reason on it. A worker parked at the approval
    /// boundary is never resumed here — it stays parked until an explicit
    /// decision arrives. Reattachment emits the same journaled lifecycle
    /// notifications the live spawn path emits, plus an explicit
    /// `run_reattached` boundary record.
    fn reattach_detached(self: &Arc<Self>, owner: &AgentIdentity) -> Result<(), String> {
        if owner.id != ROOT_AGENT_ID {
            return Ok(());
        }
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Prove (or re-acquire) the durable claim before trusting any record.
        // A refusal is recorded on every affected record and surfaced below.
        let _ = self.ensure_fleet_lease();
        let mut plans = Vec::new();
        let mut reattached = Vec::new();
        let mut parked = Vec::new();
        let mut refused = Vec::new();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.shutting_down {
                return Err("delegation team is shutting down".into());
            }
            if let Some(error) = &state.persistence_error {
                return Err(format!("delegation persistence is unavailable: {error}"));
            }
            let ids = state.records.keys().cloned().collect::<Vec<_>>();
            for id in ids {
                let Some(record) = state.records.get(&id) else {
                    continue;
                };
                // Two kinds of record are reattachable at a new owning run:
                // * a live worker that survived the previous turn in this process
                //   (`detached` marker set while its task keeps running), and
                // * a durable record reconstructed from the roster with no live
                //   task (`Detached`).
                let live_task = record.live_task;
                let detached = record.detached || record.status.is_recoverable();
                if !detached {
                    continue;
                }
                if live_task {
                    // The worker still owns its receiver and its permit in this
                    // process: reattachment only clears the run-scoped
                    // detachment marker. No second worker, no second permit.
                    if let Some(record) = state.records.get_mut(&id) {
                        record.detached = false;
                        record.durable_diagnostic = None;
                    }
                    continue;
                }
                let park_reason = match &record.status {
                    DelegatedAgentStatus::AwaitingApproval { reason } => Some(reason.clone()),
                    _ => None,
                };
                if let Some(reason) = park_reason {
                    // Unattended mutation fails closed. A worker parked on
                    // authority it no longer has stays parked across
                    // reattachment; only an explicit decision (an owner-bound
                    // follow-up issued in a run that carries authority) may
                    // resume it.
                    let diagnostic = format!(
                        "parked at the approval boundary and not resumed by reattachment; an explicit decision is required: {reason}"
                    );
                    if let Some(record) = state.records.get_mut(&id) {
                        record.detached = true;
                        record.durable_diagnostic = Some(bounded_text(&diagnostic));
                    }
                    parked.push((id, reason));
                    continue;
                }
                if !matches!(record.status, DelegatedAgentStatus::Detached)
                    && !record.status.is_running()
                {
                    // A settled record is resumed by an explicit follow-up, not
                    // by reattachment.
                    continue;
                }
                let claim = match self.fresh_claim_for(record) {
                    Ok(claim) => claim,
                    Err(reason) => {
                        let diagnostic = format!("worker was not reattached: {reason}");
                        if let Some(record) = state.records.get_mut(&id) {
                            record.detached = true;
                            record.durable_diagnostic = Some(bounded_text(&diagnostic));
                        }
                        refused.push((id, bounded_text(&reason)));
                        continue;
                    }
                };
                let session = match self.reopen_child_session(&record.session_path) {
                    Ok(session) => session,
                    Err(error) => {
                        let reason = bounded_text(&format!(
                            "worker was not reattached: the child session could not be reopened: {error}"
                        ));
                        let record = state.records.get_mut(&id).expect("selected child exists");
                        record.status = DelegatedAgentStatus::Detached;
                        record.detached = true;
                        record.durable_diagnostic = Some(reason.clone());
                        refused.push((id, reason));
                        continue;
                    }
                };
                let record = state.records.get_mut(&id).expect("selected child exists");
                if let Err(error) = Self::discard_inputs_delivered_to_session(record, &session) {
                    let reason = bounded_text(&format!("worker was not reattached: {error}"));
                    record.detached = true;
                    record.durable_diagnostic = Some(reason.clone());
                    refused.push((id, reason));
                    continue;
                }
                // A delivered task needs no execution slot. Otherwise many
                // interrupted transcripts can starve a later runnable child.
                let permit = if record.pending_initial_task.is_some()
                    || !record.pending_follow_ups.is_empty()
                {
                    match self.current_permits().try_acquire_owned() {
                        Ok(permit) => Some(permit),
                        // The bound is authoritative: a record that cannot acquire
                        // a slot stays visibly detached instead of oversubscribing.
                        Err(_) => {
                            let reason = "no free execution slot is available for reattachment";
                            if let Some(record) = state.records.get_mut(&id) {
                                record.detached = true;
                                record.durable_diagnostic = Some(bounded_text(&format!(
                                    "worker was not reattached: {reason}"
                                )));
                            }
                            refused.push((id, reason.to_owned()));
                            continue;
                        }
                    }
                } else {
                    None
                };
                // Keep receivers and liveness on the records until the entire
                // batch's provenance is committed. A failed reopen only refuses
                // this child; a journal failure leaves all unstarted receivers
                // owned by their records and drops the reserved permits.
                reattached.push(id.clone());
                plans.push(ReattachPlan {
                    id,
                    session,
                    initial_permit: permit,
                    claim,
                });
            }
        }
        // Journal the reattachment boundary before publishing runnable tasks
        // or settling delivered-only transcripts as interrupted.
        for plan in &plans {
            let status = if plan.initial_permit.is_some() {
                DelegatedAgentStatus::Pending
            } else {
                DelegatedAgentStatus::Interrupted
            };
            let event = ProvenanceEvent::AgentStatus {
                timestamp_ms: timestamp_ms(),
                agent_id: &plan.id,
                status: &status,
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        if !reattached.is_empty() {
            let event = ProvenanceEvent::RunReattached {
                timestamp_ms: timestamp_ms(),
                agent_ids: reattached.clone(),
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        for (id, reason) in &parked {
            let event = ProvenanceEvent::ReattachParked {
                timestamp_ms: timestamp_ms(),
                agent_id: id,
                reason,
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        for (id, reason) in &refused {
            let event = ProvenanceEvent::ReattachRefused {
                timestamp_ms: timestamp_ms(),
                agent_id: id,
                reason,
            };
            if let Err(error) = self.journal.append(&event) {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment provenance: {error}"
                ));
            }
        }
        for plan in plans {
            let has_tasks = !self.restored_tasks(&plan.id).is_empty();
            let status = if has_tasks {
                DelegatedAgentStatus::Pending
            } else {
                DelegatedAgentStatus::Interrupted
            };
            let settled_at = timestamp_ms();
            if let Err(error) = self.journal.append(&ProvenanceEvent::AgentStatus {
                timestamp_ms: settled_at,
                agent_id: &plan.id,
                status: &status,
            }) {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(format!(
                    "could not persist reattachment settlement: {error}"
                ));
            }
            let startup = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let record = state
                    .records
                    .get_mut(&plan.id)
                    .expect("selected child exists");
                record.status = status;
                record.detached = false;
                record.claim = Some(plan.claim);
                record.live_task = has_tasks;
                let startup = if has_tasks {
                    let commands = match record.detached_commands.take() {
                        Some(commands) => commands,
                        None => {
                            let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
                            record.command_tx = tx;
                            rx
                        }
                    };
                    record.durable_diagnostic = None;
                    record.worker_generation += 1;
                    Some(WorkerStartup {
                        generation: record.worker_generation,
                        identity: record.identity.clone(),
                        session: plan.session,
                        commands,
                        shutdown: record.shutdown.clone(),
                        initial_permit: plan
                            .initial_permit
                            .expect("runnable reattachment reserved a slot"),
                        extension_policy: record.extension_policy.clone(),
                        deadline: record.deadline_at_ms.map(wall_deadline_instant),
                        deadline_ms: record.deadline_at_ms,
                    })
                } else {
                    record.completed_at_ms = Some(u64::try_from(settled_at).unwrap_or(u64::MAX));
                    record.durable_diagnostic = Some(
                        "worker was interrupted before its outcome was retained; no undelivered task remains. Use subagent_continue (or followup_task) to resume the retained session explicitly; delivered work was not replayed".into(),
                    );
                    None
                };
                self.persist_durable_fleet_locked(&mut state);
                if let Some(error) = &state.persistence_error {
                    return Err(format!(
                        "could not persist reattachment settlement: {error}"
                    ));
                }
                startup
            };
            if let Some(startup) = startup {
                self.spawn_worker(startup);
            }
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.persist_durable_fleet_locked(&mut state);
            if let Some(error) = &state.persistence_error {
                return Err(format!("could not persist reattachment roster: {error}"));
            }
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        Ok(())
    }

    /// In-process resolution of a worker's launchable interactive handle.
    ///
    /// Unlike [`resolve_launchable_child_session`], this knows the
    /// process-local liveness the durable roster cannot carry, so it refuses a
    /// session that a live worker still owns.
    pub(crate) fn launchable_child_session(
        &self,
        reference: &str,
    ) -> Result<LaunchableChildSession, DelegationError> {
        validate_launch_reference(reference)?;
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state
            .records
            .values()
            .find(|record| {
                delegated_session_reference(&record.session_path).as_deref() == Some(reference)
            })
            .ok_or_else(|| DelegationError::Unlaunchable("unknown worker handle".into()))?;
        launchability(record).map_err(DelegationError::Unlaunchable)?;
        Ok(LaunchableChildSession {
            reference: reference.to_owned(),
            session_path: record.session_path.clone(),
            agent_id: record.identity.id.clone(),
            agent_path: record.identity.path.clone(),
            status: record.status.label().to_owned(),
        })
    }

    fn tools(self: &Arc<Self>, identity: &AgentIdentity) -> Vec<Arc<dyn Tool>> {
        CollaborationToolKind::ALL
            .into_iter()
            .map(|kind| {
                Arc::new(CollaborationTool {
                    manager: Arc::downgrade(self),
                    owner: identity.clone(),
                    kind,
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    fn prepare_owning_run(self: &Arc<Self>, owner: &AgentIdentity) -> Result<(), String> {
        if owner.id == ROOT_AGENT_ID {
            if owner.path != ROOT_AGENT_PATH || owner.depth != 0 {
                return Err("invalid root delegation identity".into());
            }
            // Session-scoped lifetime: a new owning run reattaches the fleet
            // that survived the previous turn instead of retiring it. Live
            // workers keep running; restored workers run undelivered tasks or
            // settle as interrupted for explicit continuation. Execution capacity
            // is *not* handed back here: a surviving worker keeps its slot, so
            // the cap cannot drift up on reattachment.
            {
                let _journal_order = self
                    .journal_order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let owns_lease = self.ensure_fleet_lease().is_ok();
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(error) = &state.persistence_error {
                    return Err(format!("delegation persistence is unavailable: {error}"));
                }
                if state.shutting_down {
                    return Err("delegation team is shutting down".into());
                }
                // A previous explicit stop (owner teardown, team stop, or a
                // dropped agent) reactivates for the next owning run so the
                // session can never be bricked by its own stop. Retired
                // records - shut-down workers with no live task and no
                // resumable work - release their name and slot here; the
                // journal keeps the shutdown evidence.
                state.root_active = true;
                // A worker parked by the previous session owner is reattachable
                // again at this new owning-run boundary.
                state.session_owner_released = false;
                let before = state.records.len();
                if owns_lease {
                    state.records.retain(|_, record| {
                        !matches!(record.status, DelegatedAgentStatus::Shutdown)
                            || record.detached_commands.is_some()
                    });
                }
                if state.records.len() != before {
                    state.total_agents = state
                        .total_agents
                        .saturating_sub(before - state.records.len())
                        .max(1);
                    self.persist_durable_fleet_locked(&mut state);
                }
            }
            self.reattach_detached(owner)?;
            return Ok(());
        }

        // Serialize the reset against journaled operations'
        // decide → append → commit windows so it cannot interleave with
        // their commit phases.
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        let descendants = state
            .records
            .iter()
            .filter(|(_, record)| is_descendant_path(&record.identity.path, &owner.path))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &descendants {
            if let Some(record) = state.records.get(id) {
                record.shutdown.cancel();
                let _ = record.command_tx.try_send(WorkerCommand::shutdown());
            }
        }
        for id in descendants {
            state.records.remove(&id);
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        Ok(())
    }

    fn current_permits(&self) -> Arc<Semaphore> {
        Arc::clone(
            &self
                .permits
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn spawn(
        self: &Arc<Self>,
        owner: &AgentIdentity,
        request: SpawnRequest,
    ) -> Result<Value, String> {
        let SpawnRequest {
            task_name,
            display_task_name,
            message,
            mut extension_policy,
            extension_provenance,
        } = request;
        validate_task_name(&task_name)?;
        if let Some(display_task_name) = display_task_name.as_deref() {
            validate_task_name(display_task_name)?;
        }
        validate_durable_text("spawn task", &message)?;
        if extension_provenance.is_some() != extension_policy.is_some() {
            return Err(
                "extension delegation policy and durable ownership provenance must be paired"
                    .into(),
            );
        }
        if let Some(provenance) = extension_provenance.as_ref() {
            if provenance.parent_session_id.trim().is_empty()
                || provenance.parent_session_id.len() > 256
                || provenance
                    .parent_session_id
                    .chars()
                    .any(char::is_whitespace)
            {
                return Err("invalid extension delegation parent session".into());
            }
            if provenance.principal.trim().is_empty() || provenance.principal.len() > 256 {
                return Err("invalid extension delegation principal".into());
            }
            ExtensionDelegationService::validate_resource_owner(&provenance.resource_owner)?;
        }
        let extension_requested_policy = extension_policy.clone();
        let resolved = self.template.resolve_model(extension_policy.as_ref())?;
        if extension_policy.is_some()
            && resolved.model.spec.pricing.is_none()
            && (extension_policy
                .as_ref()
                .is_some_and(|p| p.max_cost_microdollars.is_some())
                || self
                    .template
                    .runtime
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .max_session_cost_microdollars
                    .is_some())
        {
            return Err(
                "extension children with a cost ceiling require trusted model pricing".into(),
            );
        }
        if let Some(policy) = extension_policy.as_mut() {
            policy.resolved_model = Some(resolved.metadata.clone());
            policy.resolved_reasoning = Some(resolved.reasoning.clone());
        }
        if let Some(policy) = extension_policy.as_mut() {
            policy.validate()?;
            if owner.depth.saturating_add(1) > policy.max_depth {
                return Err(format!(
                    "extension child depth limit reached at {} (max depth {})",
                    owner.path, policy.max_depth
                ));
            }
            let allowed = policy.tools.iter().cloned().collect::<BTreeSet<_>>();
            let (_, effective_tools) = self.template.extensions.scoped_tool_snapshot(&allowed)?;
            policy.tools = effective_tools;
            if let Some(parent_turns) = self.template.max_turns {
                policy.max_turns = match policy.max_turns {
                    Some(requested) => Some(requested.min(parent_turns)),
                    None => Some(parent_turns),
                };
            }
            let runtime = self
                .template
                .runtime
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(parent_tokens) = runtime.max_session_tokens {
                policy.max_tokens = Some(
                    policy
                        .max_tokens
                        .map_or(parent_tokens, |requested| requested.min(parent_tokens)),
                );
            }
            if let Some(parent_cost) = runtime.max_session_cost_microdollars {
                policy.max_cost_microdollars = match policy.max_cost_microdollars {
                    Some(requested) => Some(requested.min(parent_cost)),
                    None => Some(parent_cost),
                };
            }
        }
        let orchestration_provenance = child_orchestration_provenance(extension_policy.as_ref());
        let effective_tool_policy = self
            .template
            .sandbox
            .effective_tool_policy(self.template.effect_broker.policy());
        let initial_task = message;
        let initial_delivery_id = new_delivery_id()?;
        if owner.depth >= self.config.limits.max_depth {
            return Err(format!(
                "delegation depth limit reached at {} (max depth {})",
                owner.path, self.config.limits.max_depth
            ));
        }
        let permit = self.current_permits().try_acquire_owned().map_err(|_| {
            "delegation concurrency limit reached; wait for an active agent".to_owned()
        })?;

        let created_at_ms = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
        let deadline = extension_policy.as_ref().and_then(|policy| {
            policy
                .timeout_ms
                .map(|timeout_ms| tokio::time::Instant::now() + Duration::from_millis(timeout_ms))
        });
        let deadline_at_ms = extension_policy
            .as_ref()
            .and_then(|policy| policy.timeout_ms)
            .map(|timeout_ms| created_at_ms.saturating_add(timeout_ms));

        let (identity, session, command_rx, shutdown, task_name) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable `sync_data`.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_owner_active_locked(&state, owner)?;
            if state.total_agents >= self.config.limits.max_total_agents {
                return Err(format!(
                    "delegation team agent limit reached ({})",
                    self.config.limits.max_total_agents
                ));
            }
            let child_path = format!("{}/{}", owner.path.trim_end_matches('/'), task_name);
            if let Some(existing) = state
                .records
                .values()
                .find(|record| record.identity.path == child_path)
            {
                // Worker names are session-scoped, not run-scoped: the name
                // stays owned by the surviving worker's durable record, so the
                // refusal names that worker and the tool that resumes it
                // instead of letting a second worker shadow it.
                return Err(existing_task_name_error(existing));
            }
            let number = state.next_agent_number;
            state.next_agent_number = state.next_agent_number.saturating_add(1);
            let identity = AgentIdentity {
                id: format!("agent-{number}"),
                path: child_path,
                depth: owner.depth + 1,
            };
            let session_path = self
                .team_directory
                .join(format!("{number:04}-{task_name}.jsonl"));
            // Create the isolated durable session before publishing the worker.
            let session_file = self
                .create_team_file(&session_path)
                .map_err(|error| error.to_string())?;
            let session = match Session::create_with_file(&session_path, session_file) {
                Ok(session) => session,
                Err(error) => {
                    let _ = self.remove_team_file_if_exists(&session_path);
                    return Err(error.to_string());
                }
            };
            let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
            let shutdown = crate::CancellationToken::default();
            let extension_session_reference = extension_provenance.as_ref().map(|_| {
                delegated_session_reference(&session_path)
                    .expect("generated extension child path has a delegated session reference")
            });
            let encoded = match serde_json::to_vec(&ProvenanceEvent::AgentSpawned {
                timestamp_ms: u128::from(created_at_ms),
                agent_id: &identity.id,
                agent_path: &identity.path,
                parent_id: &owner.id,
                task_name: &task_name,
                display_task_name: display_task_name.as_deref(),
                extension_parent_session_id: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.parent_session_id.as_str()),
                extension_principal: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.principal.as_str()),
                extension_resource_owner: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.resource_owner.as_str()),
                extension_profile: extension_provenance
                    .as_ref()
                    .and_then(|provenance| provenance.profile.as_deref()),
                extension_idempotency_key: extension_provenance
                    .as_ref()
                    .map(|provenance| provenance.idempotency_key.as_str()),
                extension_fingerprint: extension_provenance
                    .as_ref()
                    .and_then(|provenance| provenance.fingerprint.as_deref()),
                task: extension_provenance
                    .is_none()
                    .then_some(initial_task.as_str()),
                session: extension_provenance
                    .is_none()
                    .then_some(session_path.as_path()),
                session_reference: extension_session_reference.as_deref(),
                effective_tool_policy: &effective_tool_policy,
                orchestration_provenance: &orchestration_provenance,
            }) {
                Ok(encoded) => encoded,
                Err(error) => {
                    let message = format!("could not persist delegation provenance: {error}");
                    let error = io::Error::other(error);
                    self.fail_persistence_locked(&mut state, &error);
                    drop(state);
                    drop(command_tx);
                    drop(session);
                    let _ = self.remove_team_file_if_exists(&session_path);
                    return Err(message);
                }
            };
            drop(state);
            // Durable provenance sync runs without the state lock held.
            if let Err(error) = self.journal.append_encoded(&encoded) {
                let message = format!("could not persist delegation provenance: {error}");
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                drop(state);
                drop(command_tx);
                drop(session);
                let _ = self.remove_team_file_if_exists(&session_path);
                return Err(message);
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.persistence_error.is_some() || !state.root_active || state.shutting_down {
                // The team was invalidated mid-operation; discard the prepared
                // worker instead of resurrecting state after its provenance.
                let message = "delegation team is shutting down".to_owned();
                drop(state);
                drop(command_tx);
                drop(session);
                let _ = self.remove_team_file_if_exists(&session_path);
                return Err(message);
            }
            let turn_limit = extension_policy
                .as_ref()
                .and_then(|policy| policy.max_turns)
                .or(self.template.max_turns);
            state.records.insert(
                identity.id.clone(),
                AgentRecord {
                    identity: identity.clone(),
                    task_name: task_name.clone(),
                    display_task_name: display_task_name.clone(),
                    parent_id: owner.id.clone(),
                    session_path: session_path.clone(),
                    status: DelegatedAgentStatus::Pending,
                    command_tx: command_tx.clone(),
                    shutdown: shutdown.clone(),
                    interrupt_requested: false,
                    pending_messages: VecDeque::new(),
                    inflight_message_ids: BTreeSet::new(),
                    reserved_messages: QueueUsage::default(),
                    queued_follow_ups: QueueUsage::default(),
                    pending_follow_ups: VecDeque::new(),
                    pending_initial_task: Some(QueuedInitialTask {
                        task: initial_task.clone(),
                        delivery_id: initial_delivery_id,
                        attempts: 0,
                    }),
                    mailbox: VecDeque::new(),
                    mailbox_delivery: None,
                    resource_owner: None,
                    extension_policy: extension_policy.clone(),
                    effective_tool_policy: effective_tool_policy.clone(),
                    orchestration_provenance: orchestration_provenance.clone(),
                    extension_principal: extension_provenance
                        .as_ref()
                        .map(|provenance| provenance.principal.clone()),
                    extension_profile: extension_provenance
                        .as_ref()
                        .and_then(|provenance| provenance.profile.clone()),
                    extension_idempotency_key: extension_provenance
                        .as_ref()
                        .map(|provenance| provenance.idempotency_key.clone()),
                    extension_resource_owner: extension_provenance
                        .as_ref()
                        .map(|provenance| provenance.resource_owner.clone()),
                    extension_message_sha256: extension_provenance
                        .as_ref()
                        .map(|_| format!("{:x}", Sha256::digest(initial_task.as_bytes()))),
                    extension_requested_policy,
                    extension_fingerprint: extension_provenance
                        .as_ref()
                        .and_then(|provenance| provenance.fingerprint.clone()),
                    created_at_ms,
                    started_at_ms: None,
                    completed_at_ms: None,
                    turn_count: 0,
                    tool_call_count: 0,
                    active_tools: BTreeMap::new(),
                    recent_tools: VecDeque::new(),
                    usage: Usage::default(),
                    streamed_output_bytes: 0,
                    usage_uncertain: false,
                    usage_exposure: None,
                    cost: (extension_policy.is_some() && resolved.model.spec.pricing.is_some())
                        .then_some(Cost::default()),
                    cost_microdollars: (extension_policy.is_some()
                        && resolved.model.spec.pricing.is_some())
                    .then_some(0),
                    deadline_at_ms,
                    turn_limit,
                    detached: false,
                    live_task: true,
                    worker_generation: 1,
                    detached_commands: None,
                    durable_diagnostic: None,
                    claim: self.current_claim(),
                },
            );
            state.total_agents += 1;
            self.persist_durable_fleet_locked(&mut state);
            if let Some(error) = &state.persistence_error {
                return Err(format!("could not persist initial delegated task: {error}"));
            }
            (identity, session, command_rx, shutdown, task_name)
        };

        let result_policy = extension_policy.clone();
        let result_turn_limit = result_policy
            .as_ref()
            .and_then(|policy| policy.max_turns)
            .or(self.template.max_turns);
        self.spawn_worker(WorkerStartup {
            generation: 1,
            identity: identity.clone(),
            session,

            commands: command_rx,
            shutdown,
            initial_permit: permit,
            extension_policy,
            deadline,
            deadline_ms: deadline_at_ms,
        });
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);

        Ok(json!({
            "agent_id": identity.id,
            "agent_path": identity.path,
            "task_name": task_name,
            "profile": extension_provenance
                .as_ref()
                .and_then(|provenance| provenance.profile.as_deref()),
            "idempotency_key": extension_provenance
                .as_ref()
                .map(|provenance| provenance.idempotency_key.as_str()),
            "fingerprint": extension_provenance
                .as_ref()
                .and_then(|provenance| provenance.fingerprint.as_deref()),
            "status": "pending",
            "resolved_model": resolved_model_json(result_policy.as_ref()),
            "policy": public_policy_json(result_policy.as_ref()),
            "effective_tool_policy": effective_tool_policy,
            "orchestration_provenance": orchestration_provenance,
            "created_at_ms": created_at_ms,
            "started_at_ms": Value::Null,
            "completed_at_ms": Value::Null,
            "turn_limit": result_turn_limit,
            "deadline_at_ms": deadline_at_ms,
        }))
    }

    /// Keep a join handle for the actual worker future so a panic or external
    /// abort cannot strand its record in `Running`.
    fn spawn_worker(self: &Arc<Self>, startup: WorkerStartup) {
        let id = startup.identity.id.clone();
        // The incarnation was advanced atomically with publication of the
        // pending record: an old supervisor cannot settle its replacement even
        // in the interval before this new task is actually spawned.
        let generation = startup.generation;
        let claim = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(&id)
            .and_then(|record| record.claim.clone());
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let worker = Arc::clone(&supervisor);
            let result = tokio::spawn(async move { worker.run_worker(startup).await }).await;
            if let Err(error) = result {
                let cause = if error.is_panic() {
                    "worker task panicked"
                } else {
                    "worker task was aborted"
                };
                supervisor.mark_worker_aborted(&id, claim.as_ref(), generation, cause);
            }
        });
    }

    async fn run_worker(self: Arc<Self>, startup: WorkerStartup) {
        let WorkerStartup {
            generation,
            identity,
            session,
            mut commands,
            shutdown,
            initial_permit,
            extension_policy,
            mut deadline,
            mut deadline_ms,
        } = startup;
        let session_path = session.path().to_path_buf();
        let _liveness = WorkerLiveness::new(&self, identity.id.clone(), generation);
        let mut unopened_session = Some(session);
        let mut agent = None;
        let mut queued_tasks = self.restored_tasks(&identity.id);
        let mut initial_permit = Some(initial_permit);
        let mut retry_undelivered_task = false;
        let mut retry_pending_messages = 0;
        let mut timeout_settled = false;
        loop {
            // A follow-up accepted for a settled worker may re-anchor the
            // host-owned wall budget; adopt a newer deadline before the
            // local, spawn-frozen budget can end the resumed worker.
            self.adopt_host_deadline(&identity, &mut deadline, &mut deadline_ms);
            if deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
                if !timeout_settled {
                    if !self.set_status(&identity.id, DelegatedAgentStatus::TimedOut, true) {
                        return;
                    }
                    self.request_shutdown_descendants(&identity.id);
                    initial_permit.take();
                    queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                    timeout_settled = true;
                }
                // Keep the receiver and accepted follow-ups, but perform no
                // work until an explicit follow-up re-anchors the host budget.
            } else {
                timeout_settled = false;
            }
            if shutdown.is_cancelled() {
                if self.session_owner_released() {
                    self.park_released_worker(&identity.id, Some(commands));
                } else {
                    self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                }
                self.request_shutdown_descendants(&identity.id);
                return;
            }
            if self.interrupt_requested(&identity.id) {
                queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                initial_permit.take();
                let saw_shutdown =
                    self.drain_interrupted_commands(&identity.id, &mut commands, &mut queued_tasks);
                if saw_shutdown || shutdown.is_cancelled() {
                    if self.session_owner_released() {
                        self.park_released_worker(&identity.id, Some(commands));
                    } else {
                        self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    }
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
                self.set_status(&identity.id, DelegatedAgentStatus::Interrupted, true);
                self.request_shutdown_descendants(&identity.id);
                retry_undelivered_task = false;
                continue;
            }

            if queued_tasks.is_empty() {
                // An idle worker owns no execution slot. The fleet cap counts
                // running work, so a worker that reattached with nothing to do
                // releases its slot instead of parking on it; it acquires a new
                // one through `acquire_follow_up_permit` when work arrives.
                initial_permit.take();
            }
            if !queued_tasks.is_empty() && !retry_undelivered_task && !timeout_settled {
                if agent.is_none() {
                    let child_session = match unopened_session.take() {
                        Some(session) => Ok(session),
                        None => self.reopen_child_session(&session_path),
                    };
                    let build = child_session.and_then(|session| {
                        self.build_child_agent(session, &identity, extension_policy.as_ref())
                    });
                    match build {
                        Ok(child) => agent = Some(child),
                        Err(error) => {
                            // Initialization has not durably accepted the active
                            // task. Release its execution slot, preserve the task
                            // at the FIFO head, and retry only after explicit new
                            // work prevents a persistent failure hot loop.
                            initial_permit.take();
                            let task = queued_tasks.pop_front().expect("startup task exists");
                            let restored = restore_undelivered_task(
                                &mut queued_tasks,
                                task.clone(),
                                false,
                                &WorkerOutcome::Failed(error.to_string()),
                            );
                            self.persist_task_result(&identity.id, &task, &restored, false);
                            let diagnostic = match restored {
                                TaskRestore::DeadLettered { attempts } => format!(
                                    "delegated task was dead-lettered after {attempts} undelivered attempts: {error}"
                                ),
                                _ => format!(
                                    "delegated agent could not start; task retained for retry: {error}"
                                ),
                            };
                            self.fail_worker_start(&identity.id, bounded_text(&diagnostic));
                            self.request_shutdown_descendants(&identity.id);
                            retry_undelivered_task =
                                matches!(restored, TaskRestore::Restored { .. });
                            retry_pending_messages = self.pending_message_count(&identity.id);
                            continue;
                        }
                    }
                }
                let permit = if let Some(permit) = initial_permit.take() {
                    permit
                } else {
                    if !self.set_pending_if_needed(&identity.id) {
                        return;
                    }
                    match self
                        .acquire_follow_up_permit(&identity.id, &shutdown, deadline)
                        .await
                    {
                        PermitWait::Acquired(permit) => permit,
                        PermitWait::TimedOut => continue,
                        PermitWait::Interrupted => {
                            queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                            let saw_shutdown = self.drain_interrupted_commands(
                                &identity.id,
                                &mut commands,
                                &mut queued_tasks,
                            );
                            if saw_shutdown || shutdown.is_cancelled() {
                                if self.session_owner_released() {
                                    self.park_released_worker(&identity.id, Some(commands));
                                } else {
                                    self.set_status(
                                        &identity.id,
                                        DelegatedAgentStatus::Shutdown,
                                        true,
                                    );
                                }
                                self.request_shutdown_descendants(&identity.id);
                                return;
                            }
                            self.set_status(&identity.id, DelegatedAgentStatus::Interrupted, true);
                            self.request_shutdown_descendants(&identity.id);
                            continue;
                        }
                        PermitWait::Shutdown => {
                            if self.session_owner_released() {
                                self.park_released_worker(&identity.id, Some(commands));
                            } else {
                                self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                            }
                            self.request_shutdown_descendants(&identity.id);
                            return;
                        }
                    }
                };
                if shutdown.is_cancelled() {
                    drop(permit);
                    if self.session_owner_released() {
                        self.park_released_worker(&identity.id, Some(commands));
                    } else {
                        self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    }
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
                if !self.set_status(&identity.id, DelegatedAgentStatus::Running, true) {
                    return;
                }
                let task = queued_tasks
                    .pop_front()
                    .expect("checked delegated task queue is not empty");
                let active_follow_up_usage = match &task {
                    QueuedTask::Initial(_) => None,
                    QueuedTask::FollowUp(follow_up) => Some(follow_up.usage()),
                };
                let pending = self.take_pending_messages(&identity.id);
                let formatted_task = task.format(&pending);
                let execution = self
                    .execute_child_run(
                        agent.as_mut().expect("delegated agent initialized"),
                        formatted_task,
                        ChildRunContext {
                            queued_delivery_ids: queued_tasks
                                .iter()
                                .chain(std::iter::once(&task))
                                .map(|task| task.delivery_id().to_owned())
                                .collect(),
                            identity: &identity,
                            commands: &mut commands,
                            shutdown: &shutdown,
                            extension_policy: extension_policy.as_ref(),
                            deadline,
                        },
                    )
                    .await;
                self.update_agent_session_accounting(
                    &identity.id,
                    agent
                        .as_ref()
                        .expect("delegated agent remains initialized")
                        .session(),
                    agent
                        .as_ref()
                        .expect("initialized child")
                        .model()
                        .spec
                        .pricing
                        .is_some(),
                );
                drop(permit);

                let WorkerExecution {
                    mut outcome,
                    deferred_follow_ups,
                    acknowledged_follow_ups,
                    task_delivered,
                } = execution;
                // An owning terminal signal wins a race with a child terminal
                // event that was already dequeued but not yet recorded.
                if shutdown.is_cancelled() {
                    outcome = WorkerOutcome::Shutdown;
                } else if !matches!(&outcome, WorkerOutcome::Shutdown)
                    && self.interrupt_requested(&identity.id)
                {
                    outcome = WorkerOutcome::Interrupted;
                }
                let mut delivered_follow_ups = acknowledged_follow_ups;
                if task_delivered {
                    self.release_prompt_message_reservations(&identity.id, &pending);
                } else if !matches!(&outcome, WorkerOutcome::Shutdown) {
                    self.restore_pending_messages(&identity.id, pending);
                }
                if task_delivered {
                    if let Some(usage) = active_follow_up_usage {
                        delivered_follow_ups.add_usage(usage);
                    }
                }
                let task_restore = restore_undelivered_task(
                    &mut queued_tasks,
                    task.clone(),
                    task_delivered,
                    &outcome,
                );
                self.persist_task_result(&identity.id, &task, &task_restore, task_delivered);
                let task_restored = matches!(task_restore, TaskRestore::Restored { .. });
                match task_restore {
                    TaskRestore::Restored { .. } => {}
                    TaskRestore::DeadLettered { attempts } => {
                        outcome = WorkerOutcome::Failed(format!(
                            "delegated task was dead-lettered after {attempts} undelivered attempts"
                        ));
                    }
                    TaskRestore::NotRestored => {}
                }
                self.release_follow_up_usage(&identity.id, delivered_follow_ups);
                let retained_pending_messages = self.pending_message_count(&identity.id);

                match outcome {
                    WorkerOutcome::Shutdown => {
                        if self.session_owner_released() {
                            self.park_released_worker(&identity.id, Some(commands));
                        } else {
                            self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                        }
                        self.request_shutdown_descendants(&identity.id);
                        return;
                    }
                    WorkerOutcome::TimedOut => {
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::FollowUp));
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                    WorkerOutcome::Interrupted => {
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        let saw_shutdown = self.drain_interrupted_commands(
                            &identity.id,
                            &mut commands,
                            &mut queued_tasks,
                        );
                        if saw_shutdown || shutdown.is_cancelled() {
                            if self.session_owner_released() {
                                self.park_released_worker(&identity.id, Some(commands));
                            } else {
                                self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                            }
                            self.request_shutdown_descendants(&identity.id);
                            return;
                        }
                        self.set_status(&identity.id, DelegatedAgentStatus::Interrupted, true);
                        self.request_shutdown_descendants(&identity.id);
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                    WorkerOutcome::LimitReached {
                        output,
                        turn_count,
                        turn_limit,
                    } => {
                        self.set_status(
                            &identity.id,
                            DelegatedAgentStatus::LimitReached {
                                output: bounded_text(&output),
                                turn_count,
                                turn_limit,
                            },
                            true,
                        );
                        self.request_shutdown_descendants(&identity.id);
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        // A max-turn run is a durable terminal completion, not
                        // a retryable task failure. Follow-ups may explicitly
                        // resume the same child session.
                        retry_undelivered_task = task_restored;
                        retry_pending_messages = retained_pending_messages;
                    }
                    WorkerOutcome::Failed(error) => {
                        if self.worker_is_detached(&identity.id)
                            && is_missing_approval_authority(&error)
                        {
                            // Unattended mutation fails closed. A detached
                            // worker that needs a decision it no longer has
                            // authority for parks in a bounded, durable state:
                            // it neither proceeds nor blocks forever. A later
                            // turn that reattaches can supply the decision.
                            self.set_status(
                                &identity.id,
                                DelegatedAgentStatus::AwaitingApproval {
                                    reason: bounded_text(&error),
                                },
                                true,
                            );
                            self.request_shutdown_descendants(&identity.id);
                            return;
                        }
                        self.set_status(
                            &identity.id,
                            DelegatedAgentStatus::Failed {
                                error: bounded_text(&error),
                            },
                            true,
                        );
                        self.request_shutdown_descendants(&identity.id);
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        // A pre-flight prompt failure did not durably accept the
                        // active task. Keep it at the FIFO head, but wait for an
                        // explicit message or follow-up instead of hot-looping
                        // on a persistent storage failure.
                        retry_undelivered_task = task_restored;
                        retry_pending_messages = retained_pending_messages;
                    }
                    WorkerOutcome::Completed(output) => {
                        self.set_status(
                            &identity.id,
                            DelegatedAgentStatus::Completed {
                                output: bounded_text(&output),
                            },
                            true,
                        );
                        queued_tasks
                            .extend(deferred_follow_ups.into_iter().map(QueuedTask::follow_up));
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                }
                continue;
            }

            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.interrupt_requested(&identity.id) {
                continue;
            }
            let command = tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    // The receiver is still borrowed by this select; the parked
                    // record rebuilds its command channel when it is reattached.
                    if self.session_owner_released() {
                        self.park_released_worker(&identity.id, None);
                    } else {
                        self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    }
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if deadline.is_some() && !timeout_settled => {
                    // Re-check the authoritative deadline and settle once at
                    // the loop boundary; keep the idle receiver resumable.
                    continue;
                }
                _ = &mut notified => {
                    if retry_undelivered_task
                        && self.pending_message_count(&identity.id) > retry_pending_messages
                    {
                        retry_undelivered_task = false;
                        retry_pending_messages = 0;
                    }
                    continue;
                },
                command = commands.recv() => command,
            };
            let Some(command) = command else {
                self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, false);
                self.request_shutdown_descendants(&identity.id);
                return;
            };
            match command.kind {
                WorkerCommandKind::Message(message) => {
                    self.queue_reserved_message(&identity.id, message);
                    retry_undelivered_task = false;
                    retry_pending_messages = 0;
                }
                WorkerCommandKind::FollowUp => {
                    let pending = self.restored_tasks(&identity.id);
                    // A wakeup can outlive queue seeding or delivery. It is not
                    // a second acceptance and must not trigger another retry.
                    let new_work = pending.iter().any(|pending| {
                        !queued_tasks
                            .iter()
                            .any(|queued| queued.delivery_id() == pending.delivery_id())
                    });
                    queued_tasks = pending;
                    if !new_work {
                        continue;
                    }
                    retry_undelivered_task = false;
                    retry_pending_messages = 0;
                }
                WorkerCommandKind::Shutdown => {
                    self.set_status(&identity.id, DelegatedAgentStatus::Shutdown, true);
                    self.request_shutdown_descendants(&identity.id);
                    return;
                }
            }
        }
    }

    /// A follow-up accepted for a settled worker starts a new run. When the
    /// original wall budget already elapsed, the manager re-anchors the
    /// record deadline; the worker's own deadline was frozen at spawn, so
    /// adopt the newer host-owned value before the local budget can end the
    /// resumed worker. Restored workers also initialize an absent local budget
    /// from that same durable absolute deadline.
    fn adopt_host_deadline(
        &self,
        identity: &AgentIdentity,
        deadline: &mut Option<tokio::time::Instant>,
        deadline_ms: &mut Option<u64>,
    ) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get(&identity.id) else {
            return;
        };
        let Some(host) = record.deadline_at_ms else {
            return;
        };
        if deadline.is_none() || deadline_ms.is_none_or(|local| host > local) {
            *deadline = Some(wall_deadline_instant(host));
            *deadline_ms = Some(host);
        }
    }

    fn build_child_agent(
        self: &Arc<Self>,
        mut session: Session,
        identity: &AgentIdentity,
        extension_policy: Option<&ExtensionAgentSessionPolicy>,
    ) -> Result<Agent, DelegationError> {
        let resolved = self
            .template
            .resolve_model(extension_policy)
            .map_err(DelegationError::InvalidConfig)?;
        let parent_path = identity
            .path
            .rsplit_once('/')
            .map(|(parent, _)| {
                if parent.is_empty() {
                    ROOT_AGENT_PATH
                } else {
                    parent
                }
            })
            .unwrap_or(ROOT_AGENT_PATH);
        let base_system = self
            .template
            .base_system
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let (system, extensions, max_turns) = if let Some(policy) = extension_policy {
            let allowed = policy.tools.iter().cloned().collect::<BTreeSet<_>>();
            let (extensions, effective) = self
                .template
                .extensions
                .scoped_tool_snapshot(&allowed)
                .map_err(DelegationError::InvalidConfig)?;
            if effective != policy.tools {
                return Err(DelegationError::InvalidConfig(
                    "effective extension child tool scope changed after spawn admission".into(),
                ));
            }
            let scope_note = if policy
                .tools
                .iter()
                .all(|tool| matches!(tool.as_str(), "read" | "search"))
            {
                "Only the listed read/search tools are installed; mutation, shell, and collaboration tools are unavailable."
            } else {
                "The listed standard file and shell tools are installed for this task; collaboration tools are unavailable. Mutating tools remain subject to the inherited approval policy; host policy is authoritative."
            };
            (
                format!(
                    "{base_system}\n\nThis is a host-enforced depth-one child session. {scope_note}"
                ),
                extensions,
                policy.max_turns,
            )
        } else {
            (
                format!(
                    "{}\n\n{}",
                    base_system,
                    child_instructions(identity, parent_path, &self.config.limits)
                ),
                self.template.extensions.clone(),
                self.template.max_turns,
            )
        };
        let runtime = self
            .template
            .runtime
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if extension_policy.is_some() {
            let mut cursor = session.head_ref();
            let mut matches_config = false;
            while let Some(id) = cursor {
                let entry = session.entry(id).expect("session branch entries exist");
                if let crate::session::EntryValue::Config {
                    model, reasoning, ..
                } = &entry.value
                {
                    matches_config = model.as_deref() == Some(&resolved.metadata.model)
                        && reasoning.as_deref() == Some(&resolved.metadata.reasoning);
                    break;
                }
                cursor = entry.parent.as_ref();
            }
            if !matches_config {
                session.append(crate::session::EntryValue::Config {
                    model: Some(resolved.metadata.model.clone()),
                    reasoning: Some(resolved.metadata.reasoning.clone()),
                    reasoning_mode: Some(
                        match self.template.reasoning_mode {
                            octet_ai::ReasoningMode::Standard => "standard",
                            octet_ai::ReasoningMode::Pro => "pro",
                        }
                        .into(),
                    ),
                })?;
            }
        }
        let mut agent = Agent::new(AgentConfig {
            client: self.template.client.clone(),
            model: resolved.model.clone(),
            session,
            system,
            sandbox: self.template.sandbox.clone(),
            effect_broker: self.template.effect_broker.clone(),
            extensions,
            max_turns,
            reasoning: resolved.reasoning.clone(),
            reasoning_mode: self.template.reasoning_mode,
            cache_retention: self.template.cache_retention,
            session_id: None,
        })?;
        if extension_policy.is_some() {
            agent.mark_ultra_observation_managed();
        }
        if let Some(record) = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get_mut(&identity.id)
        {
            record.resource_owner = Some(agent.resource_owner_id().to_owned());
        }
        agent.set_compaction_model(runtime.compaction_model);
        agent.set_compaction_token_mode(
            runtime.auto_compaction_mode,
            runtime.auto_compaction_threshold,
            runtime.compaction_keep_recent_tokens,
        )?;
        agent.set_completion_policy(runtime.completion_policy);
        agent.set_output_modalities(runtime.output_modalities);
        agent.set_tool_schema_budget_bytes(runtime.tool_schema_budget_bytes);
        if let Some(policy) = extension_policy {
            agent.inherit_max_output_tokens(
                runtime
                    .max_output_tokens
                    .min(resolved.model.spec.limits.max_output_tokens),
            );
            let max_session_tokens = match (runtime.max_session_tokens, policy.max_tokens) {
                (Some(parent), Some(requested)) => Some(parent.min(requested)),
                (Some(parent), None) => Some(parent),
                (None, requested) => requested,
            };
            let max_session_cost_microdollars = match (
                runtime.max_session_cost_microdollars,
                policy.max_cost_microdollars,
            ) {
                (Some(parent), Some(requested)) => Some(parent.min(requested)),
                (Some(parent), None) => Some(parent),
                (None, requested) => requested,
            };
            agent.set_max_session_tokens(max_session_tokens);
            agent.set_max_session_cost_microdollars(max_session_cost_microdollars);
        } else {
            agent.inherit_max_output_tokens(
                runtime
                    .max_output_tokens
                    .min(resolved.model.spec.limits.max_output_tokens),
            );
            agent.set_max_session_tokens(runtime.max_session_tokens);
            agent.set_max_session_cost_microdollars(runtime.max_session_cost_microdollars);
        }
        agent.set_provider_retries_enabled(runtime.provider_retries_enabled);
        agent.set_max_network_wait(runtime.max_network_wait);
        if extension_policy.is_none() {
            let binding = DelegationBinding {
                manager: Arc::clone(self),
                identity: identity.clone(),
                system_instructions: Arc::from(""),
            };
            agent.install_delegation_tools(self.tools(identity));
            agent.set_delegation_binding(binding)?;
        }
        agent.finalize_tool_surface();
        Ok(agent)
    }

    async fn execute_child_run(
        self: &Arc<Self>,
        agent: &mut Agent,
        task: String,
        context: ChildRunContext<'_>,
    ) -> WorkerExecution {
        let ChildRunContext {
            mut queued_delivery_ids,
            identity,
            commands,
            shutdown,
            extension_policy,
            deadline,
        } = context;
        if deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
            return WorkerExecution::new(WorkerOutcome::TimedOut);
        }
        if shutdown.is_cancelled() {
            return WorkerExecution::new(WorkerOutcome::Shutdown);
        }
        if self.interrupt_requested(&identity.id) {
            return WorkerExecution::new(WorkerOutcome::Interrupted);
        }
        // Row 3.5: one delegation boundary per driven child run. The child
        // agent observes with the span's derived context, so every span it
        // records nests under `octet.agent.delegation`.
        let delegation_guard = self
            .span_context()
            .begin_typed::<DelegationSpan>(EmptyAttributes {});
        agent.set_telemetry_context(delegation_guard.context());
        let entries_before_prompt = agent.session().entries().len();
        let session_path = agent.session().path().to_path_buf();
        // Bind prompt-error inspection to the exact session object already
        // owned by the child. Reopening the path after `prompt` would allow a
        // directory-entry replacement to misclassify durable task delivery.
        let inspection_file = match agent.session().try_clone_file() {
            Ok(file) => file,
            Err(error) => {
                return WorkerExecution::new(WorkerOutcome::Failed(format!(
                    "delegated task could not start because its session descriptor could not be cloned; task retained for retry: {error}"
                )));
            }
        };
        let persisted_task = task.clone();
        let prompt = agent.prompt(task).await;
        let mut run = match prompt {
            Ok(run) => run,
            Err(error) => {
                // `Run` borrows the agent on success, so inspect a clone of the
                // already-authorized session descriptor on this error path.
                let task_delivered =
                    Session::open_read_only_with_file(&session_path, inspection_file)
                        .ok()
                        .map(|session| {
                            session
                                .entries()
                                .iter()
                                .skip(entries_before_prompt)
                                .any(|entry| {
                                    matches!(
                                        &entry.value,
                                        crate::session::EntryValue::Message(
                                            octet_ai::Message::User(message)
                                        ) if message.content.len() == 1
                                            && matches!(
                                                &message.content[0],
                                                octet_ai::UserPart::Text(text)
                                                    if text == &persisted_task
                                            )
                                    )
                                })
                        })
                        .unwrap_or(false);
                let diagnostic = if task_delivered {
                    format!(
                        "delegated run could not start after the task was durably accepted: {error}"
                    )
                } else {
                    format!(
                        "delegated prompt was not durably accepted; task retained for retry: {error}"
                    )
                };
                let mut execution = WorkerExecution::new(WorkerOutcome::Failed(diagnostic));
                execution.task_delivered = task_delivered;
                return execution;
            }
        };
        let control = run.control();

        let output_limit = extension_policy
            .map(|policy| policy.max_output_bytes)
            .unwrap_or(MAX_PROVENANCE_TEXT_BYTES);
        let mut output = String::new();
        // A successful nonblocking control enqueue is not delivery: the agent
        // acknowledges only after the input has been appended durably. Keep the
        // original work and its queue reservation until that event so a run
        // failure cannot silently discard accepted steering or follow-ups.
        let mut submitted_messages = VecDeque::new();
        let mut deferred_messages = VecDeque::new();
        let mut defer_messages = false;
        let mut submitted_follow_ups = VecDeque::new();
        let mut deferred_follow_ups = VecDeque::new();
        let mut defer_follow_ups = queued_delivery_ids.len() > 1;
        let mut acknowledged_follow_ups = QueueUsage::default();
        let mut requested_interrupt = false;
        let mut requested_shutdown = false;
        let mut requested_timeout = false;
        let mut requested_token_limit = false;
        let turn_limit = extension_policy
            .and_then(|policy| policy.max_turns)
            .or(self.template.max_turns);
        let mut turns_completed = 0_u64;
        let mut last_stream_update = None;
        let mut commands_open = true;
        enum Next {
            Event,
            Command(Option<WorkerCommand>),
            Changed,
            Shutdown,
            Deadline,
        }
        let outcome = loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !requested_interrupt && self.interrupt_requested(&identity.id) {
                requested_interrupt = true;
                control.abort();
            }
            // Keep the large event outside the small control discriminator.
            let mut next_event = None;
            let next = tokio::select! {
                biased;
                _ = shutdown.cancelled(), if !requested_shutdown => Next::Shutdown,
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if deadline.is_some() && !requested_timeout => Next::Deadline,
                _ = &mut notified, if !requested_interrupt => Next::Changed,
                event = run.next() => {
                    next_event = event;
                    Next::Event
                },
                command = commands.recv(), if commands_open => Next::Command(command),
            };
            match next {
                Next::Changed => {}
                Next::Deadline => {
                    control.abort();
                    requested_timeout = true;
                }
                Next::Shutdown => {
                    control.abort();
                    requested_shutdown = true;
                }
                Next::Command(None) => {
                    commands_open = false;
                    control.abort();
                    requested_shutdown = true;
                }
                Next::Command(Some(command)) => match command.kind {
                    WorkerCommandKind::Message(message) => {
                        if defer_messages
                            || requested_interrupt
                            || requested_shutdown
                            || control.try_steer(format_direct_message(&message)).is_err()
                        {
                            // Once one message cannot enter the active run, keep
                            // later messages behind it to preserve FIFO order.
                            defer_messages = true;
                            deferred_messages.push_back(message);
                        } else {
                            submitted_messages.push_back(message);
                        }
                    }
                    WorkerCommandKind::FollowUp => {
                        for task in self.restored_tasks(&identity.id) {
                            let QueuedTask::FollowUp(follow_up) = task else {
                                continue;
                            };
                            if !queued_delivery_ids.insert(follow_up.delivery_id.clone()) {
                                continue;
                            }
                            if defer_follow_ups
                                || requested_interrupt
                                || requested_shutdown
                                || control
                                    .try_follow_up(format_follow_up(&follow_up, &[]))
                                    .is_err()
                            {
                                // Preserve FIFO across the active run-control queue
                                // and the worker's deferred queue.
                                defer_follow_ups = true;
                                deferred_follow_ups.push_back(follow_up);
                            } else {
                                submitted_follow_ups.push_back(follow_up);
                            }
                        }
                    }
                    WorkerCommandKind::Shutdown => {
                        requested_shutdown = true;
                        control.abort();
                    }
                },
                Next::Event => match next_event {
                    None => {
                        break if requested_shutdown {
                            WorkerOutcome::Shutdown
                        } else if requested_timeout {
                            WorkerOutcome::TimedOut
                        } else if requested_token_limit {
                            WorkerOutcome::Failed("maximum delegated token budget reached".into())
                        } else if requested_interrupt {
                            WorkerOutcome::Interrupted
                        } else {
                            WorkerOutcome::Failed(
                                "delegated run ended without a terminal event".into(),
                            )
                        };
                    }
                    Some(AgentEvent::SteeringDelivered { messages }) => {
                        for _ in 0..messages.len() {
                            let Some(message) = submitted_messages.pop_front() else {
                                debug_assert!(
                                    false,
                                    "steering acknowledgement exceeded submissions"
                                );
                                break;
                            };
                            self.release_message_reservation(&identity.id, &message);
                        }
                    }
                    Some(AgentEvent::FollowUpDelivered { messages }) => {
                        for _ in 0..messages.len() {
                            let Some(follow_up) = submitted_follow_ups.pop_front() else {
                                debug_assert!(
                                    false,
                                    "follow-up acknowledgement exceeded submissions"
                                );
                                break;
                            };
                            self.acknowledge_follow_up_delivery(&identity.id, &follow_up);
                            acknowledged_follow_ups.add_usage(follow_up.usage());
                        }
                    }
                    Some(AgentEvent::ToolStarted { id, name, args }) => {
                        let args_summary = tool_args_summary(&args);
                        self.update_agent_tool_started(&identity.id, &id.0, name, args_summary);
                    }
                    Some(AgentEvent::ToolFinished { id, result, .. }) => {
                        let is_error = match &result {
                            Err(_) => true,
                            Ok(output) => output.is_error(),
                        };
                        self.update_agent_tool_finished(&identity.id, &id.0, is_error);
                    }
                    Some(AgentEvent::OutputDelta { text, .. }) => {
                        self.update_agent_streamed_output(
                            &identity.id,
                            text.len(),
                            &mut last_stream_update,
                        );
                    }
                    Some(AgentEvent::ProviderRetry { .. }) => {
                        self.clear_agent_streamed_output(&identity.id);
                        last_stream_update = None;
                    }
                    Some(AgentEvent::ProviderUsageUncertain) => {
                        self.mark_agent_usage_uncertain(&identity.id);
                    }
                    Some(AgentEvent::CandidateRejected {
                        usage,
                        session_cost_microdollars,
                        ..
                    }) => {
                        self.update_agent_usage(
                            &identity.id,
                            usage,
                            session_cost_microdollars,
                            false,
                        );
                        last_stream_update = None;
                    }
                    Some(AgentEvent::TurnFinished {
                        message,
                        usage,
                        session_cost_microdollars,
                        ..
                    }) => {
                        self.update_agent_usage(
                            &identity.id,
                            usage,
                            session_cost_microdollars,
                            true,
                        );
                        last_stream_update = None;
                        turns_completed = turns_completed.saturating_add(1);
                        if extension_policy.is_some_and(|policy| {
                            policy
                                .max_tokens
                                .is_some_and(|limit| delegation_usage_tokens(&usage) > limit)
                        }) {
                            control.abort();
                            requested_token_limit = true;
                        }
                        for part in message.content {
                            if let AssistantPart::Text(text) = part {
                                if !output.is_empty() {
                                    output.push('\n');
                                }
                                output.push_str(&text);
                                if output.len() > output_limit {
                                    output = bounded_text_to(&output, output_limit);
                                }
                            }
                        }
                    }
                    Some(AgentEvent::RunFinished { reason, .. }) => {
                        break if requested_shutdown {
                            WorkerOutcome::Shutdown
                        } else if requested_timeout {
                            WorkerOutcome::TimedOut
                        } else if requested_token_limit {
                            WorkerOutcome::Failed("maximum delegated token budget reached".into())
                        } else if requested_interrupt {
                            WorkerOutcome::Interrupted
                        } else {
                            match reason {
                                FinishReason::Completed => WorkerOutcome::Completed(output),
                                FinishReason::Aborted => WorkerOutcome::Interrupted,
                                FinishReason::Failed(error) => {
                                    WorkerOutcome::Failed(error.to_string())
                                }
                                FinishReason::MaxTurns => WorkerOutcome::LimitReached {
                                    output,
                                    turn_count: turns_completed,
                                    turn_limit: turn_limit.expect(
                                        "MaxTurns requires a configured delegated turn limit",
                                    ),
                                },
                            }
                        };
                    }
                    Some(_) => {}
                },
            }
        };

        submitted_messages.extend(deferred_messages);
        for message in submitted_messages {
            self.queue_reserved_message(&identity.id, message);
        }
        // Any unacknowledged control submission is older than work deferred
        // after the control queue filled, so prepend it to retain acceptance
        // order for the next child run.
        submitted_follow_ups.extend(deferred_follow_ups);
        // Row 3.5: settle the child span at its single driven outcome.
        delegation_guard.finish(matches!(
            outcome,
            WorkerOutcome::Failed(_) | WorkerOutcome::TimedOut
        ));
        WorkerExecution {
            outcome,
            deferred_follow_ups: submitted_follow_ups,
            acknowledged_follow_ups,
            task_delivered: true,
        }
    }

    fn update_agent_tool_started(
        &self,
        id: &str,
        call_id: &str,
        name: String,
        args_summary: String,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.tool_call_count = record.tool_call_count.saturating_add(1);
        record.active_tools.insert(call_id.to_owned(), name.clone());
        record.recent_tools.push_back(ChildToolActivity {
            name,
            args_summary,
            started_at_ms: timestamp_ms() as u64,
            finished_at_ms: None,
            error: false,
            call_id: call_id.to_owned(),
        });
        while record.recent_tools.len() > MAX_CHILD_TOOL_ACTIVITY {
            record.recent_tools.pop_front();
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    fn update_agent_tool_finished(&self, id: &str, call_id: &str, is_error: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.active_tools.remove(call_id);
        if let Some(entry) = record
            .recent_tools
            .iter_mut()
            .rev()
            .find(|entry| entry.call_id == call_id && entry.finished_at_ms.is_none())
        {
            entry.finished_at_ms = Some(timestamp_ms() as u64);
            entry.error = is_error;
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    fn mark_agent_usage_uncertain(&self, id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.usage_uncertain = true;
        record.usage_exposure = None;
        record.cost_microdollars = None;
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    fn update_agent_streamed_output(
        &self,
        id: &str,
        bytes: usize,
        last_update: &mut Option<Instant>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        let previous = record.streamed_output_bytes.div_ceil(4);
        record.streamed_output_bytes = record.streamed_output_bytes.saturating_add(bytes as u64);
        let changed = record.streamed_output_bytes.div_ceil(4) != previous;
        drop(state);
        let now = Instant::now();
        if changed
            && last_update
                .is_none_or(|last| now.duration_since(last) >= STREAMED_OUTPUT_UPDATE_INTERVAL)
        {
            *last_update = Some(now);
            self.publish_telemetry(None, None);
        }
    }

    fn clear_agent_streamed_output(&self, id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        let changed = record.streamed_output_bytes != 0;
        record.streamed_output_bytes = 0;
        drop(state);
        if changed {
            self.publish_telemetry(None, None);
        }
    }

    fn update_agent_usage(
        &self,
        id: &str,
        usage: Usage,
        cost_microdollars: Option<u64>,
        completed_turn: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        if completed_turn {
            record.turn_count = record.turn_count.saturating_add(1);
        }
        record.usage = usage;
        record.streamed_output_bytes = 0;
        if !record.usage_uncertain && cost_microdollars.is_some() {
            record.cost_microdollars = cost_microdollars;
        }
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    fn update_agent_session_accounting(&self, id: &str, session: &Session, priced: bool) {
        let mut usage = Usage::default();
        let mut aggregate_cost = priced.then_some(Cost::default());
        for record in session.usage_records() {
            add_delegated_usage(&mut usage, &record.usage);
            match (aggregate_cost.as_mut(), record.cost) {
                (Some(total), Some(cost)) => add_delegated_cost(total, cost),
                (Some(_), None) => aggregate_cost = None,
                (None, _) => {}
            }
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.usage = usage;
        record.streamed_output_bytes = 0;
        record.cost = aggregate_cost;
        record.usage_uncertain = session.has_uncertain_usage();
        record.usage_exposure = record
            .usage_uncertain
            .then(|| session.usage_uncertainty_exposure())
            .flatten();
        record.cost_microdollars = if record.usage_uncertain {
            None
        } else {
            aggregate_cost.map(|cost| cost.total)
        };
        record.active_tools.clear();
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    /// Marks a worker task dead the moment `run_worker` returns, whatever the
    /// exit path was. A session-owned record without a live task is what a
    /// launchable interactive handle requires.
    fn mark_worker_stopped(&self, id: &str, generation: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(id) {
            if record.worker_generation == generation {
                record.live_task = false;
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }

    /// The join supervisor alone calls this after an abnormal task exit. A
    /// normal worker has already committed a terminal status, so this cannot
    /// create a duplicate terminal notification.
    fn mark_worker_aborted(
        &self,
        id: &str,
        claim: Option<&DurableFleetClaim>,
        generation: u64,
        cause: &str,
    ) {
        // Claim, running-state check, and terminal mutation share this one
        // lock. An old supervisor therefore cannot settle a newer reattachment.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.persistence_error.is_some() {
            return;
        }
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        if record.claim.as_ref() != claim
            || record.worker_generation != generation
            || !record.status.is_running()
        {
            return;
        }
        record.live_task = false;
        // The crashed receiver is gone; an explicit follow-up must reopen this
        // session rather than publishing into the dead process-local channel.
        // Its delivery attempts are gone too, but their durable payloads remain
        // available for the replacement worker after session reconciliation.
        record.inflight_message_ids.clear();
        record.reserved_messages = QueueUsage::default();
        record.detached = true;
        record.status = DelegatedAgentStatus::Failed {
            error: bounded_text(&format!("delegated {cause}; worker settled by supervisor")),
        };
        record.completed_at_ms = Some(u64::try_from(timestamp_ms()).unwrap_or(u64::MAX));
        let parent_id = record.parent_id.clone();
        let message = MailboxMessage {
            kind: "task_status",
            from: id.to_owned(),
            task_name: Some(record.task_name.clone()),
            message: status_message(&record.identity.path, &record.status),
            evictable: true,
            continued: false,
            leased: false,
        };
        push_mailbox_locked(&mut state, &parent_id, message);
        self.persist_durable_fleet_locked(&mut state);
        drop(state);
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
    }

    fn worker_is_detached(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(id)
            .is_some_and(|record| record.detached)
    }

    /// Parks a live worker whose owning session disappeared.
    ///
    /// The worker stops, but its durable child session and accounting are kept.
    /// Unsettled work becomes [`DelegatedAgentStatus::Detached`]; settled
    /// outcomes retain their terminal evidence independently of attachment.
    /// Returns `false` when the owning session is still attached, so the caller
    /// keeps its normal terminal settlement.
    fn park_released_worker(
        &self,
        id: &str,
        commands: Option<mpsc::Receiver<WorkerCommand>>,
    ) -> bool {
        if !self.session_owner_released() {
            return false;
        }
        let diagnostic = bounded_text(
            "the owning session was released while this worker was live; its durable session, task, and accounting are retained for reattachment by the next session owner",
        );
        // Same decide → append → commit ordering as every other durable status
        // transition, so the parked state and its lifecycle notification agree.
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let status = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = state.records.get(id) else {
                return false;
            };
            if record.status.is_running() || matches!(record.status, DelegatedAgentStatus::Idle) {
                DelegatedAgentStatus::Detached
            } else {
                // Attachment is independent of the settled task outcome.
                // Retain its output/error and completion time after teardown.
                record.status.clone()
            }
        };
        let encoded = serde_json::to_vec(&ProvenanceEvent::AgentStatus {
            timestamp_ms: timestamp_ms(),
            agent_id: id,
            status: &status,
        });
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                // `fail_persistence_locked` reports an io failure (the journal's
                // error type). A detached-status event that cannot be encoded is
                // the same class of problem for the caller: persistence is
                // broken, so surface it through the one path rather than a second
                // reporting channel.
                let error = io::Error::new(io::ErrorKind::InvalidData, error);
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return true;
            }
        };
        if let Err(error) = self.journal.append_encoded(&encoded) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.fail_persistence_locked(&mut state, &error);
            return true;
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(record) = state.records.get_mut(id) {
                record.status = status;
                record.detached = true;
                record.live_task = false;
                record
                    .completed_at_ms
                    .get_or_insert_with(|| u64::try_from(timestamp_ms()).unwrap_or(u64::MAX));
                record.durable_diagnostic.get_or_insert(diagnostic);
                if let Some(commands) = commands {
                    record.detached_commands = Some(commands);
                }
            }
            self.persist_durable_fleet_locked(&mut state);
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        true
    }

    fn set_status(&self, id: &str, status: DelegatedAgentStatus, notify_parent: bool) -> bool {
        // The journal_order guard makes this operation's decide → append →
        // commit window mutually exclusive with every other provenance-
        // journaled operation, so journal record order always matches state
        // mutation order. The state lock is dropped across the journal's
        // durable `sync_data` so unrelated state operations are not stalled
        // by disk latency.
        let _journal_order = self
            .journal_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.persistence_error.is_some() || !state.records.contains_key(id) {
                return false;
            }
            let record = state.records.get(id).expect("checked child exists");
            if matches!(&status, DelegatedAgentStatus::Shutdown) && !record.status.is_running() {
                return true;
            }
            if record.interrupt_requested
                && matches!(
                    status,
                    DelegatedAgentStatus::Pending | DelegatedAgentStatus::Running
                )
            {
                return true;
            }
            if record.status == status {
                if matches!(status, DelegatedAgentStatus::Interrupted) {
                    state
                        .records
                        .get_mut(id)
                        .expect("checked child exists")
                        .interrupt_requested = false;
                }
                return true;
            }
        }
        let transition_ms = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
        let encoded = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Re-check under the journal_order guard: a concurrent owning-run
            // reset or persistence failure may have invalidated this record
            // after the fast-path pass above.
            if state.persistence_error.is_some() || !state.records.contains_key(id) {
                return false;
            }
            serde_json::to_vec(&ProvenanceEvent::AgentStatus {
                timestamp_ms: u128::from(transition_ms),
                agent_id: id,
                status: &status,
            })
            .map_err(io::Error::other)
        };
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return false;
            }
        };
        if let Err(error) = self.journal.append_encoded(&encoded) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.fail_persistence_locked(&mut state, &error);
            return false;
        }
        let notification = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = state.records.get_mut(id) else {
                // An owning-run reset removed the record mid-operation; it
                // already owns its journal record and must not be resurrected.
                return true;
            };
            record.status = status;
            if matches!(
                record.status,
                DelegatedAgentStatus::Interrupted
                    | DelegatedAgentStatus::Shutdown
                    | DelegatedAgentStatus::TimedOut
            ) {
                record.pending_initial_task = None;
            }
            if matches!(record.status, DelegatedAgentStatus::Running)
                && record.started_at_ms.is_none()
            {
                record.started_at_ms = Some(transition_ms);
            }
            if !record.status.is_running() && record.completed_at_ms.is_none() {
                record.completed_at_ms = Some(transition_ms);
            }
            if matches!(record.status, DelegatedAgentStatus::Interrupted) {
                record.interrupt_requested = false;
            }
            if matches!(record.status, DelegatedAgentStatus::Shutdown) {
                record.pending_messages.clear();
                record.inflight_message_ids.clear();
                record.reserved_messages = QueueUsage::default();
                record.queued_follow_ups = QueueUsage::default();
            }
            (notify_parent && !record.status.is_running()).then(|| {
                (
                    record.parent_id.clone(),
                    MailboxMessage {
                        kind: "task_status",
                        from: id.to_owned(),
                        task_name: Some(record.task_name.clone()),
                        message: status_message(&record.identity.path, &record.status),
                        evictable: true,
                        continued: false,
                        leased: false,
                    },
                )
            })
        };
        if let Some((parent_id, message)) = notification {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            push_mailbox_locked(&mut state, &parent_id, message);
        }
        {
            // Refresh the durable session-owned roster after every durable
            // status transition so the fleet survives the owning run and a
            // process restart.
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.persist_durable_fleet_locked(&mut state);
        }
        self.changed.notify_waiters();
        self.publish_telemetry(None, None);
        true
    }

    fn fail_worker_start(&self, id: &str, error: String) {
        self.set_status(id, DelegatedAgentStatus::Failed { error }, true);
    }

    fn set_pending_if_needed(&self, id: &str) -> bool {
        self.set_status(id, DelegatedAgentStatus::Pending, false)
    }

    fn ensure_owner_active_locked(
        &self,
        state: &ManagerState,
        owner: &AgentIdentity,
    ) -> Result<(), String> {
        if let Some(error) = &state.persistence_error {
            return Err(format!("delegation persistence is unavailable: {error}"));
        }
        if state.shutting_down {
            return Err("delegation team is shutting down".into());
        }
        if owner.id == ROOT_AGENT_ID {
            return (state.root_active && owner.path == ROOT_AGENT_PATH && owner.depth == 0)
                .then_some(())
                .ok_or_else(|| "root delegation owner is not active".to_owned());
        }
        let record = state
            .records
            .get(&owner.id)
            .ok_or_else(|| "delegation owner is no longer available".to_owned())?;
        if record.identity.path != owner.path || record.identity.depth != owner.depth {
            return Err("delegation owner identity does not match team state".into());
        }
        if !matches!(record.status, DelegatedAgentStatus::Running)
            || record.interrupt_requested
            || record.shutdown.is_cancelled()
        {
            return Err(format!(
                "delegation owner is not running: {}",
                record.identity.path
            ));
        }
        Ok(())
    }

    fn fail_persistence_locked(&self, state: &mut ManagerState, error: &io::Error) {
        if state.persistence_error.is_some() {
            return;
        }
        let diagnostic = bounded_text(&format!(
            "delegation provenance persistence failed: {error}"
        ));
        state.persistence_error = Some(diagnostic.clone());
        state.shutting_down = true;
        state.root_active = false;
        let mut notifications = Vec::new();
        for record in state.records.values_mut() {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
            record.pending_messages.clear();
            record.inflight_message_ids.clear();
            record.reserved_messages = QueueUsage::default();
            record.queued_follow_ups = QueueUsage::default();
            if record.status.is_running() {
                record.status = DelegatedAgentStatus::Failed {
                    error: diagnostic.clone(),
                };
                notifications.push((
                    record.parent_id.clone(),
                    MailboxMessage {
                        kind: "task_status",
                        from: record.identity.id.clone(),
                        task_name: Some(record.task_name.clone()),
                        message: status_message(&record.identity.path, &record.status),
                        evictable: true,
                        continued: false,
                        leased: false,
                    },
                ));
            }
        }
        for (parent_id, message) in notifications {
            push_mailbox_locked(state, &parent_id, message);
        }
        self.changed.notify_waiters();
    }

    fn restored_tasks(&self, id: &str) -> VecDeque<QueuedTask> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(id)
            .map(|record| {
                record
                    .pending_initial_task
                    .iter()
                    .cloned()
                    .map(QueuedTask::Initial)
                    .chain(
                        record
                            .pending_follow_ups
                            .iter()
                            .cloned()
                            .map(QueuedTask::follow_up),
                    )
                    .collect()
            })
            .unwrap_or_default()
    }

    fn pending_message_count(&self, id: &str) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.records.get(id).map_or(0, |record| {
            record
                .pending_messages
                .len()
                .saturating_add(record.reserved_messages.messages)
                .saturating_sub(record.inflight_message_ids.len())
        })
    }

    fn interrupt_requested(&self, id: &str) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .records
            .get(id)
            .is_some_and(|record| record.interrupt_requested)
    }

    async fn acquire_follow_up_permit(
        &self,
        id: &str,
        shutdown: &crate::CancellationToken,
        deadline: Option<tokio::time::Instant>,
    ) -> PermitWait {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shutdown.is_cancelled() {
                return PermitWait::Shutdown;
            }
            {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(record) = state.records.get(id) else {
                    return PermitWait::Shutdown;
                };
                if state.persistence_error.is_some()
                    || state.shutting_down
                    || record.shutdown.is_cancelled()
                {
                    return PermitWait::Shutdown;
                }
                if record.interrupt_requested {
                    return PermitWait::Interrupted;
                }
            }
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return PermitWait::Shutdown,
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if deadline.is_some() => return PermitWait::TimedOut,
                _ = &mut notified => {}
                permit = self.current_permits().acquire_owned() => {
                    return match permit {
                        Ok(permit) => PermitWait::Acquired(permit),
                        Err(_) => PermitWait::Shutdown,
                    };
                }
            }
        }
    }

    fn drain_interrupted_commands(
        &self,
        target: &str,
        commands: &mut mpsc::Receiver<WorkerCommand>,
        queued_tasks: &mut VecDeque<QueuedTask>,
    ) -> bool {
        let mut messages = Vec::new();
        let mut saw_shutdown = false;
        while let Ok(command) = commands.try_recv() {
            match command.kind {
                WorkerCommandKind::Message(message) => messages.push(message),
                WorkerCommandKind::FollowUp => {
                    *queued_tasks = self.restored_tasks(target);
                    queued_tasks.retain(|task| matches!(task, QueuedTask::FollowUp(_)));
                }
                WorkerCommandKind::Shutdown => saw_shutdown = true,
            }
        }
        for message in messages {
            self.queue_reserved_message(target, message);
        }
        saw_shutdown
    }

    fn take_pending_messages(&self, target: &str) -> Vec<DirectedMessage> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .records
            .get_mut(target)
            .map(|record| {
                let messages = record
                    .pending_messages
                    .iter()
                    .filter(|message| !record.inflight_message_ids.contains(&message.delivery_id))
                    .cloned()
                    .collect::<Vec<_>>();
                for message in &messages {
                    record
                        .inflight_message_ids
                        .insert(message.delivery_id.clone());
                    record
                        .reserved_messages
                        .add(directed_message_bytes(message));
                }
                messages
            })
            .unwrap_or_default()
    }

    fn release_prompt_message_reservations(&self, target: &str, messages: &[DirectedMessage]) {
        if messages.is_empty() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            for message in messages {
                record.inflight_message_ids.remove(&message.delivery_id);
                record.reserved_messages.remove(QueueUsage {
                    messages: 1,
                    bytes: directed_message_bytes(message),
                });
                record
                    .pending_messages
                    .retain(|pending| pending.delivery_id != message.delivery_id);
            }
            self.persist_durable_fleet_locked(&mut state);
        }
    }

    fn restore_pending_messages(&self, target: &str, messages: Vec<DirectedMessage>) {
        if messages.is_empty() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            // The durable FIFO never gave up ownership during prompt delivery.
            // Only the process-local attempt/reservation needs to be released.
            for message in messages {
                record.inflight_message_ids.remove(&message.delivery_id);
                record.reserved_messages.remove(QueueUsage {
                    messages: 1,
                    bytes: directed_message_bytes(&message),
                });
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn queue_reserved_message(&self, target: &str, message: DirectedMessage) {
        let usage = QueueUsage {
            messages: 1,
            bytes: directed_message_bytes(&message),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record.inflight_message_ids.remove(&message.delivery_id);
            record.reserved_messages.remove(usage);
            // This command only woke the worker; the accepted payload already
            // occupies its durable position in pending_messages.
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn release_message_reservation(&self, target: &str, message: &DirectedMessage) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record.inflight_message_ids.remove(&message.delivery_id);
            record.reserved_messages.remove(QueueUsage {
                messages: 1,
                bytes: directed_message_bytes(message),
            });
            record
                .pending_messages
                .retain(|pending| pending.delivery_id != message.delivery_id);
            self.persist_durable_fleet_locked(&mut state);
        }
    }

    /// Drops one follow-up from the durable queue only after the child agent
    /// reports `FollowUpDelivered`, which is emitted after its session append.
    fn acknowledge_follow_up_delivery(&self, target: &str, follow_up: &QueuedFollowUp) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record
                .pending_follow_ups
                .retain(|pending| pending.delivery_id != follow_up.delivery_id);
            self.persist_durable_fleet_locked(&mut state);
        }
    }

    fn persist_task_result(
        &self,
        target: &str,
        task: &QueuedTask,
        result: &TaskRestore,
        delivered: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.records.get_mut(target) else {
            return;
        };
        let clear = delivered || matches!(result, TaskRestore::DeadLettered { .. });
        match task {
            QueuedTask::Initial(task) => {
                if let Some(pending) = record
                    .pending_initial_task
                    .as_mut()
                    .filter(|pending| pending.delivery_id == task.delivery_id)
                {
                    if clear {
                        record.pending_initial_task = None;
                    } else if let TaskRestore::Restored { attempts } = result {
                        pending.attempts = *attempts;
                    }
                }
            }
            QueuedTask::FollowUp(task) => {
                if clear {
                    record
                        .pending_follow_ups
                        .retain(|pending| pending.delivery_id != task.delivery_id);
                    if !delivered {
                        record.queued_follow_ups.remove(task.usage());
                    }
                } else if let TaskRestore::Restored { attempts } = result {
                    if let Some(pending) = record
                        .pending_follow_ups
                        .iter_mut()
                        .find(|pending| pending.delivery_id == task.delivery_id)
                    {
                        pending.attempts = *attempts;
                    }
                }
            }
        }
        if let TaskRestore::DeadLettered { attempts } = result {
            record.durable_diagnostic = Some(format!(
                "delegated task dead-lettered after {attempts} undelivered attempts"
            ));
        }
        self.persist_durable_fleet_locked(&mut state);
    }

    fn release_follow_up_usage(&self, target: &str, usage: QueueUsage) {
        if usage.messages == 0 {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.records.get_mut(target) {
            record.queued_follow_ups.remove(usage);
        }
    }

    fn resolve_id_locked(state: &ManagerState, target: &str) -> Option<String> {
        if target == ROOT_AGENT_ID || target == ROOT_AGENT_PATH {
            return Some(ROOT_AGENT_ID.into());
        }
        if state.records.contains_key(target) {
            return Some(target.to_owned());
        }
        state
            .records
            .iter()
            .find_map(|(id, record)| (record.identity.path == target).then(|| id.clone()))
    }

    async fn send_message(
        &self,
        owner: &AgentIdentity,
        target: &str,
        message: String,
    ) -> Result<Value, String> {
        validate_durable_text("message", &message)?;
        let candidate = DirectedMessage {
            delivery_id: new_delivery_id()?,
            from: owner.id.clone(),
            message,
        };
        let (target_id, delivery) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable `sync_data`.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let (target_id, delivery, encoded, command_permit) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.ensure_owner_active_locked(&state, owner)?;
                let target_id = Self::resolve_id_locked(&state, target)
                    .ok_or_else(|| format!("unknown delegation target: {target}"))?;

                let mut command_permit = None;
                let delivery = if target_id == ROOT_AGENT_ID {
                    let mailbox_message = MailboxMessage {
                        kind: "message",
                        from: candidate.from.clone(),
                        task_name: None,
                        message: candidate.message.clone(),
                        evictable: false,
                        continued: false,
                        leased: false,
                    };
                    if !mailbox_can_accept_after_evicting_automatic(
                        &state.root_mailbox,
                        &mailbox_message,
                    ) {
                        return Err("root delegation mailbox is full".into());
                    }
                    "queued"
                } else {
                    let record = state
                        .records
                        .get(&target_id)
                        .expect("resolved child exists");
                    if matches!(record.status, DelegatedAgentStatus::Shutdown)
                        || record.shutdown.is_cancelled()
                    {
                        return Err(format!("target is shut down: {}", record.identity.path));
                    }
                    if record.interrupt_requested {
                        return Err(format!(
                            "target is being interrupted: {}",
                            record.identity.path
                        ));
                    }
                    if !record_can_accept_pending_message(record, &candidate) {
                        return Err(format!(
                            "target pending-message queue is full: {}",
                            record.identity.path
                        ));
                    }
                    if matches!(record.status, DelegatedAgentStatus::Running) {
                        command_permit = Some(
                            record
                                .command_tx
                                .clone()
                                .try_reserve_owned()
                                .map_err(command_queue_error)?,
                        );
                        "steering"
                    } else {
                        "queued"
                    }
                };

                let encoded = serde_json::to_vec(&ProvenanceEvent::Message {
                    timestamp_ms: timestamp_ms(),
                    from: &owner.id,
                    to: &target_id,
                    kind: "message",
                    message: &candidate.message,
                })
                .map_err(io::Error::other);
                let encoded = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!("could not persist message provenance: {error}");
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                (target_id, delivery, encoded, command_permit)
            };
            if let Err(error) = self.journal.append_encoded(&encoded) {
                let message = format!("could not persist message provenance: {error}");
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(message);
            }
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if target_id == ROOT_AGENT_ID {
                    push_mailbox_bounded(
                        &mut state.root_mailbox,
                        MailboxMessage {
                            kind: "message",
                            from: candidate.from,
                            task_name: None,
                            message: candidate.message,
                            evictable: false,
                            continued: false,
                            leased: false,
                        },
                    );
                } else {
                    let Some(record) = state.records.get_mut(&target_id) else {
                        // An owning-run reset removed the record mid-operation;
                        // do not resurrect it after its provenance record.
                        return Err("delegation team is shutting down".into());
                    };
                    record.pending_messages.push_back(candidate.clone());
                    if let Some(permit) = command_permit {
                        record
                            .inflight_message_ids
                            .insert(candidate.delivery_id.clone());
                        record
                            .reserved_messages
                            .add(directed_message_bytes(&candidate));
                        permit.send(WorkerCommand::message(candidate));
                    }
                }
                self.persist_durable_fleet_locked(&mut state);
                if let Some(error) = state.persistence_error.clone() {
                    return Err(format!("could not persist queued message: {error}"));
                }
            }
            (target_id, delivery)
        };
        self.changed.notify_waiters();
        Ok(json!({"delivered_to": target_id, "delivery": delivery}))
    }

    async fn follow_up(
        self: &Arc<Self>,
        owner: &AgentIdentity,
        request: FollowUpRequest,
    ) -> Result<Value, String> {
        validate_durable_text("follow-up", &request.message)?;
        let follow_up = QueuedFollowUp {
            delivery_id: new_delivery_id()?,
            from: owner.id.clone(),
            message: request.message,
            attempts: 0,
        };
        let (target_id, target_path, running_now, resume) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable syncs.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let (
                target_id,
                target_path,
                running_now,
                follow_up,
                encoded_message,
                encoded_status,
                command_permit,
                resume,
            ) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.ensure_owner_active_locked(&state, owner)?;
                let target_id = Self::resolve_id_locked(&state, &request.target)
                    .ok_or_else(|| format!("unknown delegation target: {}", request.target))?;
                if target_id == ROOT_AGENT_ID {
                    return Err("followup_task cannot target the root agent".into());
                }
                let record = state
                    .records
                    .get(&target_id)
                    .expect("resolved child exists");
                if matches!(record.status, DelegatedAgentStatus::Shutdown)
                    || record.shutdown.is_cancelled()
                {
                    return Err(format!("target is shut down: {}", record.identity.path));
                }
                if record.interrupt_requested {
                    return Err(format!(
                        "target is being interrupted: {}",
                        record.identity.path
                    ));
                }
                if !record_can_accept_follow_up(record, &follow_up) {
                    return Err(format!(
                        "target follow-up queue is full: {}",
                        record.identity.path
                    ));
                }
                // A suspended worker (a record restored from the durable
                // roster, or parked at the approval boundary / session release)
                // has no live task in this process. An explicit follow-up is the
                // decision and the new task: the worker is started again under
                // a fresh fleet claim, or the refusal names its reason.
                let suspended =
                    !record.live_task && (record.detached || record.detached_commands.is_some());
                let running_now = !suspended && record.status.is_running();
                let target_path = record.identity.path.clone();
                let session_path = record.session_path.clone();
                let extension_policy = record.extension_policy.clone();
                let shutdown = record.shutdown.clone();
                let claim = if suspended {
                    Some(self.fresh_claim_for(record)?)
                } else {
                    None
                };
                let (permit, mut session, mut resumed_commands) = if suspended {
                    let permit = self.current_permits().try_acquire_owned().map_err(|_| {
                        "delegation concurrency limit reached; no free execution slot is available to resume this worker"
                            .to_owned()
                    })?;
                    let session = self.reopen_child_session(&session_path).map_err(|error| {
                        format!(
                            "worker could not be resumed: its child session could not be reopened: {error}"
                        )
                    })?;
                    Self::discard_inputs_delivered_to_session(
                        state
                            .records
                            .get_mut(&target_id)
                            .expect("resolved child exists"),
                        &session,
                    )
                    .map_err(|error| format!("worker could not be resumed: {error}"))?;
                    let commands = match state
                        .records
                        .get_mut(&target_id)
                        .expect("resolved child exists")
                        .detached_commands
                        .take()
                    {
                        Some(commands) => commands,
                        None => {
                            let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
                            state
                                .records
                                .get_mut(&target_id)
                                .expect("resolved child exists")
                                .command_tx = command_tx;
                            command_rx
                        }
                    };
                    (Some(permit), Some(session), Some(commands))
                } else {
                    (None, None, None)
                };
                let command_permit = state.records[&target_id]
                    .command_tx
                    .clone()
                    .try_reserve_owned()
                    .map_err(command_queue_error)?;

                let encoded_message = serde_json::to_vec(&ProvenanceEvent::Message {
                    timestamp_ms: timestamp_ms(),
                    from: &owner.id,
                    to: &target_id,
                    kind: "follow_up",
                    message: &follow_up.message,
                })
                .map_err(io::Error::other);
                let encoded_message = match encoded_message {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!("could not persist follow-up provenance: {error}");
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                let encoded_status = (!running_now)
                    .then(|| {
                        serde_json::to_vec(&ProvenanceEvent::AgentStatus {
                            timestamp_ms: timestamp_ms(),
                            agent_id: &target_id,
                            status: &DelegatedAgentStatus::Pending,
                        })
                        .map_err(io::Error::other)
                    })
                    .transpose();
                let encoded_status = match encoded_status {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!(
                            "could not persist pending follow-up status provenance: {error}"
                        );
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                let resume = match (permit, session.take(), resumed_commands.take(), claim) {
                    (Some(initial_permit), Some(session), Some(commands), Some(claim)) => Some((
                        WorkerStartup {
                            generation: state.records[&target_id].worker_generation + 1,
                            identity: state.records[&target_id].identity.clone(),
                            session,
                            commands,
                            shutdown,
                            initial_permit,
                            extension_policy,
                            deadline: None,
                            deadline_ms: None,
                        },
                        claim,
                    )),
                    _ => None,
                };
                (
                    target_id,
                    target_path,
                    running_now,
                    follow_up,
                    encoded_message,
                    encoded_status,
                    command_permit,
                    resume,
                )
            };
            if let Err(error) = self.journal.append_encoded(&encoded_message) {
                let message = format!("could not persist follow-up provenance: {error}");
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.fail_persistence_locked(&mut state, &error);
                return Err(message);
            }
            if let Some(encoded) = &encoded_status {
                if let Err(error) = self.journal.append_encoded(encoded) {
                    let message =
                        format!("could not persist pending follow-up status provenance: {error}");
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    self.fail_persistence_locked(&mut state, &error);
                    return Err(message);
                }
            }
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !running_now {
                    if let Some(record) = state.records.get_mut(&target_id) {
                        record.status = DelegatedAgentStatus::Pending;
                        // A resumed worker is no longer terminal; drop the stale
                        // completion timestamp so elapsed-time consumers do not see
                        // a frozen first-run interval.
                        record.completed_at_ms = None;
                        // A follow-up to a settled worker starts a new run. If the
                        // original wall budget already elapsed while the worker was
                        // idle, the spawn-frozen deadline would start an instantly
                        // expired run; re-anchor it from the requested timeout so
                        // the resumed run owns a fresh budget.
                        if let (Some(deadline), Some(timeout)) = (
                            record.deadline_at_ms,
                            record
                                .extension_policy
                                .as_ref()
                                .and_then(|policy| policy.timeout_ms),
                        ) {
                            let now = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
                            if deadline <= now {
                                record.deadline_at_ms = Some(now.saturating_add(timeout));
                            }
                        }
                        if let Some((startup, claim)) = &resume {
                            // The explicit decision clears the park: the worker
                            // may run again under this session owner's claim.
                            record.claim = Some(claim.clone());
                            record.worker_generation = startup.generation;
                            record.detached = false;
                            record.durable_diagnostic = None;
                            record.live_task = true;
                        }
                    }
                }
                if let Some(record) = state.records.get_mut(&target_id) {
                    let usage = follow_up.usage();
                    record.queued_follow_ups.add_usage(usage);
                    record.pending_follow_ups.push_back(follow_up.clone());
                }
                // The journal proves provenance; the fleet roster proves the
                // queued payload. Do not acknowledge acceptance until both are
                // durable, otherwise a restart could silently lose this input.
                self.persist_durable_fleet_locked(&mut state);
                if let Some(error) = state.persistence_error.clone() {
                    return Err(format!("could not persist queued follow-up: {error}"));
                }
                command_permit.send(WorkerCommand::follow_up());
            }
            (target_id, target_path, running_now, resume)
        };
        if let Some((mut startup, _)) = resume {
            // A resumed worker keeps its host-owned wall budget: adopt the
            // durable deadline the follow-up just re-anchored so the resumed run
            // cannot outlive it or start already expired.
            let host_deadline = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .records
                .get(&startup.identity.id)
                .and_then(|record| record.deadline_at_ms);
            startup.deadline_ms = host_deadline;
            startup.deadline = host_deadline.map(wall_deadline_instant);
            self.spawn_worker(startup);
        }
        self.changed.notify_waiters();
        Ok(json!({
            "agent_id": target_id,
            "agent_path": target_path,
            "delivery": if running_now {"follow_up"} else {"new_run"}
        }))
    }

    async fn wait(
        self: &Arc<Self>,
        owner: &AgentIdentity,
        timeout: Duration,
        cancellation: &crate::CancellationToken,
        output_limit: usize,
    ) -> Result<WaitOutput, String> {
        if let Some(result) = self.take_wait_result(owner, output_limit)? {
            return Ok(result);
        }
        let _waiter = self.register_waiter(owner)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.take_wait_result(owner, output_limit)? {
                return Ok(result);
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err("wait_agent cancelled".into()),
                _ = tokio::time::sleep_until(deadline) => {
                    let agents = self.list_value_for(owner)?;
                    return Ok(WaitOutput {
                        value: json!({"timed_out": true, "messages": [], "agents": agents}),
                        delivery_id: None,
                    });
                }
                _ = &mut notified => {}
            }
        }
    }

    fn register_waiter(&self, owner: &AgentIdentity) -> Result<WaiterGuard<'_>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        if state.active_waiters >= self.config.limits.max_total_agents {
            return Err(format!(
                "delegation waiter limit reached ({})",
                self.config.limits.max_total_agents
            ));
        }
        state.active_waiters += 1;
        Ok(WaiterGuard { manager: self })
    }

    fn take_wait_result(
        &self,
        owner: &AgentIdentity,
        output_limit: usize,
    ) -> Result<Option<WaitOutput>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        let delivery_id = state.next_mailbox_delivery;
        let leased = if owner.id == ROOT_AGENT_ID {
            if state.root_mailbox.is_empty() {
                None
            } else {
                self.verify_held_fleet_lease()?;
                if state.root_mailbox_delivery.is_some() {
                    return Err(
                        "a root mailbox delivery is already awaiting durable acknowledgement"
                            .into(),
                    );
                }
                let (value, plan) =
                    lease_mailbox_page(&mut state.root_mailbox, delivery_id, output_limit)?;
                state.root_mailbox_delivery = Some(plan);
                Some(value)
            }
        } else {
            let record = state
                .records
                .get_mut(&owner.id)
                .expect("validated owner exists");
            if record.mailbox.is_empty() {
                None
            } else {
                self.verify_held_fleet_lease()?;
                if record.mailbox_delivery.is_some() {
                    return Err(
                        "an agent mailbox delivery is already awaiting durable acknowledgement"
                            .into(),
                    );
                }
                let (value, plan) =
                    lease_mailbox_page(&mut record.mailbox, delivery_id, output_limit)?;
                record.mailbox_delivery = Some(plan);
                Some(value)
            }
        };
        if let Some(value) = leased {
            state.next_mailbox_delivery = state.next_mailbox_delivery.saturating_add(1);
            self.persist_durable_fleet_locked(&mut state);
            if let Some(error) = state.persistence_error.clone() {
                return Err(format!("could not persist mailbox delivery: {error}"));
            }
            return Ok(Some(WaitOutput {
                value,
                delivery_id: Some(delivery_id),
            }));
        }

        let descendants_running = state.records.values().any(|record| {
            is_descendant_path(&record.identity.path, &owner.path) && record.status.is_running()
        });
        if !descendants_running {
            Ok(Some(WaitOutput {
                value: json!({"timed_out": false, "messages": [], "agents": list_value_locked(&state)}),
                delivery_id: None,
            }))
        } else {
            Ok(None)
        }
    }

    fn resolve_mailbox_delivery(&self, owner_id: &str, delivery_id: u64, delivered: bool) {
        if self.verify_held_fleet_lease().is_err() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let resolved = if owner_id == ROOT_AGENT_ID {
            match state.root_mailbox_delivery {
                Some(plan) if plan.id == delivery_id => {
                    state.root_mailbox_delivery = None;
                    resolve_mailbox_page(&mut state.root_mailbox, plan, delivered);
                    true
                }
                _ => false,
            }
        } else if let Some(record) = state.records.get_mut(owner_id) {
            match record.mailbox_delivery {
                Some(plan) if plan.id == delivery_id => {
                    record.mailbox_delivery = None;
                    resolve_mailbox_page(&mut record.mailbox, plan, delivered);
                    true
                }
                _ => false,
            }
        } else {
            false
        };
        if resolved {
            self.persist_durable_fleet_locked(&mut state);
        }
        drop(state);
        if resolved {
            self.changed.notify_waiters();
        }
    }

    fn list_value_for(&self, owner: &AgentIdentity) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_owner_active_locked(&state, owner)?;
        Ok(list_value_locked(&state))
    }

    async fn interrupt(&self, owner: &AgentIdentity, target: &str) -> Result<Value, String> {
        let (target_id, path, status, requested) = {
            // The journal_order guard spans decide → append → commit so that
            // journal record order matches state mutation order, while the
            // state lock is dropped across the journal's durable `sync_data`.
            let _journal_order = self
                .journal_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_fleet_lease()?;
            let (target_id, path, status, requested, encoded) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                self.ensure_owner_active_locked(&state, owner)?;
                let target_id = Self::resolve_id_locked(&state, target)
                    .ok_or_else(|| format!("unknown delegation target: {target}"))?;
                if target_id == ROOT_AGENT_ID {
                    return Err("interrupt_agent cannot target the root agent".into());
                }
                let record = state
                    .records
                    .get(&target_id)
                    .expect("resolved child exists");
                if !is_descendant_path(&record.identity.path, &owner.path)
                    && owner.id != ROOT_AGENT_ID
                {
                    return Err("an agent may only interrupt its descendants".into());
                }
                let path = record.identity.path.clone();
                let status = record.status.clone();
                let requested = (status.is_running() || status == DelegatedAgentStatus::Idle)
                    && !record.interrupt_requested;
                let encoded = requested
                    .then(|| {
                        serde_json::to_vec(&ProvenanceEvent::InterruptRequested {
                            timestamp_ms: timestamp_ms(),
                            from: &owner.id,
                            to: &target_id,
                        })
                        .map_err(io::Error::other)
                    })
                    .transpose();
                let encoded = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let message = format!("could not persist interrupt provenance: {error}");
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                };
                (target_id, path, status, requested, encoded)
            };
            if requested {
                if let Some(encoded) = encoded {
                    if let Err(error) = self.journal.append_encoded(&encoded) {
                        let message = format!("could not persist interrupt provenance: {error}");
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        self.fail_persistence_locked(&mut state, &error);
                        return Err(message);
                    }
                }
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(record) = state.records.get_mut(&target_id) {
                    record.interrupt_requested = true;
                }
            }
            (target_id, path, status, requested)
        };
        if requested {
            self.request_shutdown_descendants(&target_id);
            self.changed.notify_waiters();
        }
        Ok(
            json!({"agent_id": target_id, "agent_path": path, "previous_status": status.label(), "interrupt_requested": requested}),
        )
    }

    /// Cumulative accounting snapshots for extension-owned descendants.
    ///
    /// Reading never consumes a watermark: only the owning session's synced
    /// usage ledger establishes which increments have actually been mirrored.
    /// Include uncertainty-only snapshots even when no turn completed.
    fn extension_usage_records(&self, owner_id: &str) -> Vec<DelegatedUsageRecord> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owner_path = if owner_id == ROOT_AGENT_ID {
            Some(ROOT_AGENT_PATH.to_owned())
        } else {
            state
                .records
                .get(owner_id)
                .map(|record| record.identity.path.clone())
        };
        let Some(owner_path) = owner_path else {
            return Vec::new();
        };
        state
            .records
            .values()
            .filter(|record| {
                record.extension_principal.is_some()
                    && is_descendant_path(&record.identity.path, &owner_path)
            })
            .map(|record| DelegatedUsageRecord {
                agent_id: record.identity.id.clone(),
                usage: record.usage,
                usage_uncertain: record.usage_uncertain,
                usage_exposure: record.usage_exposure,
                cost: record.cost,
                turn_count: record.turn_count,
                tool_call_count: record.tool_call_count,
            })
            .collect()
    }

    /// Durable idempotency: the spawn result for an extension principal's
    /// idempotency key, reconstructed from the session-owned record that
    /// survived the parent turn or a process restart.
    fn extension_owned_record(
        &self,
        principal: &str,
        resource_owner: &str,
        idempotency_key: &str,
    ) -> Option<ExtensionDurableSpawn> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state.records.values().find(|record| {
            record.extension_principal.as_deref() == Some(principal)
                && record.extension_idempotency_key.as_deref() == Some(idempotency_key)
                // Legacy records without an owner are returned only to refuse
                // an unverifiable retry, never to create a duplicate worker.
                && record.extension_resource_owner.as_deref().is_none_or(|owner| owner == resource_owner)
        })?;
        Some(ExtensionDurableSpawn {
            task_name: record
                .display_task_name
                .clone()
                .unwrap_or_else(|| record.task_name.clone()),
            profile: record.extension_profile.clone(),
            fingerprint: record.extension_fingerprint.clone(),
            policy: record.extension_requested_policy.clone(),
            resource_owner: record.extension_resource_owner.clone(),
            message_sha256: record.extension_message_sha256.clone(),
            result: extension_spawn_result_value(record),
        })
    }

    fn request_shutdown_descendants(&self, owner_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owner_path = if owner_id == ROOT_AGENT_ID {
            state.root_active = false;
            // The owning session is gone. Live workers must stop, but their
            // durable child sessions are retained: each worker parks as a
            // recoverable `detached` record instead of retiring, so the next
            // session owner (a rebuilt agent, a restarted process) can reattach
            // and continue it.
            state.session_owner_released = true;
            ROOT_AGENT_PATH.to_owned()
        } else if let Some(record) = state.records.get(owner_id) {
            record.identity.path.clone()
        } else {
            return;
        };
        for record in state
            .records
            .values()
            .filter(|record| is_descendant_path(&record.identity.path, &owner_path))
        {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn request_shutdown_agent_trees(&self, roots: &BTreeSet<String>) {
        if roots.is_empty() {
            return;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root_paths = roots
            .iter()
            .filter_map(|id| {
                state
                    .records
                    .get(id)
                    .map(|record| record.identity.path.clone())
            })
            .collect::<Vec<_>>();
        for record in state.records.values().filter(|record| {
            roots.contains(&record.identity.id)
                || root_paths
                    .iter()
                    .any(|root| is_descendant_path(&record.identity.path, root))
        }) {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
        }
        drop(state);
        self.changed.notify_waiters();
    }
}

impl Drop for DelegationManager {
    fn drop(&mut self) {
        let _ = self.journal.append(&ProvenanceEvent::TeamShutdown {
            timestamp_ms: timestamp_ms(),
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.shutting_down = true;
        state.root_active = false;
        for record in state.records.values() {
            record.shutdown.cancel();
            let _ = record.command_tx.try_send(WorkerCommand::shutdown());
        }
    }
}

enum PermitWait {
    Acquired(OwnedSemaphorePermit),
    TimedOut,
    Interrupted,
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct QueuedFollowUp {
    /// Cryptographically random host delivery identity, persisted until the
    /// child session records this exact envelope.
    #[serde(default)]
    delivery_id: String,
    from: String,
    message: String,
    /// Number of failed attempts to append this input to the child session.
    #[serde(default)]
    attempts: u8,
}

impl QueuedFollowUp {
    fn usage(&self) -> QueueUsage {
        QueueUsage {
            messages: 1,
            bytes: self.from.len().saturating_add(self.message.len()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QueuedInitialTask {
    task: String,
    delivery_id: String,
    attempts: u8,
}

#[derive(Clone)]
enum QueuedTask {
    Initial(QueuedInitialTask),
    FollowUp(QueuedFollowUp),
}

impl QueuedTask {
    #[cfg(test)]
    fn initial(task: String) -> Self {
        Self::Initial(QueuedInitialTask {
            task,
            delivery_id: new_delivery_id().unwrap(),
            attempts: 0,
        })
    }

    fn delivery_id(&self) -> &str {
        match self {
            Self::Initial(task) => &task.delivery_id,
            Self::FollowUp(task) => &task.delivery_id,
        }
    }

    fn follow_up(follow_up: QueuedFollowUp) -> Self {
        Self::FollowUp(follow_up)
    }

    fn format(&self, pending: &[DirectedMessage]) -> String {
        match self {
            Self::Initial(task) => format_initial_task(
                &format!(
                    "<octet_delegation_delivery id=\"{}\" kind=\"initial\">\n{}\n</octet_delegation_delivery>",
                    task.delivery_id, task.task
                ),
                pending,
            ),
            Self::FollowUp(follow_up) => format_follow_up(follow_up, pending),
        }
    }

    fn attempts(&self) -> u8 {
        match self {
            Self::Initial(task) => task.attempts,
            Self::FollowUp(follow_up) => follow_up.attempts,
        }
    }

    fn increment_attempts(&mut self) {
        match self {
            Self::Initial(task) => task.attempts = task.attempts.saturating_add(1),
            Self::FollowUp(follow_up) => follow_up.attempts = follow_up.attempts.saturating_add(1),
        }
    }
}

enum TaskRestore {
    NotRestored,
    Restored { attempts: u8 },
    DeadLettered { attempts: u8 },
}

fn restore_undelivered_task(
    queued_tasks: &mut VecDeque<QueuedTask>,
    mut task: QueuedTask,
    task_delivered: bool,
    outcome: &WorkerOutcome,
) -> TaskRestore {
    let should_restore = !task_delivered
        && match outcome {
            WorkerOutcome::Shutdown => false,
            WorkerOutcome::Interrupted | WorkerOutcome::TimedOut => {
                matches!(&task, QueuedTask::FollowUp(_))
            }
            WorkerOutcome::LimitReached { .. }
            | WorkerOutcome::Completed(_)
            | WorkerOutcome::Failed(_) => true,
        };
    if !should_restore {
        return TaskRestore::NotRestored;
    }
    task.increment_attempts();
    if task.attempts() >= MAX_UNDELIVERED_TASK_ATTEMPTS {
        return TaskRestore::DeadLettered {
            attempts: task.attempts(),
        };
    }
    let attempts = task.attempts();
    queued_tasks.push_front(task);
    TaskRestore::Restored { attempts }
}

struct WorkerExecution {
    outcome: WorkerOutcome,
    deferred_follow_ups: VecDeque<QueuedFollowUp>,
    acknowledged_follow_ups: QueueUsage,
    task_delivered: bool,
}

impl WorkerExecution {
    fn new(outcome: WorkerOutcome) -> Self {
        Self {
            outcome,
            deferred_follow_ups: VecDeque::new(),
            acknowledged_follow_ups: QueueUsage::default(),
            task_delivered: false,
        }
    }
}

#[derive(Debug)]
enum WorkerOutcome {
    Completed(String),
    /// The run exhausted its configured per-run turn budget after producing
    /// the bounded output collected so far.
    LimitReached {
        output: String,
        turn_count: u64,
        turn_limit: u64,
    },
    Interrupted,
    TimedOut,
    Failed(String),
    Shutdown,
}

struct WaiterGuard<'a> {
    manager: &'a DelegationManager,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_waiters = state.active_waiters.saturating_sub(1);
    }
}

#[derive(Clone, Copy)]
enum CollaborationToolKind {
    Spawn,
    FollowUp,
    SendMessage,
    Wait,
    List,
    Interrupt,
}

impl CollaborationToolKind {
    const ALL: [Self; 6] = [
        Self::Spawn,
        Self::FollowUp,
        Self::SendMessage,
        Self::Wait,
        Self::List,
        Self::Interrupt,
    ];

    fn definition(self) -> ToolDef {
        match self {
            Self::Spawn => tool_def(
                "spawn_agent",
                "Spawn an isolated child agent for an independent task. Returns immediately; use wait_agent or list_agents for status.",
                json!({
                    "type": "object",
                    "properties": {
                        "task_name": {"type": "string", "description": "Unique lowercase task name under this agent (letters, digits, underscore, hyphen)."},
                        "message": {"type": "string", "description": "Complete task and relevant context for the child."}
                    },
                    "required": ["task_name", "message"],
                    "additionalProperties": false
                }),
            ),
            Self::FollowUp => tool_def(
                "followup_task",
                "Send additional work to a delegated agent. It is queued after an active run or starts a new run when idle.",
                target_message_schema(),
            ),
            Self::SendMessage => tool_def(
                "send_message",
                "Send information to another agent. Active agents receive steering; idle agents receive it with their next task.",
                target_message_schema(),
            ),
            Self::Wait => tool_def(
                "wait_agent",
                "Wait for delegated-agent messages or status changes. Returns immediately if this agent has messages or no descendants are running.",
                json!({
                    "type": "object",
                    "properties": {
                        "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TOOL_TIMEOUT_MS, "description": "Maximum wait in milliseconds (default 30000)."}
                    },
                    "additionalProperties": false
                }),
            ),
            Self::List => tool_def(
                "list_agents",
                "List every agent in this delegation team, including durable session paths and current status.",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ),
            Self::Interrupt => tool_def(
                "interrupt_agent",
                "Interrupt a running descendant agent and propagate cancellation to its descendants.",
                json!({
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "description": "Agent ID or absolute delegation path."}
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
            ),
        }
    }
}

struct CollaborationTool {
    manager: Weak<DelegationManager>,
    owner: AgentIdentity,
    kind: CollaborationToolKind,
}

#[async_trait::async_trait]
impl Tool for CollaborationTool {
    fn definition(&self) -> ToolDef {
        self.kind.definition()
    }

    fn effect(&self, _args: &Value, _ctx: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Delegation)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<ToolOutput, ToolError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| ToolError::new("delegation team is no longer available"))?;
        if matches!(self.kind, CollaborationToolKind::Wait) {
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(30_000)
                .clamp(1, MAX_TOOL_TIMEOUT_MS);
            let wait = manager
                .wait(
                    &self.owner,
                    Duration::from_millis(timeout_ms),
                    &ctx.cancellation,
                    ctx.sandbox.max_output_bytes,
                )
                .await
                .map_err(ToolError::new)?;
            let text = match serde_json::to_string(&wait.value) {
                Ok(text) => text,
                Err(error) => {
                    if let Some(delivery_id) = wait.delivery_id {
                        manager.resolve_mailbox_delivery(&self.owner.id, delivery_id, false);
                    }
                    return Err(ToolError::new(format!(
                        "could not encode collaboration result: {error}"
                    )));
                }
            };
            let mut output = ToolOutput::new(text);
            if let Some(delivery_id) = wait.delivery_id {
                let commit_manager = self.manager.clone();
                let rollback_manager = self.manager.clone();
                let commit_owner = self.owner.id.clone();
                let rollback_owner = self.owner.id.clone();
                output = output.with_delivery_commit(
                    move || {
                        if let Some(manager) = commit_manager.upgrade() {
                            manager.resolve_mailbox_delivery(&commit_owner, delivery_id, true);
                        }
                    },
                    move || {
                        if let Some(manager) = rollback_manager.upgrade() {
                            manager.resolve_mailbox_delivery(&rollback_owner, delivery_id, false);
                        }
                    },
                );
            }
            return Ok(output);
        }

        let value = match self.kind {
            CollaborationToolKind::Spawn => {
                let request = SpawnRequest {
                    task_name: required_string(&args, "task_name")?,
                    display_task_name: None,
                    message: required_string(&args, "message")?,
                    extension_policy: None,
                    extension_provenance: None,
                };
                manager.spawn(&self.owner, request)
            }
            CollaborationToolKind::FollowUp => {
                let request = FollowUpRequest {
                    target: required_string(&args, "target")?,
                    message: required_string(&args, "message")?,
                };
                manager.follow_up(&self.owner, request).await
            }
            CollaborationToolKind::SendMessage => {
                let target = required_string(&args, "target")?;
                let message = required_string(&args, "message")?;
                manager.send_message(&self.owner, &target, message).await
            }
            CollaborationToolKind::Wait => unreachable!("wait returned above"),
            CollaborationToolKind::List => manager.list_value_for(&self.owner),
            CollaborationToolKind::Interrupt => {
                let target = required_string(&args, "target")?;
                manager.interrupt(&self.owner, &target).await
            }
        }
        .map_err(ToolError::new)?;
        serde_json::to_string(&value)
            .map(ToolOutput::new)
            .map_err(|error| {
                ToolError::new(format!("could not encode collaboration result: {error}"))
            })
    }
}

pub(crate) fn enable_root_delegation(
    agent: &mut Agent,
    config: DelegationConfig,
    template: DelegationTemplate,
    root_tools: bool,
) -> Result<DelegationBinding, DelegationError> {
    if root_tools {
        for name in &COLLABORATION_TOOL_NAMES {
            if agent
                .registered_tool_names()
                .iter()
                .any(|registered| registered == name)
            {
                return Err(DelegationError::DuplicateTool((*name).into()));
            }
        }
    }
    let manager = DelegationManager::create(config, template, agent.session().path(), root_tools)?;
    // Row 3.5: delegated child runs are observed with the owner's explicit
    // context. The observer is inert unless the host installed one.
    manager.set_span_context(agent.telemetry_context().clone());
    let binding = manager.root_binding();
    if manager.root_tools {
        agent.append_system_instructions(binding.system_instructions().to_owned());
        agent.install_delegation_tools(manager.tools(&binding.identity));
    }
    Ok(binding)
}

fn root_instructions(config: &DelegationConfig) -> String {
    let proactive = match config.mode {
        DelegationMode::Available => {
            "Delegation is available when the user or task explicitly benefits from separate agents."
        }
        DelegationMode::Proactive => {
            "Use sub-agents proactively when parallel work would materially improve speed or quality."
        }
    };
    format!(
        "<octet_multi_agent_v2>\nYou are {ROOT_AGENT_PATH}, the root of a bounded agent team. {proactive}\nUse spawn_agent for independent work, send_message for timely context, followup_task for additional work, wait_agent/list_agents to coordinate, and interrupt_agent to stop obsolete work. Integrate and verify child results yourself; do not present unverified child output as fact. Delegation is bounded to {} concurrent agents including you, depth {}, and {} total agents.\n</octet_multi_agent_v2>",
        config.limits.max_concurrent_agents,
        config.limits.max_depth,
        config.limits.max_total_agents
    )
}

fn child_instructions(
    identity: &AgentIdentity,
    parent_path: &str,
    limits: &DelegationLimits,
) -> String {
    format!(
        "<octet_multi_agent_v2>\nYou are {}, delegated by {}. Complete the assigned task independently and return a concise, evidence-based result. Use send_message for information your parent needs before completion. You may spawn useful independent sub-agents within the remaining bounds (max depth {}, max {} concurrent including root). Coordinate with wait_agent/list_agents and interrupt obsolete descendants. Your final response is delivered automatically to your parent.\n</octet_multi_agent_v2>",
        identity.path, parent_path, limits.max_depth, limits.max_concurrent_agents
    )
}

fn new_delivery_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("could not allocate delivery identity: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn delivery_ids_in_envelopes(text: &str) -> BTreeSet<String> {
    const OPEN: &str = "<octet_delegation_delivery id=\"";
    const MIDDLE: &str = "\" kind=\"";
    const CLOSE: &str = "</octet_delegation_delivery>";
    let mut ids = BTreeSet::new();
    let mut rest = text;
    while let Some(offset) = rest.find(OPEN) {
        rest = &rest[offset + OPEN.len()..];
        let Some((id, tail)) = rest.split_once(MIDDLE) else {
            break;
        };
        let Some((kind, tail)) = tail.split_once("\">\n") else {
            break;
        };
        let Some(close) = tail.find(CLOSE) else { break };
        if id.len() == 32
            && id.bytes().all(|byte| byte.is_ascii_hexdigit())
            && matches!(kind, "message" | "follow_up" | "initial")
        {
            ids.insert(id.to_owned());
        }
        rest = &tail[close + CLOSE.len()..];
    }
    ids
}

fn format_initial_task(task: &str, pending: &[DirectedMessage]) -> String {
    if pending.is_empty() {
        return task.to_owned();
    }
    let mut formatted = String::new();
    for directed in pending {
        formatted.push_str(&format_direct_message(directed));
        formatted.push_str("\n\n");
    }
    formatted.push_str(task);
    formatted
}

fn format_direct_message(message: &DirectedMessage) -> String {
    format!(
        "<octet_delegation_delivery id=\"{}\" kind=\"message\">\n<agent_message from=\"{}\">\n{}\n</agent_message>\n</octet_delegation_delivery>",
        message.delivery_id, message.from, message.message
    )
}

fn format_follow_up(follow_up: &QueuedFollowUp, pending: &[DirectedMessage]) -> String {
    let mut formatted = String::new();
    for directed in pending {
        formatted.push_str(&format_direct_message(directed));
        formatted.push_str("\n\n");
    }
    formatted.push_str(&format!(
        "<octet_delegation_delivery id=\"{}\" kind=\"follow_up\">\n<followup_task from=\"{}\">\n{}\n</followup_task>\n</octet_delegation_delivery>",
        follow_up.delivery_id, follow_up.from, follow_up.message
    ));
    formatted
}

fn mailbox_message_bytes(message: &MailboxMessage) -> usize {
    message.kind.len()
        + message.from.len()
        + message.task_name.as_ref().map_or(0, String::len)
        + message.message.len()
}

fn mailbox_can_accept(mailbox: &VecDeque<MailboxMessage>, message: &MailboxMessage) -> bool {
    mailbox.len() < MAX_MAILBOX_MESSAGES
        && mailbox
            .iter()
            .fold(0usize, |total, item| {
                total.saturating_add(mailbox_message_bytes(item))
            })
            .saturating_add(mailbox_message_bytes(message))
            <= MAX_MAILBOX_BYTES
}

fn mailbox_can_accept_after_evicting_automatic(
    mailbox: &VecDeque<MailboxMessage>,
    message: &MailboxMessage,
) -> bool {
    let message_bytes = mailbox_message_bytes(message);
    if message_bytes > MAX_MAILBOX_BYTES {
        return false;
    }
    let mut entries = mailbox.len();
    let mut bytes = mailbox.iter().fold(0usize, |total, item| {
        total.saturating_add(mailbox_message_bytes(item))
    });
    if entries < MAX_MAILBOX_MESSAGES && bytes.saturating_add(message_bytes) <= MAX_MAILBOX_BYTES {
        return true;
    }
    for entry in mailbox
        .iter()
        .filter(|entry| entry.evictable && !entry.leased)
    {
        entries = entries.saturating_sub(1);
        bytes = bytes.saturating_sub(mailbox_message_bytes(entry));
        if entries < MAX_MAILBOX_MESSAGES
            && bytes.saturating_add(message_bytes) <= MAX_MAILBOX_BYTES
        {
            return true;
        }
    }
    false
}

fn push_mailbox_bounded(mailbox: &mut VecDeque<MailboxMessage>, message: MailboxMessage) {
    let message_bytes = mailbox_message_bytes(&message);
    if message_bytes > MAX_MAILBOX_BYTES {
        return;
    }
    while !mailbox_can_accept(mailbox, &message) {
        let Some(index) = mailbox
            .iter()
            .position(|entry| entry.evictable && !entry.leased)
        else {
            // Accepted direct messages are durable work and must never be evicted by
            // best-effort automatic notifications.
            return;
        };
        mailbox.remove(index);
    }
    mailbox.push_back(message);
}

fn mailbox_delivery_message(message: &MailboxMessage, text: &str, remaining_bytes: usize) -> Value {
    let mut value = json!({
        "kind": message.kind,
        "from": message.from,
        "task_name": message.task_name,
        "message": text,
    });
    let object = value
        .as_object_mut()
        .expect("mailbox delivery message is an object");
    if message.continued {
        object.insert("continued".into(), Value::Bool(true));
    }
    if remaining_bytes > 0 {
        object.insert("remaining_bytes".into(), json!(remaining_bytes));
    }
    value
}

fn mailbox_delivery_value(messages: Vec<Value>, more: bool) -> Value {
    json!({"timed_out": false, "messages": messages, "more": more})
}

fn encoded_value_len(value: &Value) -> Result<usize, String> {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .map_err(|error| format!("could not encode mailbox delivery: {error}"))
}

fn lease_mailbox_page(
    mailbox: &mut VecDeque<MailboxMessage>,
    delivery_id: u64,
    output_limit: usize,
) -> Result<(Value, MailboxDeliveryPlan), String> {
    if mailbox.iter().any(|message| message.leased) {
        return Err("a mailbox delivery is already awaiting durable acknowledgement".into());
    }

    let mut rendered = Vec::new();
    let mut complete_messages = 0usize;
    for message in mailbox.iter() {
        let mut candidate = rendered.clone();
        candidate.push(mailbox_delivery_message(message, &message.message, 0));
        // `false` is one byte longer than `true`, so this remains safe if
        // overflow means the final page needs to advertise `more: true`.
        if encoded_value_len(&mailbox_delivery_value(candidate, false))? > output_limit {
            break;
        }
        rendered.push(mailbox_delivery_message(message, &message.message, 0));
        complete_messages += 1;
    }

    let (value, partial_bytes, touched_messages) = if complete_messages > 0 {
        let more = complete_messages < mailbox.len();
        (mailbox_delivery_value(rendered, more), 0, complete_messages)
    } else {
        let message = mailbox
            .front()
            .expect("mailbox page is created only for a non-empty mailbox");
        let boundaries = message
            .message
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .filter(|index| *index < message.message.len())
            .collect::<Vec<_>>();
        let mut low = 0usize;
        let mut high = boundaries.len();
        let mut best = None;
        while low < high {
            let middle = low + (high - low) / 2;
            let end = boundaries[middle];
            let remaining = message.message.len() - end;
            let candidate = mailbox_delivery_value(
                vec![mailbox_delivery_message(
                    message,
                    &message.message[..end],
                    remaining,
                )],
                true,
            );
            if encoded_value_len(&candidate)? <= output_limit {
                best = Some((end, candidate));
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let Some((partial_bytes, value)) = best else {
            return Err(format!(
                "delegation tool-output limit ({output_limit} bytes) is too small for one mailbox message chunk"
            ));
        };
        (value, partial_bytes, 1)
    };

    debug_assert!(encoded_value_len(&value).is_ok_and(|length| length <= output_limit));
    for message in mailbox.iter_mut().take(touched_messages) {
        message.leased = true;
    }
    Ok((
        value,
        MailboxDeliveryPlan {
            id: delivery_id,
            complete_messages,
            partial_bytes,
            touched_messages,
        },
    ))
}

fn resolve_mailbox_page(
    mailbox: &mut VecDeque<MailboxMessage>,
    plan: MailboxDeliveryPlan,
    delivered: bool,
) {
    if !delivered {
        for message in mailbox.iter_mut().take(plan.touched_messages) {
            message.leased = false;
        }
        return;
    }

    for _ in 0..plan.complete_messages {
        let removed = mailbox
            .pop_front()
            .expect("leased complete mailbox message still exists");
        debug_assert!(removed.leased);
    }
    if plan.partial_bytes > 0 {
        let message = mailbox
            .front_mut()
            .expect("leased partial mailbox message still exists");
        debug_assert!(message.leased && message.message.is_char_boundary(plan.partial_bytes));
        message.message.drain(..plan.partial_bytes);
        message.continued = true;
        message.leased = false;
    }
}

fn push_mailbox_locked(state: &mut ManagerState, target: &str, message: MailboxMessage) {
    if target == ROOT_AGENT_ID {
        push_mailbox_bounded(&mut state.root_mailbox, message);
    } else if let Some(record) = state.records.get_mut(target) {
        push_mailbox_bounded(&mut record.mailbox, message);
    }
}

fn directed_message_bytes(message: &DirectedMessage) -> usize {
    message.from.len().saturating_add(message.message.len())
}

fn record_can_accept_pending_message(record: &AgentRecord, message: &DirectedMessage) -> bool {
    let pending_bytes = record.pending_messages.iter().fold(0usize, |total, item| {
        total.saturating_add(directed_message_bytes(item))
    });
    record
        .pending_messages
        .len()
        .saturating_add(record.reserved_messages.messages)
        .saturating_sub(record.inflight_message_ids.len())
        < MAX_PENDING_MESSAGES
        && pending_bytes
            .saturating_add(record.reserved_messages.bytes)
            .saturating_sub(
                record
                    .pending_messages
                    .iter()
                    .filter(|item| record.inflight_message_ids.contains(&item.delivery_id))
                    .map(directed_message_bytes)
                    .sum::<usize>(),
            )
            .saturating_add(directed_message_bytes(message))
            <= MAX_PENDING_MESSAGE_BYTES
}

fn record_can_accept_follow_up(record: &AgentRecord, follow_up: &QueuedFollowUp) -> bool {
    let usage = follow_up.usage();
    record.queued_follow_ups.messages < MAX_QUEUED_FOLLOW_UPS
        && record.queued_follow_ups.bytes.saturating_add(usage.bytes) <= MAX_QUEUED_FOLLOW_UP_BYTES
}

fn command_queue_error<T>(error: tokio::sync::mpsc::error::TrySendError<T>) -> String {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            "target delegation command queue is full".into()
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "target worker is no longer available".into()
        }
    }
}

/// Restore the remaining absolute wall budget, including time spent detached.
fn wall_deadline_instant(deadline_ms: u64) -> tokio::time::Instant {
    let now = u64::try_from(timestamp_ms()).unwrap_or(u64::MAX);
    tokio::time::Instant::now() + Duration::from_millis(deadline_ms.saturating_sub(now))
}

fn classify_delegation_failure(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("persist") || lower.contains("session descriptor") {
        "persistence_failure"
    } else if lower.contains("provider") || lower.contains("model response") {
        "provider_failure"
    } else if lower.contains("tool")
        && (lower.contains("policy")
            || lower.contains("scope")
            || lower.contains("unavailable")
            || lower.contains("denied"))
    {
        "tool_policy_failure"
    } else if lower.contains("token")
        || lower.contains("turn")
        || lower.contains("cost")
        || lower.contains("limit")
    {
        "limit"
    } else if lower.contains("start") || lower.contains("construct") {
        "child_construction_failure"
    } else if lower.contains("interrupt") || lower.contains("abort") || lower.contains("cancel") {
        "cancellation"
    } else {
        "child_failure"
    }
}

fn status_message(path: &str, status: &DelegatedAgentStatus) -> String {
    match status {
        DelegatedAgentStatus::Completed { output } => {
            format!("{path} completed:\n{output}")
        }
        DelegatedAgentStatus::LimitReached {
            output,
            turn_count,
            turn_limit,
        } => {
            let output = if output.is_empty() {
                "no final answer was produced".to_owned()
            } else {
                format!("partial output:\n{output}")
            };
            format!("{path} reached its turn limit ({turn_count}/{turn_limit} turns); {output}")
        }
        DelegatedAgentStatus::Failed { error } => format!("{path} failed: {error}"),
        DelegatedAgentStatus::Interrupted => format!("{path} was interrupted"),
        DelegatedAgentStatus::TimedOut => format!("{path} timed out"),
        DelegatedAgentStatus::Detached => {
            format!("{path} is detached: it survived its owning run and can be reattached")
        }
        DelegatedAgentStatus::AwaitingApproval { reason } => {
            format!("{path} is awaiting approval and has not acted: {reason}")
        }
        DelegatedAgentStatus::Shutdown => format!("{path} was shut down"),
        DelegatedAgentStatus::Pending
        | DelegatedAgentStatus::Idle
        | DelegatedAgentStatus::Running => {
            format!("{path} is {}", status.label())
        }
    }
}

/// One bounded durable snapshot of a session-owned worker.
fn durable_fleet_record(record: &AgentRecord) -> DurableFleetRecord {
    DurableFleetRecord {
        agent_id: record.identity.id.clone(),
        agent_path: record.identity.path.clone(),
        parent_id: record.parent_id.clone(),
        depth: record.identity.depth,
        task_name: record.task_name.clone(),
        display_task_name: record.display_task_name.clone(),
        session_path: record.session_path.clone(),
        status: record.status.clone(),
        detached: record.detached,
        created_at_ms: record.created_at_ms,
        started_at_ms: record.started_at_ms,
        completed_at_ms: record.completed_at_ms,
        turn_count: record.turn_count,
        tool_call_count: record.tool_call_count,
        usage: record.usage,
        usage_uncertain: record.usage_uncertain,
        usage_exposure: record.usage_exposure,
        cost: record.cost,
        cost_microdollars: record.cost_microdollars,
        deadline_at_ms: record.deadline_at_ms,
        turn_limit: record.turn_limit,
        extension_principal: record.extension_principal.clone(),
        extension_profile: record.extension_profile.clone(),
        extension_idempotency_key: record.extension_idempotency_key.clone(),
        extension_resource_owner: record.extension_resource_owner.clone(),
        extension_message_sha256: record.extension_message_sha256.clone(),
        extension_requested_policy: record.extension_requested_policy.clone(),
        extension_fingerprint: record.extension_fingerprint.clone(),
        extension_policy: record.extension_policy.clone(),
        resource_owner: record.resource_owner.clone(),
        durable_diagnostic: record.durable_diagnostic.clone(),
        claim: record.claim.clone(),
        pending_messages: record.pending_messages.clone(),
        queued_follow_ups: record.pending_follow_ups.clone(),
        pending_initial_task: record.pending_initial_task.clone(),
        mailbox: record.mailbox.iter().map(Into::into).collect(),
        mailbox_delivery: record.mailbox_delivery,
    }
}

/// Parse the numeric suffix of a stable `agent-{n}` identity.
fn agent_number_from_id(id: &str) -> Option<u64> {
    id.strip_prefix("agent-").and_then(|rest| rest.parse().ok())
}

/// Reconstruct the stable `spawn_agent` result for a session-owned record.
///
/// Used to honour an extension spawn idempotency key across a turn or process
/// boundary without spawning a duplicate worker.
fn extension_spawn_result_value(record: &AgentRecord) -> Value {
    json!({
        "agent_id": record.identity.id,
        "agent_path": record.identity.path,
        "task_name": record
            .display_task_name
            .as_deref()
            .unwrap_or(record.task_name.as_str()),
        "profile": record.extension_profile,
        "idempotency_key": record.extension_idempotency_key,
        "fingerprint": record.extension_fingerprint,
        "status": record.status,
        "resolved_model": resolved_model_json(record.extension_policy.as_ref()),
        "policy": public_policy_json(record.extension_policy.as_ref()),
        "effective_tool_policy": record.effective_tool_policy,
        "orchestration_provenance": record.orchestration_provenance,
        "created_at_ms": record.created_at_ms,
        "started_at_ms": record.started_at_ms,
        "completed_at_ms": record.completed_at_ms,
        "turn_limit": record.turn_limit,
        "deadline_at_ms": record.deadline_at_ms,
    })
}

/// Why a session-owned worker can or cannot be opened as its own interactive
/// session in another process.
///
/// The handle is the opaque, path-free `agent-session:<sha256>` reference that
/// the extension already receives. This verdict is the host-side half: the
/// launchable path is host-only, and nothing here carries a credential, a
/// transcript path, or any session secret.
fn launchability(record: &AgentRecord) -> Result<(), String> {
    match &record.status {
        // Unattended mutation fails closed: a worker parked on a decision it
        // no longer has authority for must not be opened for more work.
        DelegatedAgentStatus::AwaitingApproval { .. } => Err(
            "worker is parked at the approval boundary; supplying new authority is required before it can be opened"
                .to_owned(),
        ),
        _ => {
            if record.live_task {
                // One writer per session: a live worker owns this transcript in
                // this process, so another process must not attach to it.
                Err(
                    "a live worker owns this session in the current process; stop or detach it first"
                        .to_owned(),
                )
            } else if !record.session_path.exists() {
                Err("the worker session file is gone".to_owned())
            } else {
                Ok(())
            }
        }
    }
}

/// A session-owned worker that can be handed to another process and opened as
/// its own interactive session.
///
/// The caller keeps the handle string (`reference`) and never receives it back
/// from an extension; `session_path` is host-only and must not be published to
/// an extension, a notice, or a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchableChildSession {
    /// Opaque, path-free, argv-safe handle: `agent-session:<sha256>`.
    pub reference: String,
    /// Host-only transcript path for the launching process.
    pub session_path: PathBuf,
    /// Stable worker identity for diagnostics.
    pub agent_id: String,
    /// Absolute delegation path of the worker.
    pub agent_path: String,
    /// Bounded worker state label at hand-over time.
    pub status: String,
}

/// Strict `agent-session:<sha256>` handle validation.
///
/// The token is deliberately a boring, quotable, argv-safe identifier: the
/// launcher passes it as one `argv` element and rejects shell metacharacters,
/// and nothing in it names a path, a credential, or a session secret.
fn validate_launch_reference(reference: &str) -> Result<(), DelegationError> {
    let Some(digest) = reference.strip_prefix("agent-session:") else {
        return Err(DelegationError::Unlaunchable(
            "worker handle must be agent-session:<sha256>".into(),
        ));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DelegationError::Unlaunchable(
            "worker handle must carry exactly 64 lowercase hex digits".into(),
        ));
    }
    Ok(())
}

/// Resolves the session-owned launchable handle for one worker from the
/// root-scoped durable roster (`<session directory>/fleet-<owner hash>.json`).
///
/// This is the host-side primitive a *separate* process needs in order to open
/// a session-owned worker as its own interactive session: it needs no live
/// agent, only the owning session directory. It fails closed - an unknown
/// handle, a parked worker, or a vanished transcript is an explicit, bounded
/// refusal, never a fabricated launch. Liveness is process-local, so a worker
/// that is still live in another process is caught by the child session's own
/// open-time lock rather than by this roster read.
pub fn resolve_launchable_child_session(
    session_directory: &Path,
    reference: &str,
) -> Result<LaunchableChildSession, DelegationError> {
    validate_launch_reference(reference)?;
    let unavailable = |message: String| DelegationError::Unlaunchable(message);
    let directory = std::fs::read_dir(session_directory).map_err(|error| {
        unavailable(format!(
            "no session-owned delegation roster in this session: {error}"
        ))
    })?;
    let mut paths = Vec::new();
    // A workspace contains private team directories as well as per-root files.
    // Bound inventory work without loading other roots' transcript bodies.
    for (count, entry) in directory.enumerate() {
        if count >= 100_000 {
            return Err(unavailable(
                "delegation directory inventory limit exceeded".into(),
            ));
        }
        let entry = entry.map_err(|error| unavailable(error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let scoped = name
            .strip_prefix("fleet-")
            .and_then(|name| name.strip_suffix(".json"))
            .is_some_and(|digest| {
                digest.len() == 16 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            });
        if scoped || name == FLEET_ROSTER_FILE {
            paths.push(entry.path());
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err(unavailable(
            "no session-owned delegation roster in this session".into(),
        ));
    }
    let mut found = None;
    for path in paths {
        // Other roots' damaged or obsolete snapshots cannot make a valid
        // worker unavailable. Invalid files confer no launch authority.
        let Ok(bytes) = secure_fs::read_private_file_bounded(&path, MAX_FLEET_ROSTER_BYTES) else {
            continue;
        };
        let Ok(fleet) = serde_json::from_slice::<DurableFleet>(&bytes) else {
            continue;
        };
        if !matches!(fleet.version, 1 | FLEET_ROSTER_VERSION) {
            continue;
        }
        let scoped_path = fleet_roster_path(session_directory, &fleet.root_session);
        if path
            .file_name()
            .is_some_and(|name| name == FLEET_ROSTER_FILE)
        {
            // Never let an old shared snapshot shadow a migrated owner's state.
            if scoped_path.exists() {
                continue;
            }
        } else if path != scoped_path {
            continue;
        }
        if let Some(record) = fleet.records.into_iter().find(|record| {
            delegated_session_reference(&record.session_path).as_deref() == Some(reference)
        }) {
            found = Some(record);
            break;
        }
    }
    let record = found.ok_or_else(|| unavailable("unknown worker handle".into()))?;
    if let DelegatedAgentStatus::AwaitingApproval { .. } = record.status {
        return Err(DelegationError::Unlaunchable(
            "worker is parked at the approval boundary; supplying new authority is required before it can be opened"
                .into(),
        ));
    }
    if !record.session_path.exists() {
        return Err(DelegationError::Unlaunchable(
            "the worker session file is gone".into(),
        ));
    }
    Ok(LaunchableChildSession {
        reference: reference.to_owned(),
        session_path: record.session_path,
        agent_id: record.agent_id,
        agent_path: record.agent_path,
        status: record.status.label().to_owned(),
    })
}

/// Opaque, session-owned delegation handle for host-side callers.
///
/// It owns the durable worker records of one session and resolves a worker's
/// launchable interactive handle with the process-local liveness the durable
/// roster cannot carry. Extensions never see this type: they receive only the
/// opaque `agent-session:<sha256>` token plus the `launchable` /
/// `launch_blocked` verdict on each `agent/list` row.
#[derive(Clone)]
pub struct SessionDelegationHandle {
    manager: Arc<DelegationManager>,
}

impl SessionDelegationHandle {
    /// Resolves a worker's launchable handle, failing closed on a parked
    /// worker, a live in-process worker, or a vanished transcript.
    pub fn launchable_child_session(
        &self,
        reference: &str,
    ) -> Result<LaunchableChildSession, DelegationError> {
        self.manager.launchable_child_session(reference)
    }

    /// The opaque handle of one worker, when it has a durable transcript.
    pub fn reference_for_agent(&self, agent_id: &str) -> Option<String> {
        let state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .records
            .get(agent_id)
            .and_then(|record| delegated_session_reference(&record.session_path))
    }

    /// Session directory that owns the durable roster.
    pub fn session_directory(&self) -> &Path {
        &self.manager.config.session_directory
    }
}

impl std::fmt::Debug for SessionDelegationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionDelegationHandle")
            .field("session_directory", &self.manager.config.session_directory)
            .finish()
    }
}

/// Bounded, actionable diagnostic for a spawn that reuses a live
/// session-scoped worker name.
///
/// The name is never recycled silently: the caller is told which worker holds
/// it, what state that worker is in, and which collaboration tool resumes or
/// stops it.
fn existing_task_name_error(record: &AgentRecord) -> String {
    let name = record
        .display_task_name
        .as_deref()
        .unwrap_or(record.task_name.as_str());
    let parent = record
        .identity
        .path
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                ROOT_AGENT_PATH
            } else {
                parent
            }
        })
        .unwrap_or(ROOT_AGENT_PATH);
    let state = record.status.label();
    let id = &record.identity.id;
    match &record.status {
        DelegatedAgentStatus::Detached | DelegatedAgentStatus::AwaitingApproval { .. } => format!(
            "task name already exists under {parent}: {name} is {state} (agent {id}); its owning \
             session reattaches it on a later turn, and a free concurrency slot is required"
        ),
        DelegatedAgentStatus::Pending | DelegatedAgentStatus::Running => format!(
            "task name already exists under {parent}: {name} is {state} (agent {id}); steer it \
             with send_message or followup_task"
        ),
        _ => format!(
            "task name already exists under {parent}: {name} is {state} (agent {id}); send more \
             work with followup_task, or stop it with interrupt_agent"
        ),
    }
}

/// Whether a child failure means the effect required an approval authority
/// that was not attached to this run.
///
/// This is the authority-to-act gate for unattended mutation, not the
/// credential/OAuth or persisted-trust gate.
fn is_missing_approval_authority(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("approval is unavailable")
        || lower.contains("approval was not granted")
        || lower.contains("approval_unavailable")
        || lower.contains("approval_denied")
}

fn list_value_locked(state: &ManagerState) -> Value {
    let agents = state
        .records
        .values()
        .map(agent_record_value)
        .collect::<Vec<_>>();
    json!({"agents": agents, "persistence_error": state.persistence_error})
}

fn agent_record_value(record: &AgentRecord) -> Value {
    // Session-owned launchable handle. The token is the same opaque,
    // path-free, argv-safe reference the extension already receives; only the
    // verdict travels with it, never the transcript path.
    let blocked = launchability(record).err();
    let handle = delegated_session_reference(&record.session_path);
    let phase = if !record.active_tools.is_empty() {
        "using_tool"
    } else {
        match &record.status {
            DelegatedAgentStatus::Pending => "queued",
            DelegatedAgentStatus::Idle => "idle",
            DelegatedAgentStatus::Running => "thinking",
            DelegatedAgentStatus::Completed { .. } => "completed",
            DelegatedAgentStatus::LimitReached { .. } => "limit_reached",
            DelegatedAgentStatus::Interrupted => "interrupted",
            DelegatedAgentStatus::Failed { .. } => "failed",
            DelegatedAgentStatus::TimedOut => "timed_out",
            DelegatedAgentStatus::Detached => "detached",
            DelegatedAgentStatus::AwaitingApproval { .. } => "awaiting_approval",
            DelegatedAgentStatus::Shutdown => "shutdown",
        }
    };
    json!({
        "agent_id": record.identity.id,
        "agent_path": record.identity.path,
        "parent_id": record.parent_id,
        "task_name": record.display_task_name.as_deref().unwrap_or(record.task_name.as_str()),
        "depth": record.identity.depth,
        "session": record.session_path,
        "status": record.status,
        "resolved_model": resolved_model_json(record.extension_policy.as_ref()),
        "policy": public_policy_json(record.extension_policy.as_ref()),
        "effective_tool_policy": record.effective_tool_policy,
        "orchestration_provenance": record.orchestration_provenance,
        "profile": record.extension_profile,
        "idempotency_key": record.extension_idempotency_key,
        "fingerprint": record.extension_fingerprint,
        "created_at_ms": record.created_at_ms,
        "started_at_ms": record.started_at_ms,
        "completed_at_ms": record.completed_at_ms,
        "detached": record.detached,
        "live_task": record.live_task,
        "handle": handle,
        "launchable": blocked.is_none(),
        "launch_blocked": blocked,
        "diagnostic": record.durable_diagnostic,
        "turn_count": record.turn_count,
        "turn_limit": record.turn_limit,
        "tool_call_count": record.tool_call_count,
        "phase": phase,
        "tool_name": record.active_tools.values().next_back(),
        "recent_tools": record
            .recent_tools
            .iter()
            .map(|activity| {
                json!({
                    "name": activity.name,
                    "args": activity.args_summary,
                    "started_at_ms": activity.started_at_ms,
                    "finished_at_ms": activity.finished_at_ms,
                    "error": activity.error,
                })
            })
            .collect::<Vec<_>>(),
        "usage": record.usage,
        "usage_uncertain": record.usage_uncertain,
        "cost_microdollars": record.cost_microdollars,
        "deadline_at_ms": record.deadline_at_ms,
    })
}

fn target_message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": {"type": "string", "description": "Agent ID or absolute delegation path."},
            "message": {"type": "string", "description": "Information or task text to deliver."}
        },
        "required": ["target", "message"],
        "additionalProperties": false
    })
}

fn tool_def(name: &str, description: &str, input_schema: Value) -> ToolDef {
    ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: name.into(),
        description: description.into(),
        parameters: input_schema,
    }
}

fn required_string(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ToolError::new(format!("{key} must be a non-empty string")))
}

fn validate_task_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 48 {
        return Err("task_name must contain 1 to 48 characters".into());
    }
    if !name.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    }) {
        return Err(
            "task_name may contain only lowercase ASCII letters, digits, underscore, and hyphen"
                .into(),
        );
    }
    Ok(())
}

fn is_descendant_path(candidate: &str, parent: &str) -> bool {
    candidate.len() > parent.len()
        && candidate.starts_with(parent)
        && candidate.as_bytes().get(parent.len()) == Some(&b'/')
}

fn validate_durable_text(kind: &str, text: &str) -> Result<(), String> {
    if text.len() > MAX_PROVENANCE_TEXT_BYTES {
        return Err(format!(
            "{kind} exceeds the {}-byte delegation limit",
            MAX_PROVENANCE_TEXT_BYTES
        ));
    }
    Ok(())
}

pub(crate) fn add_delegated_usage(total: &mut Usage, next: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(next.input_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(next.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(next.cache_write_tokens);
    total.cache_write_1h_tokens = total
        .cache_write_1h_tokens
        .saturating_add(next.cache_write_1h_tokens);
    total.output_tokens = total.output_tokens.saturating_add(next.output_tokens);
    total.reasoning_tokens = total.reasoning_tokens.saturating_add(next.reasoning_tokens);
    total.total_tokens = total.total_tokens.saturating_add(next.total_tokens);
}

pub(crate) fn add_delegated_cost(total: &mut Cost, next: Cost) {
    total.input = total.input.saturating_add(next.input);
    total.output = total.output.saturating_add(next.output);
    total.reasoning = total.reasoning.saturating_add(next.reasoning);
    total.cache_read = total.cache_read.saturating_add(next.cache_read);
    total.cache_write = total.cache_write.saturating_add(next.cache_write);
    let remainder = u64::from(total.total_picodollars_remainder)
        .saturating_add(u64::from(next.total_picodollars_remainder));
    total.total = total
        .total
        .saturating_add(next.total)
        .saturating_add(remainder / u64::from(PICODOLLARS_PER_MICRODOLLAR));
    total.total_picodollars_remainder = (remainder % u64::from(PICODOLLARS_PER_MICRODOLLAR)) as u32;
}

/// Increment of a cumulative token snapshot. Saturating, so a record that was
/// somehow rolled back can never underflow the root ledger.
pub(crate) fn subtract_usage(total: Usage, mirrored: Usage) -> Usage {
    Usage {
        input_tokens: total.input_tokens.saturating_sub(mirrored.input_tokens),
        cache_read_tokens: total
            .cache_read_tokens
            .saturating_sub(mirrored.cache_read_tokens),
        cache_write_tokens: total
            .cache_write_tokens
            .saturating_sub(mirrored.cache_write_tokens),
        cache_write_1h_tokens: total
            .cache_write_1h_tokens
            .saturating_sub(mirrored.cache_write_1h_tokens),
        output_tokens: total.output_tokens.saturating_sub(mirrored.output_tokens),
        reasoning_tokens: total
            .reasoning_tokens
            .saturating_sub(mirrored.reasoning_tokens),
        total_tokens: total.total_tokens.saturating_sub(mirrored.total_tokens),
    }
}

/// Increment of a cumulative cost snapshot.
pub(crate) fn subtract_cost(total: Cost, mirrored: Cost) -> Cost {
    let scale = u128::from(PICODOLLARS_PER_MICRODOLLAR);
    let delta = (u128::from(total.total) * scale + u128::from(total.total_picodollars_remainder))
        .saturating_sub(
            u128::from(mirrored.total) * scale + u128::from(mirrored.total_picodollars_remainder),
        );
    Cost {
        input: total.input.saturating_sub(mirrored.input),
        output: total.output.saturating_sub(mirrored.output),
        reasoning: total.reasoning.saturating_sub(mirrored.reasoning),
        cache_read: total.cache_read.saturating_sub(mirrored.cache_read),
        cache_write: total.cache_write.saturating_sub(mirrored.cache_write),
        total: (delta / scale) as u64,
        total_picodollars_remainder: (delta % scale) as u32,
    }
}

fn delegation_usage_tokens(usage: &Usage) -> u64 {
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

fn bounded_text_to(text: &str, limit: usize) -> String {
    const SUFFIX: &str = "\n...[truncated]";
    if text.len() <= limit {
        return text.to_owned();
    }
    let suffix = if limit >= SUFFIX.len() { SUFFIX } else { "" };
    let mut end = limit.saturating_sub(suffix.len()).min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], suffix)
}

fn bounded_text(text: &str) -> String {
    bounded_text_to(text, MAX_PROVENANCE_TEXT_BYTES)
}

/// Reduce parsed tool arguments to a bounded single-line summary of
/// `key=value` scalar pairs, collapsing whitespace so the value renders on
/// one picker row. Non-scalar arguments are summarized as their type.
fn tool_args_summary(args: &serde_json::Value) -> String {
    let flatten = |value: &str| {
        let mut collapsed = String::with_capacity(value.len());
        let mut previous_was_space = true;
        for character in value.chars() {
            if character.is_whitespace() {
                if !previous_was_space {
                    collapsed.push(' ');
                    previous_was_space = true;
                }
            } else {
                collapsed.push(character);
                previous_was_space = false;
            }
        }
        collapsed.trim_end().to_owned()
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(object) = args.as_object() {
        for (key, value) in object {
            let rendered = match value {
                serde_json::Value::String(text) => flatten(text),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                serde_json::Value::Null => continue,
                _ => flatten(&value.to_string()),
            };
            parts.push(format!("{key}={rendered}"));
            if parts.join(" ").len() > MAX_TOOL_ARGS_SUMMARY_BYTES {
                parts.pop();
                break;
            }
        }
    }
    let mut summary = parts.join(" ");
    if summary.len() > MAX_TOOL_ARGS_SUMMARY_BYTES {
        summary = bounded_text_to(&summary, MAX_TOOL_ARGS_SUMMARY_BYTES);
    }
    summary.replace('\n', " ")
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn create_private_team_directory(
    parent: &Path,
) -> Result<Arc<secure_fs::PrivateDirectory>, DelegationError> {
    let parent = std::path::absolute(parent)?;
    Ok(Arc::new(secure_fs::create_bound_private_directory(
        &parent, "team-",
    )?))
}

fn cleanup_failed_team_activation(
    team_directory: &secure_fs::PrivateDirectory,
) -> Result<(), String> {
    let mut failures = Vec::new();
    let journal = team_directory.path().join("provenance.jsonl");
    if let Err(error) = team_directory.remove_regular_file_if_exists(&journal) {
        failures.push(format!("remove provenance journal: {error}"));
    }
    if let Err(error) = team_directory.remove_empty_if_exists() {
        failures.push(format!("remove team directory: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests;
