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

mod collaboration_tools;
mod commands;
mod extension_service;
mod fleet;
mod helpers;
mod launch;
mod mailbox;
mod manager_fleet;
mod manager_messages;
mod manager_status;
mod manager_telemetry;
mod manager_workers;
mod tasks;
mod template;

pub(crate) use self::collaboration_tools::enable_root_delegation;
use self::collaboration_tools::*;
use self::commands::*;
pub(crate) use self::extension_service::ExtensionAgentSessionPolicy;
pub(crate) use self::extension_service::ExtensionDelegationService;
pub(crate) use self::extension_service::ExtensionDelegationSpawnRequest;
use self::extension_service::*;
pub(crate) use self::fleet::DelegatedUsageRecord;
use self::fleet::*;
pub(crate) use self::helpers::add_delegated_cost;
pub(crate) use self::helpers::add_delegated_usage;
pub(crate) use self::helpers::subtract_cost;
pub(crate) use self::helpers::subtract_usage;
use self::helpers::*;
pub use self::launch::resolve_launchable_child_session;
pub use self::launch::LaunchableChildSession;
pub use self::launch::SessionDelegationHandle;
use self::launch::*;
use self::mailbox::*;
use self::tasks::*;
pub(crate) use self::template::AgentIdentity;
pub(crate) use self::template::DelegationRuntimeSettings;
pub(crate) use self::template::DelegationTemplate;

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

impl DelegationManager {
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

#[cfg(test)]
mod tests;
