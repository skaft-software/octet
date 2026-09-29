#![allow(missing_docs)]

//! Default-off adapter from the graphical host contracts to the real octet App.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use async_trait::async_trait;
use futures_util::StreamExt as _;
use octet_agent::extension_runtime::{
    ExtensionRuntimeDomain, ExtensionRuntimeManager, ExtensionTrustDomain,
};
use octet_agent::{
    AgentEvent, CompactionReason, ContextBreakdown as AgentContextBreakdown,
    ContextSnapshot as AgentContextSnapshot, Entry, EntryId, EntryValue, GoalDecision, GoalDriver,
    GoalState as AgentGoalState, GoalStore as AgentGoalStore, GoalTurnSource, InputPart,
    OutputChannel, RunControl, RunPhase as AgentRunPhase,
    RunTerminalState as AgentRunTerminalState, Session, SessionRunOutcome, SessionRunOutcomeStatus,
    ToolError, ToolOutput, ToolProgress, UserInput,
};
use octet_ai::{
    AssistantPart, ImageSource, Media, Message, Modality, Model, ModelCatalog, ModelId,
    ReasoningConfig, ToolCallId, ToolResultPart, UserPart,
};
use octet_serve_backend::{
    isolate_process_group, parse_test_output, refresh_repository_context, ActiveCompaction,
    ActivityPhase, ActivityPhaseSummary, ActorOwnerState, AgentRunPhase as ServeRunPhase,
    AgentRunTelemetry, AgentRunTerminalState as ServeRunTerminalState, ArtifactId, ArtifactKind,
    ArtifactRef, AttachmentError, AttachmentFingerprint, AttachmentPolicy, AttachmentRef,
    AttachmentStore, AttentionState, AuthorityProfile, ColorScheme, CommandDiscovery,
    CommandSuggestion, CommandSuggestionKind, CompletedCompaction, CompletionReview,
    ContextCategory, ContextCategoryTotal, ContextCompactionReason, ContextStatus, ContextTotals,
    ContextUsage, ConversationBranchOperation, ConversationBranchProvenance, CreateSessionRequest,
    DocumentReference, DocumentStore, DocumentStoreError, DriverCommandOutcome, DurableEntryId,
    EventPayload, EvidenceCoverage, ExtensionPresentation, FileChange, FileEntryId,
    FinalizeCompletion, FinalizeDecision, GoalAction, GoalState as ServeGoalState, GoalStore,
    GoalStoreError, HostCapabilities, HostDescriptor, HostId, HostService, InferenceRequest,
    InferenceRequestStore, InputModality, ItemDelta, ItemId, ItemLifecycle, ItemPayload,
    LifetimeUsage, LoopbackConfig, LoopbackServer, ModelInputPricing, ModelInputPricingTier,
    ModelSelection, ModelSummary, PendingRequest, PermanentDeleteConfirmation, ProcessTree,
    ProjectFileRead, ProjectFileSearchResult, ProjectFileSystem, ProjectFileSystemError,
    ProjectFileTree, ProjectFileWrite, ProjectId, ProjectRegistry, ProjectRegistryError,
    ProjectSummary, PromptInput, ProtocolValidation, PullRequestState, PullRequestSummary,
    RegistryProjectId, RegistryProjectState, RepositoryContextError, RepositoryContextSnapshot,
    RequestAnswer, RequestId, RequestKind, RequestState, RunId, RuntimeId, SearchDocument,
    SearchDocumentKind, SearchError, SemanticRole, ServiceError, SessionBranchEntry,
    SessionBranchEntryKind, SessionBranchGraph, SessionCatalogState, SessionCommand, SessionCursor,
    SessionDriver, SessionId, SessionItem, SessionLiveState, SessionRetention, SessionSeed,
    SessionSnapshot, SessionSummary, SessionSupervisor, SkillSuggestion, SlashCommandInvocation,
    SourceId, SourceKind, SourceRef, StoredAttachment, StoredResource, StructuredTestResults,
    SupervisorConfig, TerminationSignal, TestCommandOutcome, TestCommandStatus, TestFramework,
    TestOutputInput, ThemeColor, ThemeDensity, ThemeDto, ThemeId, ThemeMotion, ThemeOption,
    ThemeRoleStyle, ThemeSourceClass, ThemeTypography, TimestampedEvent, ToolActivity,
    ToolActivityStatus, ToolKind, ToolResultSummary, TranscriptSearchIndex,
    TranscriptSearchRequest, TranscriptSearchResult, TrustedFileEntry, TrustedFileError,
    TrustedFileIndexSummary, TrustedFileRead, TrustedFileSearchResult, TrustedProjectFiles, TurnId,
    UsageActivity, UsagePeriod, UsageSnapshot, UsageStats, UsageStoreError, UserMessageDelivery,
    MAX_ITEM_TEXT_BYTES, MAX_MODEL_INPUT_PRICING_TIERS, MAX_PROMPT_BYTES, MAX_TEST_OUTPUT_BYTES,
    PROTOCOL_VERSION,
};
use sexy_tui_rs::{Color as TuiColor, TextStyle as TuiTextStyle};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;
use tokio::sync::{mpsc, oneshot};

use crate::app::bootstrap::{
    build_app_with_runtime_manager, rebuild_app, LaunchSelection, SessionSelection,
};
use crate::app::{reasoning_label, supported_levels_with_subagents, App, Reconfig};
use crate::commands;
use crate::compaction::attempt_compaction;
use crate::config::{self, Config};
use crate::extensions::subagents_extension_activation_configured;
use crate::modes::HostRunOutcome;
use crate::resources::{compose_instructions, validate_skill_requirements};
use crate::session_store::{
    SessionCatalogEntry, SessionMeta, SessionStorageLifecycle, SessionStore, SessionUsageRecord,
};

const DRIVER_MAILBOX_CAPACITY: usize = 64;
const DRIVER_EVENT_CAPACITY: usize = 512;
const MAX_BUFFERED_DISCOVERY_EVENTS: usize = 64;
const DISCOVERY_BACKPRESSURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_GRAPHICAL_MODELS: usize = octet_serve_backend::MAX_MODELS;
const MAX_PROJECTED_SESSION_ITEMS: usize = 9_000;
const MAX_PROJECTED_BRANCH_ENTRIES: usize = 2_048;
const MAX_BRANCH_DELTA_ENTRIES: usize = 128;
const MAX_GRAPHICAL_SESSION_EXPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_OPAQUE_RESOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DELEGATION_TEAM_DIRECTORIES: usize = 2_048;
const MAX_DELEGATED_SESSIONS_PER_TEAM: usize = 256;
const MAX_DELEGATION_PROVENANCE_RECORDS: usize = 1_024;
const MAX_DELEGATION_PROVENANCE_LINE_BYTES: usize = 256 * 1024;
const DELEGATED_SESSION_PREFIX: &str = "agent-session:";
const EXTERNAL_EFFECTS_WARNING: &str = "Conversation branching changes only octet's transcript. Filesystem, command, network, and other external effects from later work are not rolled back.";

static NEXT_ACTOR_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Adapter from the optional Serve goal persistence to the provider-neutral
/// continuation driver. The actor owns when this adapter is invoked; the
/// store itself remains durable and frontend-independent.
#[derive(Clone)]
struct ServeGoalStore {
    store: GoalStore,
}

impl ServeGoalStore {
    fn session_id(raw: &str) -> Result<SessionId, String> {
        SessionId::new(raw.to_owned()).map_err(|_| "invalid session id".to_owned())
    }

    fn state(state: octet_serve_backend::GoalState) -> AgentGoalState {
        state
    }

    fn result(
        result: Result<octet_serve_backend::GoalState, octet_serve_backend::GoalStoreError>,
    ) -> Result<AgentGoalState, String> {
        result.map(Self::state).map_err(|error| error.to_string())
    }
}

impl AgentGoalStore for ServeGoalStore {
    fn get(&self, session_id: &str) -> Result<Option<AgentGoalState>, String> {
        let session_id = Self::session_id(session_id)?;
        self.store
            .get(&session_id)
            .map(|state| state.map(Self::state))
            .map_err(|error| error.to_string())
    }

    fn record_turn(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        Self::result(self.store.record_turn(&session_id))
    }

    fn mark_complete(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        Self::result(self.store.mark_complete(&session_id))
    }

    fn mark_blocked(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        Self::result(self.store.mark_blocked(&session_id))
    }

    fn pause(&self, session_id: &str) -> Result<AgentGoalState, String> {
        let session_id = Self::session_id(session_id)?;
        let state = self
            .store
            .apply(&session_id, GoalAction::Pause)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "goal was cleared".to_owned())?;
        Ok(Self::state(state))
    }
}

fn goal_service_error(error: GoalStoreError) -> ServiceError {
    match error {
        GoalStoreError::InvalidObjective
        | GoalStoreError::InvalidTurnBudget
        | GoalStoreError::NotFound
        | GoalStoreError::InvalidTransition => ServiceError::InvalidGoal,
        GoalStoreError::UnsafePath | GoalStoreError::CorruptState | GoalStoreError::Storage(_) => {
            ServiceError::Internal
        }
    }
}

fn goal_event(goal: Option<ServeGoalState>, revision: u64) -> TimestampedEvent {
    event(EventPayload::GoalChanged { goal, revision })
}

fn current_goal(
    store: Option<&GoalStore>,
    session_id: &SessionId,
) -> Result<Option<ServeGoalState>, ServiceError> {
    store
        .map(|store| store.get(session_id).map_err(goal_service_error))
        .transpose()
        .map(|goal| goal.flatten())
}

fn current_goal_event(
    store: Option<&GoalStore>,
    session_id: &SessionId,
) -> Result<TimestampedEvent, ServiceError> {
    let goal = current_goal(store, session_id)?;
    let revision = store
        .map(|store| store.revision(session_id).map_err(goal_service_error))
        .transpose()?
        .unwrap_or(0);
    Ok(goal_event(goal, revision))
}

fn apply_goal_command(
    store: &GoalStore,
    session_id: &SessionId,
    command: SessionCommand,
) -> Result<Option<ServeGoalState>, ServiceError> {
    match command {
        SessionCommand::SetGoal {
            objective,
            turn_budget,
        } => store
            .set(session_id, &objective, turn_budget)
            .map(Some)
            .map_err(goal_service_error),
        SessionCommand::PauseGoal => store
            .apply(session_id, GoalAction::Pause)
            .map_err(goal_service_error),
        SessionCommand::ResumeGoal => store
            .apply(session_id, GoalAction::Resume)
            .map_err(goal_service_error),
        SessionCommand::ClearGoal => store
            .apply(session_id, GoalAction::Clear)
            .map_err(goal_service_error),
        _ => Err(ServiceError::InvalidBoundary),
    }
}

fn goal_deadline_after_user_change(
    goal_driver: Option<&GoalDriver>,
) -> Result<Option<tokio::time::Instant>, ServiceError> {
    let Some(goal_driver) = goal_driver else {
        return Ok(None);
    };
    goal_driver.user_spoke();
    match goal_driver
        .turn_settled(GoalTurnSource::User, "", false)
        .map_err(|_| ServiceError::Internal)?
    {
        GoalDecision::Wait { delay, .. } => Ok(Some(tokio::time::Instant::now() + delay)),
        _ => Ok(None),
    }
}

fn goal_mutation_outcome(
    plan: &WorkerPlan,
    command: SessionCommand,
) -> Result<DriverCommandOutcome, ServiceError> {
    let Some(store) = plan.goal_store.as_ref() else {
        return Err(ServiceError::InvalidBoundary);
    };
    let goal = apply_goal_command(store, &plan.session_id, command)?;
    let revision = store
        .revision(&plan.session_id)
        .map_err(goal_service_error)?;
    Ok(DriverCommandOutcome::with_events(vec![goal_event(
        goal, revision,
    )]))
}

mod conversations;
use conversations::*;
mod projects;
use projects::*;
mod recovery;
use recovery::*;
mod runs;
use runs::{run_worker, WorkerCommand, WorkerMessage, WorkerPlan};
mod sessions;
use sessions::*;
mod routing;
mod startup;

pub use startup::run_with_session_name;

fn authority_profiles_from_sandbox(
    sandbox: &crate::config::SandboxPolicy,
) -> Vec<AuthorityProfile> {
    // Serve has one immutable launch policy, not per-session sandboxes. Do not
    // offer narrower labels without enforcing them across tools, extensions,
    // delegated children, and the host-wide terminal.
    vec![authority_ceiling_from_sandbox(sandbox)]
}

fn authority_ceiling_from_sandbox(sandbox: &crate::config::SandboxPolicy) -> AuthorityProfile {
    let can_mutate_files = sandbox.allow_edit || sandbox.allow_write;
    if sandbox.process_execution_allowed() || (can_mutate_files && sandbox.allow_external_paths) {
        // A confined built-in path policy cannot contain shells or extensions.
        AuthorityProfile::FullAccess
    } else if can_mutate_files {
        AuthorityProfile::Workspace
    } else {
        AuthorityProfile::ReadOnly
    }
}

#[derive(Clone)]
struct OctetHost {
    config: Config,
    catalog: ModelCatalog,
    models: Vec<ModelSummary>,
    descriptor: HostDescriptor,
    projects: Arc<Mutex<ProjectRegistry>>,
    launch_project_id: ProjectId,
    themes: Vec<ThemeOption>,
    selected_theme_id: ThemeId,
    attachments: Option<AttachmentStore>,
    documents: Option<DocumentStore>,
    goals: GoalStore,
    trusted_files: Arc<Mutex<HashMap<String, TrustedProjectFiles>>>,
    search_index: Arc<Mutex<TranscriptSearchIndex>>,
    search_index_initialized: Arc<AtomicBool>,
    resources: Option<octet_serve_backend::ResourceStore>,
    usage: Arc<Mutex<InferenceRequestStore>>,
    pull_requests: Arc<Mutex<PullRequestStore>>,
    serve_state_dir: PathBuf,
    session_deletion_lock: Arc<tokio::sync::Mutex<()>>,
    startup_session_name: Arc<Mutex<Option<String>>>,
    #[cfg(test)]
    checkout_hooks: Arc<Mutex<VecDeque<CheckoutTestHooks>>>,
    #[cfg(test)]
    open_count: Arc<AtomicU64>,
}

struct ProjectContext {
    project_id: ProjectId,
    config: Config,
    sessions: SessionStore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DelegatedSessionFingerprint {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl DelegatedSessionFingerprint {
    fn from_metadata(metadata: &std::fs::Metadata) -> Result<Self, ServiceError> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().map_err(|_| ServiceError::InvalidSeed)?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }

    fn same_file_as(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(not(unix))]
        {
            let _ = other;
            false
        }
    }
}

#[derive(Default)]
struct DelegatedSessionProvenance {
    display_task_name: Option<String>,
    parent_session_id: Option<String>,
    extension_principal: Option<String>,
    extension_resource_owner: Option<String>,
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_extension_delegation_principal(value: &str) -> bool {
    let Some((name, digest)) = value.split_once("@sha256:") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && is_lower_hex_digest(digest)
}

fn is_extension_resource_owner(value: &str) -> bool {
    value
        .strip_prefix("session-")
        .is_some_and(is_lower_hex_digest)
}

fn delegated_session_provenance(team: &Path, child: &Path) -> DelegatedSessionProvenance {
    let Ok(file) =
        octet_agent::secure_fs::open_private_file_for_read(&team.join("provenance.jsonl"))
    else {
        return DelegatedSessionProvenance::default();
    };
    let mut reader = BufReader::new(file);
    let Some(expected_reference) = octet_agent::delegated_session_reference(child) else {
        return DelegatedSessionProvenance::default();
    };
    let mut result = DelegatedSessionProvenance::default();
    for _ in 0..MAX_DELEGATION_PROVENANCE_RECORDS {
        let mut line = Vec::new();
        loop {
            let Ok(available) = reader.fill_buf() else {
                return DelegatedSessionProvenance::default();
            };
            if available.is_empty() {
                break;
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if line.len().saturating_add(take) > MAX_DELEGATION_PROVENANCE_LINE_BYTES {
                return DelegatedSessionProvenance::default();
            }
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        if line.is_empty() {
            break;
        }
        let Ok(record) = serde_json::from_slice::<serde_json::Value>(&line) else {
            return DelegatedSessionProvenance::default();
        };
        if record.get("event").and_then(serde_json::Value::as_str) != Some("agent_spawned") {
            continue;
        }
        let matches_child = record
            .get("session_reference")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|reference| reference == expected_reference);
        if matches_child {
            result.display_task_name = record
                .get("display_task_name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= 48
                        && name.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'_' | b'-')
                        })
                })
                .map(str::to_owned);
            result.parent_session_id = record
                .get("extension_parent_session_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| SessionId::new(*id).is_ok())
                .map(str::to_owned);
            result.extension_principal = record
                .get("extension_principal")
                .and_then(serde_json::Value::as_str)
                .filter(|principal| is_extension_delegation_principal(principal))
                .map(str::to_owned);
            result.extension_resource_owner = record
                .get("extension_resource_owner")
                .and_then(serde_json::Value::as_str)
                .filter(|owner| is_extension_resource_owner(owner))
                .map(str::to_owned);
            break;
        }
    }
    result
}

struct DelegatedSessionContext {
    project_id: ProjectId,
    parent_session_id: SessionId,
    config: Config,
    session: Session,
    meta: SessionMeta,
    fingerprint: DelegatedSessionFingerprint,
}

const SESSION_DELETION_VERSION: u16 = 1;
const SESSION_DELETION_DIRECTORY: &str = "session-deletions-v1";
const MAX_SESSION_DELETION_RECORD_BYTES: u64 = 4 * 1024;
const PULL_REQUEST_STORE_VERSION: u16 = 1;
const PULL_REQUEST_STORE_FILE: &str = "pull-requests-v1.json";
const MAX_PULL_REQUEST_STORE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PULL_REQUEST_RECORDS: usize = 2_000;
const MAX_GITHUB_CLI_OUTPUT_BYTES: u64 = 16 * 1024;
const MAX_CONCURRENT_GITHUB_QUERIES: usize = 4;
const GITHUB_CLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
const GITHUB_CLI_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);
const GITHUB_CLI_FORCE_PERIOD: std::time::Duration = std::time::Duration::from_millis(400);
const GITHUB_CLI_CLEANUP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);
const PULL_REQUEST_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Explicitly retained values for the host-owned `gh` helper. Provider
/// credentials, dynamic-loader controls, and arbitrary dotenv values must not
/// cross this boundary.
const GITHUB_CLI_INHERITED_ENVIRONMENT: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_CONFIG_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "GIT_SSL_CAPATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_COLLATE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "__CF_USER_TEXT_ENCODING",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TERM",
    "COLORTERM",
];

#[cfg(windows)]
const GITHUB_CLI_EXECUTABLE_NAMES: &[&str] = &["gh.exe", "gh"];
#[cfg(not(windows))]
const GITHUB_CLI_EXECUTABLE_NAMES: &[&str] = &["gh"];

static GITHUB_QUERY_PERMITS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_GITHUB_QUERIES);

fn external_github_path_directory(root: &Path, directory: &Path) -> Option<PathBuf> {
    if !directory.is_absolute() || directory.starts_with(root) {
        return None;
    }
    let directory = directory.canonicalize().ok()?;
    if !directory.is_absolute()
        || directory.starts_with(root)
        || !directory.symlink_metadata().ok()?.is_dir()
    {
        return None;
    }
    Some(directory)
}

fn resolve_github_cli_executable_from_path(workspace: &Path, path: &OsStr) -> Option<PathBuf> {
    let root = workspace.canonicalize().ok()?;
    if !root.is_absolute() || !root.symlink_metadata().ok()?.is_dir() {
        return None;
    }
    for raw_directory in std::env::split_paths(path) {
        let Some(directory) = external_github_path_directory(&root, &raw_directory) else {
            continue;
        };
        for name in GITHUB_CLI_EXECUTABLE_NAMES {
            let Ok(candidate) = directory.join(name).canonicalize() else {
                continue;
            };
            if !candidate.is_absolute() || candidate.starts_with(&root) {
                continue;
            }
            let Ok(metadata) = candidate.symlink_metadata() else {
                continue;
            };
            let file_type = metadata.file_type();
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
}

fn resolve_github_cli_executable(workspace: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    resolve_github_cli_executable_from_path(workspace, &path)
}

fn sanitized_github_cli_path_from(workspace: &Path, path: &OsStr) -> Option<OsString> {
    let root = workspace.canonicalize().ok()?;
    let mut directories = Vec::new();
    for raw_directory in std::env::split_paths(path) {
        let Some(directory) = external_github_path_directory(&root, &raw_directory) else {
            continue;
        };
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    (!directories.is_empty())
        .then(|| std::env::join_paths(directories).ok())
        .flatten()
}

fn github_cli_environment_from(
    workspace: &Path,
    mut get: impl FnMut(&str) -> Option<OsString>,
    path: Option<&OsStr>,
) -> BTreeMap<OsString, OsString> {
    let mut environment = GITHUB_CLI_INHERITED_ENVIRONMENT
        .iter()
        .filter_map(|name| get(name).map(|value| (OsString::from(*name), value)))
        .collect::<BTreeMap<_, _>>();
    if let Some(path) = path.and_then(|path| sanitized_github_cli_path_from(workspace, path)) {
        environment.insert(OsString::from("PATH"), path);
    }
    environment
}

fn github_cli_environment(workspace: &Path) -> BTreeMap<OsString, OsString> {
    let path = std::env::var_os("PATH");
    github_cli_environment_from(workspace, |name| std::env::var_os(name), path.as_deref())
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PullRequestIdentity {
    host: String,
    port: u16,
    owner: String,
    repository: String,
    number: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredPullRequest {
    session_id: String,
    url: String,
    number: u64,
    state: PullRequestState,
    refreshed_at_ms: u64,
}

impl StoredPullRequest {
    fn summary(&self) -> PullRequestSummary {
        PullRequestSummary { state: self.state }
    }

    fn validate(&self) -> bool {
        SessionId::new(self.session_id.clone()).is_ok()
            && self.number > 0
            && self.refreshed_at_ms > 0
            && pull_request_url_is_valid(&self.url, self.number)
    }
}

fn pull_request_identity(value: &str, number: u64) -> Option<PullRequestIdentity> {
    if value.len() > 2_048 || number == 0 {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    let path_segments = url.path_segments()?.collect::<Vec<_>>();
    let host = url.host_str()?;
    let path_matches = path_segments.len() == 4
        && path_segments.iter().all(|segment| !segment.is_empty())
        && path_segments[..2]
            .iter()
            .all(|segment| !segment.contains('%'))
        && path_segments[2] == "pull"
        && path_segments[3] == number.to_string();
    if url.scheme() != "https"
        || url.cannot_be_a_base()
        || host.is_empty()
        || host.ends_with('.')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !path_matches
    {
        return None;
    }
    Some(PullRequestIdentity {
        host: host.to_ascii_lowercase(),
        port: url.port_or_known_default()?,
        owner: path_segments[0].to_ascii_lowercase(),
        repository: path_segments[1].to_ascii_lowercase(),
        number,
    })
}

fn pull_request_url_is_valid(value: &str, number: u64) -> bool {
    pull_request_identity(value, number).is_some()
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredPullRequestCatalog {
    version: u16,
    #[serde(deserialize_with = "deserialize_unique_pull_request_records")]
    records: BTreeMap<String, StoredPullRequest>,
}

fn deserialize_unique_pull_request_records<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, StoredPullRequest>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueRecordsVisitor;

    impl<'de> serde::de::Visitor<'de> for UniqueRecordsVisitor {
        type Value = BTreeMap<String, StoredPullRequest>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a pull-request record map with unique session IDs")
        }

        fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut records = BTreeMap::new();
            while let Some((session_id, pull_request)) = entries.next_entry()? {
                if records.insert(session_id, pull_request).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate pull-request session ID",
                    ));
                }
            }
            Ok(records)
        }
    }

    deserializer.deserialize_map(UniqueRecordsVisitor)
}

struct PullRequestStore {
    path: PathBuf,
    records: BTreeMap<String, StoredPullRequest>,
    catalog_changes: BTreeSet<String>,
    deleted_sessions: BTreeSet<String>,
}

impl PullRequestStore {
    fn empty(serve_state_dir: &Path) -> Self {
        Self {
            path: serve_state_dir.join(PULL_REQUEST_STORE_FILE),
            records: BTreeMap::new(),
            catalog_changes: BTreeSet::new(),
            deleted_sessions: BTreeSet::new(),
        }
    }

    fn open(serve_state_dir: &Path) -> anyhow::Result<Self> {
        let path = serve_state_dir.join(PULL_REQUEST_STORE_FILE);
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty(serve_state_dir));
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_PULL_REQUEST_STORE_BYTES
        {
            anyhow::bail!("pull-request evidence store is unsafe");
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&path)?;
        let opened_metadata = file.metadata()?;
        if !opened_metadata.is_file() || opened_metadata.len() > MAX_PULL_REQUEST_STORE_BYTES {
            anyhow::bail!("pull-request evidence store changed during validation");
        }
        let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
        file.take(MAX_PULL_REQUEST_STORE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PULL_REQUEST_STORE_BYTES {
            anyhow::bail!("pull-request evidence store is too large");
        }
        let catalog = serde_json::from_slice::<StoredPullRequestCatalog>(&bytes)?;
        let mut identities = BTreeSet::new();
        if catalog.version != PULL_REQUEST_STORE_VERSION
            || catalog.records.len() > MAX_PULL_REQUEST_RECORDS
            || catalog.records.iter().any(|(session_id, record)| {
                session_id != &record.session_id
                    || !record.validate()
                    || match pull_request_identity(&record.url, record.number) {
                        Some(identity) => !identities.insert(identity),
                        None => true,
                    }
            })
        {
            anyhow::bail!("pull-request evidence store is invalid");
        }
        Ok(Self {
            path,
            records: catalog.records,
            catalog_changes: BTreeSet::new(),
            deleted_sessions: BTreeSet::new(),
        })
    }

    fn get(&self, session_id: &SessionId) -> Option<StoredPullRequest> {
        self.records.get(session_id.as_str()).cloned()
    }

    fn summary(&self, session_id: &SessionId) -> Option<PullRequestSummary> {
        self.records
            .get(session_id.as_str())
            .map(StoredPullRequest::summary)
    }

    fn summaries(&self) -> BTreeMap<String, PullRequestSummary> {
        self.records
            .iter()
            .map(|(session_id, pull_request)| (session_id.clone(), pull_request.summary()))
            .collect()
    }

    fn refreshable(&self) -> Vec<StoredPullRequest> {
        let mut pull_requests = self
            .records
            .values()
            .filter(|pull_request| pull_request.state != PullRequestState::Merged)
            .cloned()
            .collect::<Vec<_>>();
        // Oldest evidence goes first so a permit race cannot repeatedly favor
        // the same session-ID prefix while the trailing inventory stays stale.
        pull_requests.sort_by(|left, right| {
            left.refreshed_at_ms
                .cmp(&right.refreshed_at_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        pull_requests
    }

    fn take_catalog_changes(&mut self) -> BTreeSet<SessionId> {
        std::mem::take(&mut self.catalog_changes)
            .into_iter()
            .map(|session_id| SessionId::new(session_id).expect("stored pull-request session ID"))
            .collect()
    }

    fn replace(
        &mut self,
        session_id: &SessionId,
        pull_request: Option<StoredPullRequest>,
    ) -> anyhow::Result<()> {
        self.transaction(|store| store.replace_unpersisted(session_id, pull_request))
    }

    fn delete_session(&mut self, session_id: &SessionId) -> anyhow::Result<()> {
        // A hosted refresh may already be finishing on the blocking pool when
        // actor retirement begins. Fence the identity before removal so that a
        // late first-discovery result cannot recreate evidence after permanent
        // session deletion.
        self.deleted_sessions.insert(session_id.as_str().to_owned());
        if self.records.contains_key(session_id.as_str()) {
            self.replace(session_id, None)?;
        }
        Ok(())
    }

    fn replace_unpersisted(
        &mut self,
        session_id: &SessionId,
        pull_request: Option<StoredPullRequest>,
    ) -> anyhow::Result<()> {
        let previous_summary = self.summary(session_id);
        if let Some(pull_request) = pull_request.as_ref() {
            if self.deleted_sessions.contains(session_id.as_str()) {
                anyhow::bail!("pull-request session was permanently deleted");
            }
            if self.records.len() >= MAX_PULL_REQUEST_RECORDS
                && !self.records.contains_key(session_id.as_str())
            {
                anyhow::bail!("pull-request evidence store is full");
            }
            if pull_request.session_id != session_id.as_str() || !pull_request.validate() {
                anyhow::bail!("pull-request evidence is invalid");
            }
            let identity = pull_request_identity(&pull_request.url, pull_request.number)
                .ok_or_else(|| anyhow::anyhow!("pull-request evidence is invalid"))?;
            if self.records.iter().any(|(other_session_id, other)| {
                other_session_id != session_id.as_str()
                    && pull_request_identity(&other.url, other.number).as_ref() == Some(&identity)
            }) {
                anyhow::bail!("pull-request evidence is already associated with another session");
            }
        }
        match pull_request {
            Some(pull_request) => self
                .records
                .insert(session_id.as_str().to_owned(), pull_request),
            None => self.records.remove(session_id.as_str()),
        };
        if self.summary(session_id) != previous_summary {
            self.catalog_changes.insert(session_id.as_str().to_owned());
        }
        Ok(())
    }

    fn transaction<T>(
        &mut self,
        update: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let previous_records = self.records.clone();
        let previous_catalog_changes = self.catalog_changes.clone();
        let outcome = match update(self) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.records = previous_records;
                self.catalog_changes = previous_catalog_changes;
                return Err(error);
            }
        };
        if self.records == previous_records {
            self.catalog_changes = previous_catalog_changes;
        } else if let Err(error) = self.persist() {
            self.records = previous_records;
            self.catalog_changes = previous_catalog_changes;
            return Err(error);
        }
        Ok(outcome)
    }

    fn persist(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&StoredPullRequestCatalog {
            version: PULL_REQUEST_STORE_VERSION,
            records: self.records.clone(),
        })?;
        if bytes.len() as u64 > MAX_PULL_REQUEST_STORE_BYTES {
            anyhow::bail!("pull-request evidence store is too large");
        }
        let directory = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("pull-request evidence store has no parent"))?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)?;
        let temporary = directory.join(format!(".pull-requests-{}", stable_hash(&random)));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary)?;
        let result = (|| -> anyhow::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            std::fs::File::open(directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GitHubPullRequest {
    number: u64,
    url: String,
    state: String,
    is_draft: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PullRequestObservation {
    Trackable {
        number: u64,
        url: String,
        state: PullRequestState,
    },
    Closed {
        number: u64,
        url: String,
    },
    Unavailable,
}

impl OctetHost {
    #[cfg(test)]
    fn new(config: Config) -> anyhow::Result<Self> {
        Self::new_with_session_name(config, None)
    }

    fn new_with_session_name(config: Config, session_name: Option<String>) -> anyhow::Result<Self> {
        #[cfg(not(unix))]
        anyhow::bail!(
            "octet serve project trust is unavailable on this platform because stable directory identity checks are not implemented"
        );
        let startup_session_name = startup::normalize_startup_session_name(session_name)?;
        let boot = crate::app::bootstrap::bootstrap(config.clone())?;
        let models = graphical_model_catalog(&boot.catalog, &config);
        if models.is_empty() {
            anyhow::bail!("no configured models are available for octet serve");
        }
        let host_id = load_or_create_host_id(&config)?;
        let workspace_name = config
            .workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace");
        let descriptor = HostDescriptor {
            id: host_id,
            name: octet_serve_backend::sanitize_public_text(
                &format!("octet — {workspace_name}"),
                256,
                false,
            ),
        };
        let (themes, selected_theme_id) = graphical_themes(&config)?;
        let state_dir = secure_serve_state_dir(&config.session_dir)?;
        let mut projects = ProjectRegistry::open(state_dir.join("projects"))?;
        let launch_project = match projects.find_by_root(&config.workspace)? {
            Some(project) => project,
            None => projects.import(&config.workspace, Some(workspace_name))?,
        };
        if config.workspace_trusted && launch_project.state == RegistryProjectState::Untrusted {
            projects.grant_trust(&launch_project.id)?;
        }
        reconcile_session_bindings(&config, &mut projects, Some(&launch_project.id))?;
        if projects.default_project().is_none()
            && launch_project.state != RegistryProjectState::Archived
        {
            projects.set_default(&launch_project.id)?;
        }
        let launch_project_id =
            ProjectId::new(launch_project.id.as_str()).map_err(anyhow::Error::msg)?;
        let attachments = match AttachmentStore::open(&state_dir) {
            Ok(store) => Some(store),
            Err(_) => {
                crate::output::stderr_line(
                    "warning: secure attachment storage is unavailable; image uploads are disabled",
                );
                None
            }
        };
        let documents = match DocumentStore::open(&state_dir) {
            Ok(store) => Some(store),
            Err(_) => {
                crate::output::stderr_line(
                    "warning: secure document storage is unavailable; text, Markdown, and PDF uploads are disabled",
                );
                None
            }
        };
        let goals = GoalStore::open(&state_dir.join("goals"))?;
        let resources = match octet_serve_backend::ResourceStore::open(&state_dir) {
            Ok(store) => Some(store),
            Err(_) => {
                crate::output::stderr_line(
                    "warning: secure evidence storage is unavailable; durable sources and outputs are disabled",
                );
                None
            }
        };
        let mut usage = InferenceRequestStore::open(&state_dir)?;
        backfill_usage_store(&config, &projects, &mut usage)?;
        let pull_requests = PullRequestStore::open(&state_dir)
            .context("failed to open stored pull-request evidence")?;
        let host = Self {
            config,
            catalog: boot.catalog,
            models,
            descriptor,
            projects: Arc::new(Mutex::new(projects)),
            launch_project_id,
            themes,
            selected_theme_id,
            attachments,
            documents,
            goals,
            trusted_files: Arc::new(Mutex::new(HashMap::new())),
            search_index: Arc::new(Mutex::new(TranscriptSearchIndex::new())),
            search_index_initialized: Arc::new(AtomicBool::new(false)),
            resources,
            usage: Arc::new(Mutex::new(usage)),
            pull_requests: Arc::new(Mutex::new(pull_requests)),
            serve_state_dir: state_dir,
            session_deletion_lock: Arc::new(tokio::sync::Mutex::new(())),
            startup_session_name: Arc::new(Mutex::new(startup_session_name)),
            #[cfg(test)]
            checkout_hooks: Arc::new(Mutex::new(VecDeque::new())),
            #[cfg(test)]
            open_count: Arc::new(AtomicU64::new(0)),
        };
        host.recover_pending_session_deletions();
        Ok(host)
    }

    fn cleanup_session_sidecars(&self, project_id: &ProjectId, session_id: &SessionId) -> bool {
        // InferenceRequestStore is intentionally excluded: its
        // conversation-content-free, append-only records are host-level
        // accounting history, not replayable session content. Permanent
        // deletion removes every session-rehydratable
        // sidecar while preserving lifetime usage totals.
        let mut complete = true;
        match &self.attachments {
            Some(store) => complete &= store.delete_session(session_id).is_ok(),
            None => complete = false,
        }
        match &self.documents {
            Some(store) => {
                complete &= store
                    .delete_session(project_id.as_str(), session_id.as_str())
                    .is_ok();
            }
            None => complete = false,
        }
        match &self.resources {
            Some(store) => complete &= store.delete_session(session_id).is_ok(),
            None => complete = false,
        }
        complete &= self.goals.delete_session(session_id).is_ok();
        match self.search_index.lock() {
            Ok(mut search_index) => {
                complete &= search_index.remove_session(session_id.as_str()).is_ok();
            }
            Err(_) => complete = false,
        }
        match self.pull_requests.lock() {
            Ok(mut pull_requests) => {
                complete &= pull_requests.delete_session(session_id).is_ok();
            }
            Err(_) => complete = false,
        }
        complete
    }

    fn recover_pending_session_deletions(&self) {
        // Construction performs recovery before the host is published, so the
        // deletion mutex must be immediately available. Keep recovery under
        // the same lock as live deletion in case this method gains another
        // caller later.
        let Ok(_deletion_guard) = self.session_deletion_lock.try_lock() else {
            crate::output::stderr_line(
                "warning: pending permanent session deletions are already being recovered",
            );
            return;
        };
        let Ok(records) = load_pending_session_deletions(&self.serve_state_dir) else {
            crate::output::stderr_line(
                "warning: pending permanent session deletions could not be inspected",
            );
            return;
        };
        for mut record in records {
            let Ok(session_id) = SessionId::new(record.session_id.clone()) else {
                continue;
            };
            let Ok(project_id) = ProjectId::new(record.project_id.clone()) else {
                continue;
            };
            let Ok(registry_id) = RegistryProjectId::parse(record.project_id.clone()) else {
                continue;
            };
            let sessions = {
                let Ok(projects) = self.projects.lock() else {
                    continue;
                };
                let Ok(root) = projects.resolve_root_for_cleanup(&registry_id) else {
                    continue;
                };
                SessionStore::new(&self.config.session_dir, root.as_path())
            };

            if !record.committed {
                match sessions.session_file_exists(session_id.as_str()) {
                    Ok(true) => {
                        let rolled_back = sessions
                            .rollback_permanent_delete(session_id.as_str())
                            .is_ok();
                        let rebound = rolled_back
                            && self.projects.lock().is_ok_and(|mut projects| {
                                projects
                                    .bind_session(session_id.as_str(), &registry_id)
                                    .is_ok()
                            });
                        if rebound
                            && remove_pending_session_deletion(
                                &self.serve_state_dir,
                                session_id.as_str(),
                            )
                            .is_ok()
                        {
                            continue;
                        }
                        crate::output::stderr_line(format!(
                            "warning: pre-commit permanent deletion rollback for session {} remains pending",
                            session_id.as_str()
                        ));
                        continue;
                    }
                    Ok(false) => {
                        record.committed = true;
                        let _ = write_pending_session_deletion(&self.serve_state_dir, &record);
                    }
                    Err(_) => {
                        crate::output::stderr_line(format!(
                            "warning: pre-commit permanent deletion for session {} could not inspect its transcript and remains pending",
                            session_id.as_str()
                        ));
                        continue;
                    }
                }
            }

            let primary_clean = sessions
                .finish_permanent_delete(session_id.as_str())
                .is_ok();
            let unbound = self
                .projects
                .lock()
                .is_ok_and(|mut projects| projects.unbind_session(session_id.as_str()).is_ok());
            let sidecars_clean = self.cleanup_session_sidecars(&project_id, &session_id);
            if primary_clean && unbound && sidecars_clean {
                let _ = remove_pending_session_deletion(&self.serve_state_dir, session_id.as_str());
            } else {
                crate::output::stderr_line(format!(
                    "warning: permanent deletion cleanup for session {} remains pending",
                    session_id.as_str()
                ));
            }
        }
    }

    fn cached_pull_request(&self, session_id: &SessionId) -> Option<PullRequestSummary> {
        self.pull_requests
            .lock()
            .ok()
            .and_then(|pull_requests| pull_requests.summary(session_id))
    }

    fn stored_session_summary(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionSummary, ServiceError> {
        let context = self.storage_context_for_session(session_id)?;
        let catalog = context
            .sessions
            .catalog_by_id(session_id.as_str())
            .map_err(|_| ServiceError::InvalidSeed)?;
        let meta = catalog.meta.as_ref().ok_or(ServiceError::NotFound)?;
        let selection = advertised_selection_from_catalog_entry(
            &catalog,
            &self.catalog,
            &context.config,
            &self.models,
        )
        .map_or_else(|| self.default_selection(), Ok)?;
        let mut summary = summary_from_meta(meta, Some(context.project_id), selection)?;
        summary.pull_request = self.cached_pull_request(session_id);
        Ok(summary)
    }

    fn default_selection(&self) -> Result<ModelSelection, ServiceError> {
        let summary = self
            .config
            .model
            .as_ref()
            .and_then(|model_id| self.models.iter().find(|summary| summary.id == model_id.0))
            .or_else(|| self.models.first())
            .ok_or(ServiceError::InvalidSeed)?;
        Ok(selection_from_summary(summary))
    }

    fn project_context(
        &self,
        requested: Option<&ProjectId>,
    ) -> Result<ProjectContext, ServiceError> {
        let projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
        let registry_id = match requested {
            Some(project_id) => registry_project_id(project_id)?,
            None => {
                let default = projects.default_project().map(|project| project.id);
                let mut candidates = default.into_iter().collect::<Vec<_>>();
                let launch_project_id = registry_project_id(&self.launch_project_id)?;
                if !candidates.contains(&launch_project_id) {
                    candidates.push(launch_project_id);
                }
                for project_id in projects.list().into_iter().map(|project| project.id) {
                    if !candidates.contains(&project_id) {
                        candidates.push(project_id);
                    }
                }
                candidates
                    .into_iter()
                    .find(|project_id| projects.resolve_trusted_root(project_id).is_ok())
                    .ok_or(ServiceError::Unauthorized)?
            }
        };
        let root = projects
            .resolve_trusted_root(&registry_id)
            .map_err(project_registry_service_error)?;
        let project_id =
            ProjectId::new(registry_id.as_str()).map_err(|_| ServiceError::Internal)?;
        let mut config = self.config.clone();
        config.workspace = root.as_path().to_owned();
        config.invocation_cwd = root.as_path().to_owned();
        config.workspace_trusted = true;
        let sessions = SessionStore::new(&config.session_dir, root.as_path());
        Ok(ProjectContext {
            project_id,
            config,
            sessions,
        })
    }

    fn project_context_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<ProjectContext, ServiceError> {
        let project_id = {
            let mut projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
            if projects.project_for_session(session_id.as_str()).is_none() {
                reconcile_session_bindings(&self.config, &mut projects, None)
                    .map_err(project_registry_service_error)?;
            }
            projects
                .project_for_session(session_id.as_str())
                .ok_or(ServiceError::NotFound)?
        };
        let project_id = ProjectId::new(project_id.as_str()).map_err(|_| ServiceError::Internal)?;
        self.project_context(Some(&project_id))
    }

    fn storage_context_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<ProjectContext, ServiceError> {
        let projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
        let registry_id = projects
            .project_for_session(session_id.as_str())
            .ok_or(ServiceError::NotFound)?;
        let root = projects
            .resolve_root(&registry_id)
            .map_err(project_registry_service_error)?;
        let project_id =
            ProjectId::new(registry_id.as_str()).map_err(|_| ServiceError::Internal)?;
        let mut config = self.config.clone();
        config.workspace = root.as_path().to_owned();
        config.invocation_cwd = root.as_path().to_owned();
        config.workspace_trusted = false;
        let sessions = SessionStore::new(&config.session_dir, root.as_path());
        Ok(ProjectContext {
            project_id,
            config,
            sessions,
        })
    }

    fn authorize_delegated_session(
        &self,
        provenance: &DelegatedSessionProvenance,
        child: &Path,
        project_id: &ProjectId,
        sessions: &SessionStore,
    ) -> Result<SessionId, ServiceError> {
        let parent_session_id = provenance
            .parent_session_id
            .as_deref()
            .ok_or(ServiceError::NotFound)?;
        let principal = provenance
            .extension_principal
            .as_deref()
            .ok_or(ServiceError::NotFound)?;
        let resource_owner = provenance
            .extension_resource_owner
            .as_deref()
            .ok_or(ServiceError::NotFound)?;
        if !octet_agent::extension_delegated_session_matches_owner(principal, resource_owner, child)
        {
            return Err(ServiceError::NotFound);
        }
        let parent_is_bound = self
            .projects
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .project_for_session(parent_session_id)
            .is_some_and(|bound| bound.as_str() == project_id.as_str());
        if !parent_is_bound {
            return Err(ServiceError::NotFound);
        }
        let parent_path = sessions
            .path_by_id(parent_session_id)
            .map_err(|_| ServiceError::NotFound)?;
        let parent_file = octet_agent::secure_fs::open_private_file_for_read(&parent_path)
            .map_err(|_| ServiceError::NotFound)?;
        let parent = Session::open_read_only_with_file(parent_path, parent_file)
            .map_err(|_| ServiceError::NotFound)?;
        if parent.resource_owner_key() != resource_owner {
            return Err(ServiceError::NotFound);
        }
        SessionId::new(parent_session_id).map_err(|_| ServiceError::NotFound)
    }

    fn delegated_session_context(
        &self,
        session_id: &SessionId,
    ) -> Result<DelegatedSessionContext, ServiceError> {
        // The opaque digest is only a lookup key. Authorization is separate:
        // the matched child must carry host-written extension provenance that
        // binds its parent session, path-free extension principal, and exact
        // parent resource owner. Native delegation children and forged or
        // incomplete retained records therefore remain undiscoverable here.
        let digest = session_id
            .as_str()
            .strip_prefix(DELEGATED_SESSION_PREFIX)
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(ServiceError::NotFound)?;
        let expected_reference = format!("{DELEGATED_SESSION_PREFIX}{digest}");
        let project_roots = {
            let projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
            projects
                .list()
                .into_iter()
                .filter_map(|project| {
                    let root = projects.resolve_root(&project.id).ok()?;
                    let project_id = ProjectId::new(project.id.as_str()).ok()?;
                    Some((
                        project_id,
                        root.as_path().to_owned(),
                        project.state == RegistryProjectState::Trusted,
                    ))
                })
                .collect::<Vec<_>>()
        };

        let mut matched = None;
        for (project_id, root, trusted) in project_roots {
            let mut config = self.config.clone();
            config.workspace = root.clone();
            config.invocation_cwd = root.clone();
            config.workspace_trusted = trusted;
            let sessions = SessionStore::new(&config.session_dir, &root);
            let delegation_root = sessions.dir().join(".delegation");
            let Ok(metadata) = delegation_root.symlink_metadata() else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let mut teams = std::fs::read_dir(&delegation_root)
                .map_err(|_| ServiceError::Unavailable)?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_type()
                        .is_ok_and(|file_type| file_type.is_dir() && !file_type.is_symlink())
                })
                .collect::<Vec<_>>();
            if teams.len() > MAX_DELEGATION_TEAM_DIRECTORIES {
                return Err(ServiceError::PayloadTooLarge);
            }
            teams.sort_by_key(std::fs::DirEntry::file_name);
            for team in teams {
                let team_name = team.file_name();
                let Some(team_name) = team_name.to_str() else {
                    continue;
                };
                if !team_name.starts_with("team-")
                    || team_name.len() > 128
                    || !team_name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    continue;
                }
                let team_path = team.path();
                let mut children = std::fs::read_dir(&team_path)
                    .map_err(|_| ServiceError::Unavailable)?
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_type()
                            .is_ok_and(|file_type| file_type.is_file() && !file_type.is_symlink())
                            && entry.path().extension().and_then(|value| value.to_str())
                                == Some("jsonl")
                            && entry.file_name() != "provenance.jsonl"
                    })
                    .collect::<Vec<_>>();
                if children.len() > MAX_DELEGATED_SESSIONS_PER_TEAM {
                    return Err(ServiceError::PayloadTooLarge);
                }
                children.sort_by_key(std::fs::DirEntry::file_name);
                for child in children {
                    let path = child.path();
                    if octet_agent::delegated_session_reference(&path).as_deref()
                        != Some(expected_reference.as_str())
                    {
                        continue;
                    }
                    if matched.is_some() {
                        return Err(ServiceError::CorruptResource);
                    }
                    let provenance = delegated_session_provenance(&team_path, &path);
                    let parent_session_id = self.authorize_delegated_session(
                        &provenance,
                        &path,
                        &project_id,
                        &sessions,
                    )?;
                    let file = octet_agent::secure_fs::open_private_file_for_read(&path)
                        .map_err(|_| ServiceError::NotFound)?;
                    let file_metadata = file.metadata().map_err(|_| ServiceError::InvalidSeed)?;
                    let fingerprint = DelegatedSessionFingerprint::from_metadata(&file_metadata)?;
                    let modified = fingerprint.modified;
                    let session = Session::open_read_only_with_file(path.clone(), file)
                        .map_err(|_| ServiceError::InvalidSeed)?;
                    let fallback_task_name = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.split_once('-').map(|(_, task)| task))
                        .filter(|name| !name.is_empty())
                        .unwrap_or("worker");
                    let task_name = provenance
                        .display_task_name
                        .as_deref()
                        .unwrap_or(fallback_task_name);
                    let title = octet_serve_backend::sanitize_public_text(
                        &format!("parent > {task_name}"),
                        512,
                        false,
                    );
                    matched = Some(DelegatedSessionContext {
                        project_id: project_id.clone(),
                        parent_session_id,
                        config: config.clone(),
                        session,
                        meta: SessionMeta {
                            id: session_id.as_str().to_owned(),
                            path,
                            title,
                            name: None,
                            tags: vec!["subagent".into(), "read-only".into()],
                            pinned: false,
                            archived: false,
                            trashed_at_ms: None,
                            purge_after_ms: None,
                            forked_from_session_id: None,
                            forked_from_entry_id: None,
                            message_count: 0,
                            modified,
                            workspace: None,
                        },
                        fingerprint,
                    });
                }
            }
        }
        matched.ok_or(ServiceError::NotFound)
    }

    fn driver_for_delegated_session(
        &self,
        session_id: &SessionId,
    ) -> Result<OctetSessionDriver, ServiceError> {
        let context = self.delegated_session_context(session_id)?;
        let selection = advertised_selection_from_session(
            &context.session,
            &self.catalog,
            &context.config,
            &self.models,
        )
        .map_or_else(|| self.default_selection(), Ok)?;
        let generation = next_actor_generation();
        let refresh = DelegatedInspectionRefresh {
            path: context.meta.path.clone(),
            workspace: context.config.workspace.clone(),
            project_id: context.project_id.clone(),
            model: selection.clone(),
            generation,
            meta: context.meta.clone(),
        };
        let fingerprint = context.fingerprint;
        let parent_session_id = context.parent_session_id.clone();
        let mut seed = seed_from_session(
            &context.session,
            session_id.clone(),
            SessionSeedOptions {
                workspace: &context.config.workspace,
                project_id: Some(context.project_id),
                model: selection,
                authority: AuthorityProfile::ReadOnly,
                generation,
                meta: Some(context.meta),
                attachment_store: None,
                resource_store: None,
            },
        )?;
        seed.summary.live_state = SessionLiveState::Locked;
        seed.summary.owner = ActorOwnerState::ExternallyLocked;
        seed.snapshot.live_state = SessionLiveState::Locked;
        seed.snapshot.delegated_parent_session_id = Some(parent_session_id);
        Ok(OctetSessionDriver::inspect(seed, refresh, fingerprint))
    }

    fn driver_for_new(
        &self,
        request: CreateSessionRequest,
    ) -> Result<OctetSessionDriver, ServiceError> {
        if request.authority != self.authority_ceiling() {
            return Err(ServiceError::Unauthorized);
        }
        let context = self.project_context(request.project_id.as_ref())?;
        let model = match request.model {
            Some(model) => model,
            None => self.default_selection()?,
        };
        let summary = self
            .models
            .iter()
            .find(|summary| summary.provider == model.provider && summary.id == model.model)
            .ok_or(ServiceError::InvalidSeed)?;
        if !summary
            .reasoning
            .iter()
            .any(|choice| choice == &model.reasoning)
        {
            return Err(ServiceError::InvalidSeed);
        }
        let resolved = self
            .catalog
            .resolve(&ModelId(model.model.clone()))
            .map_err(|_| ServiceError::InvalidSeed)?;
        let reasoning =
            config::parse_reasoning(&model.reasoning).map_err(|_| ServiceError::InvalidSeed)?;
        let session_path = context.sessions.new_path(&crate::modes::timestamp());
        let session_id = session_id_from_path(&session_path)?;
        {
            let mut projects = self.projects.lock().map_err(|_| ServiceError::Internal)?;
            let registry_id = registry_project_id(&context.project_id)?;
            projects
                .bind_session(session_id.as_str(), &registry_id)
                .map_err(project_registry_service_error)?;
        }
        // A named launch is durable before bootstrap, without constructing an
        // App or contacting a provider. Keep the prepared session's lock until
        // the worker takes ownership; unnamed provisional sessions stay lazy.
        let (session_name, prepared_session) = if request.provisional {
            let mut pending_name = self
                .startup_session_name
                .lock()
                .map_err(|_| ServiceError::Internal)?;
            let prepared = if let Some(name) = pending_name.as_deref() {
                let session = crate::app::bootstrap::open_launch_session(
                    &mut None,
                    SessionSelection::CreateNew(session_path.clone()),
                )
                .map_err(|_| ServiceError::Internal)?;
                context
                    .sessions
                    .rename(session_id.as_str(), name)
                    .map_err(|_| ServiceError::Internal)?;
                Some(session)
            } else {
                None
            };
            (pending_name.take(), prepared)
        } else {
            (None, None)
        };
        let launch_session = if prepared_session.is_some() {
            SessionSelection::OpenExisting(session_path)
        } else {
            SessionSelection::CreateNew(session_path)
        };
        let generation = next_actor_generation();
        let selection = selection_for_model(&resolved, &reasoning, &context.config);
        let project_id = Some(context.project_id.clone());
        let mut seed = empty_seed(
            session_id,
            project_id.clone(),
            selection.clone(),
            request.authority,
            generation,
        );
        if let Some(name) = session_name.as_deref() {
            seed.summary.title = name.to_owned();
        }
        let plan = WorkerPlan {
            config: context.config,
            sessions: context.sessions,
            launch: LaunchSelection {
                model: resolved.spec.id.clone(),
                session: launch_session,
                reasoning,
                reasoning_mode: self.config.reasoning_mode,
            },
            prepared_session: Mutex::new(prepared_session),
            authority: request.authority,
            available_models: self.models.clone(),
            actor_generation: generation,
            session_id: seed.summary.id.clone(),
            project_id,
            attachments: self.attachments.clone(),
            documents: self.documents.clone(),
            projects: Arc::clone(&self.projects),
            trusted_files: Arc::clone(&self.trusted_files),
            search_index: Arc::clone(&self.search_index),
            resources: self.resources.clone(),
            goal_store: Some(self.goals.clone()),
            usage: Arc::clone(&self.usage),
            pull_requests: Arc::clone(&self.pull_requests),
            pull_request_projection: Arc::new(Mutex::new(None)),
            pull_request_discovery_enabled: Arc::new(AtomicBool::new(false)),
            pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            checkout_hooks: CheckoutTestHooks::default(),
        };
        Ok(OctetSessionDriver::spawn(seed, plan, 0))
    }

    fn driver_for_existing(
        &self,
        session_id: &SessionId,
    ) -> Result<OctetSessionDriver, ServiceError> {
        #[cfg(test)]
        self.open_count.fetch_add(1, Ordering::Relaxed);
        let context = self.project_context_for_session(session_id)?;
        let metadata = context
            .sessions
            .load_metadata(session_id.as_str())
            .map_err(|_| ServiceError::InvalidSeed)?;
        if metadata.trashed_at_ms.is_some() {
            return Err(ServiceError::InvalidBoundary);
        }
        let path = context
            .sessions
            .path_by_id(session_id.as_str())
            .map_err(|_| ServiceError::NotFound)?;
        let file = octet_agent::secure_fs::open_regular_file_for_append(&path)
            .map_err(|_| ServiceError::InvalidSeed)?;
        let session =
            Session::open_with_file(path.clone(), file).map_err(|_| ServiceError::InvalidSeed)?;
        let meta = context
            .sessions
            .meta_for_open_session(session_id.as_str(), &session)
            .map_err(|_| ServiceError::InvalidSeed)?;
        let selection =
            advertised_selection_from_session(&session, &self.catalog, &self.config, &self.models)
                .map_or_else(|| self.default_selection(), Ok)?;
        let generation = next_actor_generation();
        let authority = self.authority_ceiling();
        let mut seed = seed_from_session(
            &session,
            session_id.clone(),
            SessionSeedOptions {
                workspace: &context.config.workspace,
                project_id: Some(context.project_id.clone()),
                model: selection.clone(),
                authority,
                generation,
                meta: meta.clone(),
                attachment_store: self.attachments.as_ref(),
                resource_store: self.resources.as_ref(),
            },
        )?;
        seed.summary.pull_request = self.cached_pull_request(session_id);
        let pull_request_discovery_enabled = context.config.sandbox.process_execution_allowed()
            && session
                .entries()
                .iter()
                .any(|entry| matches!(&entry.value, EntryValue::Message(Message::User(_))));
        let reasoning =
            config::parse_reasoning(&selection.reasoning).map_err(|_| ServiceError::InvalidSeed)?;
        let known_entries = session.entries().len();
        let plan = WorkerPlan {
            config: context.config,
            sessions: context.sessions,
            launch: LaunchSelection {
                model: ModelId(selection.model),
                session: SessionSelection::OpenExisting(path),
                reasoning,
                reasoning_mode: self.config.reasoning_mode,
            },
            prepared_session: Mutex::new(Some(session)),
            authority,
            available_models: self.models.clone(),
            actor_generation: generation,
            session_id: session_id.clone(),
            project_id: Some(context.project_id),
            attachments: self.attachments.clone(),
            documents: self.documents.clone(),
            projects: Arc::clone(&self.projects),
            trusted_files: Arc::clone(&self.trusted_files),
            search_index: Arc::clone(&self.search_index),
            resources: self.resources.clone(),
            goal_store: Some(self.goals.clone()),
            usage: Arc::clone(&self.usage),
            pull_requests: Arc::clone(&self.pull_requests),
            pull_request_projection: Arc::new(Mutex::new(seed.summary.pull_request.clone())),
            pull_request_discovery_enabled: Arc::new(AtomicBool::new(
                pull_request_discovery_enabled,
            )),
            pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            checkout_hooks: self
                .checkout_hooks
                .lock()
                .map_err(|_| ServiceError::Internal)?
                .pop_front()
                .unwrap_or_default(),
        };
        Ok(OctetSessionDriver::spawn(seed, plan, known_entries))
    }
}

fn document_store_service_error(error: DocumentStoreError) -> ServiceError {
    match error {
        DocumentStoreError::InvalidAssociation
        | DocumentStoreError::InvalidDocumentId
        | DocumentStoreError::Ingest(_) => ServiceError::InvalidBoundary,
        DocumentStoreError::QuotaExceeded => ServiceError::Unavailable,
        DocumentStoreError::PromptLimitExceeded => ServiceError::PayloadTooLarge,
        DocumentStoreError::NotFound => ServiceError::NotFound,
        DocumentStoreError::Corrupt => ServiceError::CorruptResource,
        DocumentStoreError::Storage => ServiceError::Internal,
    }
}

fn trusted_file_service_error(error: TrustedFileError) -> ServiceError {
    match error {
        TrustedFileError::TrustRequired => ServiceError::Unauthorized,
        TrustedFileError::RootChanged
        | TrustedFileError::ChangedSinceIndex
        | TrustedFileError::Storage => ServiceError::Unavailable,
        TrustedFileError::NotFound => ServiceError::NotFound,
        TrustedFileError::InvalidEntryId
        | TrustedFileError::InvalidSearch
        | TrustedFileError::NotText => ServiceError::InvalidBoundary,
        TrustedFileError::ContextLimitExceeded => ServiceError::PayloadTooLarge,
    }
}

fn repository_context_service_error(error: RepositoryContextError) -> ServiceError {
    match error {
        RepositoryContextError::TrustRequired => ServiceError::Unauthorized,
        RepositoryContextError::RootChanged => ServiceError::Unavailable,
    }
}

fn transcript_search_service_error(error: SearchError) -> ServiceError {
    match error {
        SearchError::EmptyQuery
        | SearchError::TooLarge
        | SearchError::InvalidText
        | SearchError::InvalidLimit
        | SearchError::InvalidLimits => ServiceError::InvalidBoundary,
        SearchError::Capacity => ServiceError::Unavailable,
    }
}

fn search_document_for_item(
    session_id: &SessionId,
    session_title: &str,
    fallback_timestamp_ms: u64,
    item: &SessionItem,
) -> Option<SearchDocument> {
    if item.lifecycle != ItemLifecycle::Committed {
        return None;
    }
    let (kind, text, timestamp_ms) = match &item.payload {
        ItemPayload::UserMessage {
            text,
            attachments,
            documents,
            project_files,
            ..
        } => {
            let mut visible = Vec::new();
            if !text.trim().is_empty() {
                visible.push(text.clone());
            }
            visible.extend(
                attachments
                    .iter()
                    .map(|attachment| attachment.display_name.clone()),
            );
            visible.extend(
                documents
                    .iter()
                    .map(|document| document.display_name.clone()),
            );
            visible.extend(project_files.iter().map(|file| file.relative_path.clone()));
            (
                SearchDocumentKind::User,
                visible.join("\n"),
                fallback_timestamp_ms,
            )
        }
        ItemPayload::AssistantMessage { text } => (
            SearchDocumentKind::Assistant,
            text.clone(),
            fallback_timestamp_ms,
        ),
        ItemPayload::ToolCall(activity) => {
            let text = [
                Some(activity.title.as_str()),
                activity.summary.as_deref(),
                activity.target.as_deref(),
                activity.output_summary.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
            (
                if activity.status == ToolActivityStatus::Failed {
                    SearchDocumentKind::Error
                } else {
                    SearchDocumentKind::Tool
                },
                text,
                activity.completed_at_ms.unwrap_or(activity.started_at_ms),
            )
        }
        ItemPayload::ToolResult(result) => (
            if result.status == ToolActivityStatus::Failed {
                SearchDocumentKind::Error
            } else {
                SearchDocumentKind::Tool
            },
            [
                Some(result.summary.as_str()),
                result.output_summary.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
            result.completed_at_ms,
        ),
        ItemPayload::RunOutcome {
            outcome: octet_serve_backend::RunOutcome::Failed,
            message,
            ..
        } => (
            SearchDocumentKind::Error,
            message
                .clone()
                .unwrap_or_else(|| "The run failed.".to_owned()),
            fallback_timestamp_ms,
        ),
        ItemPayload::Source(source) if source.kind == SourceKind::Attachment => (
            SearchDocumentKind::Attachment,
            source.title.clone(),
            source.consulted_at_ms,
        ),
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(SearchDocument {
        session_id: session_id.as_str().to_owned(),
        item_id: item.id.as_str().to_owned(),
        kind,
        session_title: bounded_text(session_title, 512),
        text: bounded_text(&text, octet_serve_backend::MAX_SEARCH_DOCUMENT_TEXT_BYTES),
        timestamp_ms,
    })
}

fn search_documents_for_seed(seed: &SessionSeed) -> Vec<SearchDocument> {
    seed.snapshot
        .items
        .iter()
        .filter_map(|item| {
            search_document_for_item(
                &seed.snapshot.session_id,
                &seed.summary.title,
                seed.summary.modified_at_ms,
                item,
            )
        })
        .collect()
}

fn with_trusted_project_files<T>(
    projects: &Arc<Mutex<ProjectRegistry>>,
    trusted_files: &Arc<Mutex<HashMap<String, TrustedProjectFiles>>>,
    project_id: &ProjectId,
    operation: impl FnOnce(&TrustedProjectFiles, &ProjectRegistry) -> Result<T, TrustedFileError>,
) -> Result<T, ServiceError> {
    let registry_id = registry_project_id(project_id)?;
    let projects = projects.lock().map_err(|_| ServiceError::Internal)?;
    let service = {
        let mut services = trusted_files.lock().map_err(|_| ServiceError::Internal)?;
        match services.get(registry_id.as_str()) {
            Some(service) => service.clone(),
            None => {
                let service = TrustedProjectFiles::open(&projects, &registry_id)
                    .map_err(trusted_file_service_error)?;
                services.insert(registry_id.as_str().to_owned(), service.clone());
                service
            }
        }
    };
    operation(&service, &projects).map_err(trusted_file_service_error)
}

fn with_project_file_system<T>(
    projects: &Arc<Mutex<ProjectRegistry>>,
    project_id: &ProjectId,
    operation: impl FnOnce(&ProjectRegistry, &RegistryProjectId) -> Result<T, ProjectFileSystemError>,
) -> Result<T, ProjectFileSystemError> {
    let registry_id = RegistryProjectId::parse(project_id.as_str())
        .map_err(|_| ProjectFileSystemError::InvalidPath)?;
    let projects = projects
        .lock()
        .map_err(|_| ProjectFileSystemError::Storage)?;
    operation(&projects, &registry_id)
}

fn public_project_summary(
    registry: &ProjectRegistry,
    project: octet_serve_backend::RegistryProjectSummary,
) -> Result<ProjectSummary, ServiceError> {
    let session_count = registry
        .sessions_for_project(&project.id)
        .len()
        .min(u32::MAX as usize) as u32;
    Ok(ProjectSummary {
        id: ProjectId::new(project.id.as_str()).map_err(|_| ServiceError::Internal)?,
        name: octet_serve_backend::sanitize_public_text(&project.display_name, 256, false),
        trusted: project.state == RegistryProjectState::Trusted,
        archived: project.state == RegistryProjectState::Archived,
        available: project.available,
        is_default: project.is_default,
        session_count,
        live_session_count: 0,
    })
}

fn export_session_bytes(
    sessions: &SessionStore,
    session_id: &SessionId,
    serve_state_dir: &Path,
    max_bytes: usize,
) -> Result<bytes::Bytes, ServiceError> {
    sessions
        .path_by_id(session_id.as_str())
        .map_err(|_| ServiceError::NotFound)?;
    let serve_state_dir = serve_state_dir
        .canonicalize()
        .map_err(|_| ServiceError::Internal)?;
    let temporary = tempfile::Builder::new()
        .prefix(".session-export-")
        .tempdir_in(&serve_state_dir)
        .map_err(|_| ServiceError::Internal)?;
    let destination = temporary.path().join("session.json");
    let report = crate::session_commands::export_portable(
        sessions,
        session_id.as_str(),
        Some(destination),
        temporary.path(),
        false,
        false,
    )
    .map_err(|_| ServiceError::Internal)?;
    if report.included_secrets {
        return Err(ServiceError::Internal);
    }
    let bytes =
        match octet_agent::secure_fs::read_regular_file_bounded(&report.destination, max_bytes) {
            Ok(bytes) => bytes,
            Err(octet_agent::secure_fs::SecureFileError::TooLarge { .. }) => {
                return Err(ServiceError::PayloadTooLarge);
            }
            Err(_) => return Err(ServiceError::Internal),
        };
    Ok(bytes::Bytes::from(bytes))
}

fn export_delegated_session_bytes(
    path: &Path,
    fingerprint: DelegatedSessionFingerprint,
    session_id: &SessionId,
    workspace: &Path,
    serve_state_dir: &Path,
    max_bytes: usize,
) -> Result<bytes::Bytes, ServiceError> {
    let source = octet_agent::secure_fs::open_private_file_for_read(path)
        .map_err(|_| ServiceError::CorruptResource)?;
    fs2::FileExt::lock_shared(&source).map_err(|_| ServiceError::CorruptResource)?;
    let snapshot = (|| {
        let current = DelegatedSessionFingerprint::from_metadata(
            &source
                .metadata()
                .map_err(|_| ServiceError::CorruptResource)?,
        )?;
        if !fingerprint.same_file_as(&current) {
            return Err(ServiceError::CorruptResource);
        }
        if current.len > max_bytes as u64 {
            return Err(ServiceError::PayloadTooLarge);
        }
        let mut transcript = Vec::with_capacity(current.len as usize);
        let mut reader = (&source).take(max_bytes as u64 + 1);
        reader
            .read_to_end(&mut transcript)
            .map_err(|_| ServiceError::CorruptResource)?;
        let after = DelegatedSessionFingerprint::from_metadata(
            &source
                .metadata()
                .map_err(|_| ServiceError::CorruptResource)?,
        )?;
        if !current.same_file_as(&after)
            || current.len != after.len
            || transcript.len() as u64 != current.len
        {
            return Err(ServiceError::CorruptResource);
        }
        if transcript.len() > max_bytes {
            return Err(ServiceError::PayloadTooLarge);
        }
        Ok(transcript)
    })();
    let unlocked = fs2::FileExt::unlock(&source);
    if unlocked.is_err() {
        return Err(ServiceError::CorruptResource);
    }
    let transcript = snapshot?;

    let temporary = tempfile::Builder::new()
        .prefix(".delegated-session-export-")
        .tempdir_in(serve_state_dir)
        .map_err(|_| ServiceError::Internal)?;
    let sessions = SessionStore::new(temporary.path(), workspace);
    octet_agent::secure_fs::create_private_directory_all(sessions.dir())
        .map_err(|_| ServiceError::Internal)?;
    // This is an already-authorized, read-only snapshot, not a launchable
    // worker. A delegated handle makes SessionStore consult the durable roster,
    // which intentionally does not exist in this temporary export store.
    let copied_id = SessionId::new("delegated-export").map_err(|_| ServiceError::Internal)?;
    let copied_path = sessions.dir().join(format!("{}.jsonl", copied_id.as_str()));
    let mut copied = octet_agent::secure_fs::create_regular_file_for_append(&copied_path)
        .map_err(|_| ServiceError::Internal)?;
    copied
        .write_all(&transcript)
        .and_then(|()| copied.sync_all())
        .map_err(|_| ServiceError::Internal)?;
    drop(copied);
    let exported = export_session_bytes(&sessions, &copied_id, serve_state_dir, max_bytes)?;
    let mut package: serde_json::Value =
        serde_json::from_slice(&exported).map_err(|_| ServiceError::Internal)?;
    // Restore only the host-generated, path-free identity after the ordinary
    // portable exporter has validated and redacted the entire snapshot.
    package["source_id"] = serde_json::Value::String(session_id.as_str().to_owned());
    let bytes = serde_json::to_vec_pretty(&package).map_err(|_| ServiceError::Internal)?;
    if bytes.len() > max_bytes {
        return Err(ServiceError::PayloadTooLarge);
    }
    Ok(bytes::Bytes::from(bytes))
}

#[cfg(any())]
#[async_trait]
impl HostService for OctetHost {
    type Driver = OctetSessionDriver;

    fn descriptor(&self) -> HostDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> HostCapabilities {
        let attachment_policy = self.attachments.as_ref().map(AttachmentStore::policy);
        HostCapabilities {
            concurrent_sessions: true,
            opaque_resources: self.resources.is_some(),
            attachments: attachment_policy.is_some(),
            attachment_policy,
            documents: self.documents.is_some(),
            trusted_project_files: cfg!(unix),
            project_file_browser: cfg!(unix),
            project_file_write: cfg!(unix) && self.config.tool_available("write"),
            transcript_search: true,
            previews: false,
            connected_devices: false,
            session_metadata: true,
            session_branches: true,
            conversation_branching: true,
            session_trash: true,
            session_export: true,
            lan_clients: false,
            terminal: self.config.sandbox.process_execution_allowed(),
            child_agents: false,
        }
    }

    fn attachment_policy(&self) -> Option<AttachmentPolicy> {
        self.attachments.as_ref().map(AttachmentStore::policy)
    }

    async fn ingest_attachment(
        &self,
        display_name: &str,
        media_type: &str,
        bytes: bytes::Bytes,
    ) -> Result<AttachmentRef, AttachmentError> {
        self.attachments
            .as_ref()
            .ok_or(AttachmentError::Unavailable)?
            .ingest(display_name, media_type, bytes)
    }

    async fn attachment_content(&self, handle: &str) -> Result<StoredAttachment, AttachmentError> {
        self.attachments
            .as_ref()
            .ok_or(AttachmentError::Unavailable)?
            .content(handle)
    }

    fn document_ingest_supported(&self) -> bool {
        self.documents.is_some()
    }

    async fn ingest_document(
        &self,
        session_id: &SessionId,
        display_name: &str,
        media_type: &str,
        bytes: bytes::Bytes,
    ) -> Result<DocumentReference, ServiceError> {
        let context = self.project_context_for_session(session_id)?;
        let store = self.documents.clone().ok_or(ServiceError::Unavailable)?;
        store
            .ingest_async(
                context.project_id.as_str().to_owned(),
                session_id.as_str().to_owned(),
                display_name.to_owned(),
                media_type.to_owned(),
                bytes,
            )
            .await
            .map_err(document_store_service_error)
    }

    async fn list_documents(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<DocumentReference>, ServiceError> {
        let context = self.project_context_for_session(session_id)?;
        self.documents
            .as_ref()
            .ok_or(ServiceError::Unavailable)?
            .list_for_session(context.project_id.as_str(), session_id.as_str())
            .map_err(document_store_service_error)
    }

    fn trusted_project_files_supported(&self) -> bool {
        cfg!(unix)
    }

    async fn trusted_file_index(
        &self,
        project_id: &ProjectId,
    ) -> Result<TrustedFileIndexSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.summary(registry),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn list_trusted_files(
        &self,
        project_id: &ProjectId,
        limit: usize,
    ) -> Result<Vec<TrustedFileEntry>, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.list(registry, limit),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn search_trusted_files(
        &self,
        project_id: &ProjectId,
        query: &str,
        limit: usize,
    ) -> Result<TrustedFileSearchResult, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        let query = query.to_owned();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.search(registry, &query, limit),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn read_trusted_file(
        &self,
        project_id: &ProjectId,
        entry_id: &FileEntryId,
    ) -> Result<TrustedFileRead, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let trusted_files = Arc::clone(&self.trusted_files);
        let project_id = project_id.clone();
        let entry_id = entry_id.clone();
        tokio::task::spawn_blocking(move || {
            with_trusted_project_files(
                &projects,
                &trusted_files,
                &project_id,
                |service, registry| service.read(registry, &entry_id),
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn project_file_browser_supported(&self) -> bool {
        cfg!(unix)
    }

    fn project_file_write_supported(&self) -> bool {
        cfg!(unix) && self.config.tool_available("write")
    }

    async fn project_file_tree(
        &self,
        project_id: &ProjectId,
        path: &str,
    ) -> Result<ProjectFileTree, ProjectFileSystemError> {
        if !self.project_file_browser_supported() {
            return Err(ProjectFileSystemError::Unavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::tree(registry, registry_id, &path)
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    async fn read_project_file(
        &self,
        project_id: &ProjectId,
        path: &str,
        start_line: Option<u32>,
        end_line: Option<u32>,
    ) -> Result<ProjectFileRead, ProjectFileSystemError> {
        if !self.project_file_browser_supported() {
            return Err(ProjectFileSystemError::Unavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::read(registry, registry_id, &path, start_line, end_line)
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    async fn search_project_files(
        &self,
        project_id: &ProjectId,
        query: &str,
    ) -> Result<ProjectFileSearchResult, ProjectFileSystemError> {
        if !self.project_file_browser_supported() {
            return Err(ProjectFileSystemError::Unavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let query = query.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::search(registry, registry_id, &query)
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    async fn write_project_file(
        &self,
        project_id: &ProjectId,
        path: &str,
        content: &str,
        expected_sha256: &str,
        force: bool,
    ) -> Result<ProjectFileWrite, ProjectFileSystemError> {
        if !self.project_file_write_supported() {
            return Err(ProjectFileSystemError::WriteUnavailable);
        }
        let projects = Arc::clone(&self.projects);
        let project_id = project_id.clone();
        let path = path.to_owned();
        let content = content.to_owned();
        let expected_sha256 = expected_sha256.to_owned();
        tokio::task::spawn_blocking(move || {
            with_project_file_system(&projects, &project_id, |registry, registry_id| {
                ProjectFileSystem::write(
                    registry,
                    registry_id,
                    &path,
                    &content,
                    &expected_sha256,
                    force,
                )
            })
        })
        .await
        .map_err(|_| ProjectFileSystemError::Storage)?
    }

    fn transcript_search_supported(&self) -> bool {
        true
    }

    async fn search_transcripts(
        &self,
        request: &TranscriptSearchRequest,
    ) -> Result<TranscriptSearchResult, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let search_index = Arc::clone(&self.search_index);
        let search_index_initialized = Arc::clone(&self.search_index_initialized);
        let base_config = self.config.clone();
        let catalog = self.catalog.clone();
        let fallback = self.default_selection()?;
        let attachments = self.attachments.clone();
        let resources = self.resources.clone();
        let request = request.clone();
        let authority = self.authority_ceiling();
        tokio::task::spawn_blocking(move || {
            // Hold the index lock while the one-time historical rebuild runs so
            // a concurrent run completion or deletion cannot be overwritten by
            // the snapshot being installed. Subsequent searches never acquire
            // the project registry lock or reopen session transcripts.
            let mut search_index = search_index.lock().map_err(|_| ServiceError::Internal)?;
            if !search_index_initialized.load(Ordering::Acquire) {
                let projects = projects.lock().map_err(|_| ServiceError::Internal)?;
                let mut rebuilt = TranscriptSearchIndex::new();
                for project in projects.list() {
                    let Ok(root) = projects.resolve_trusted_root(&project.id) else {
                        continue;
                    };
                    let sessions = SessionStore::new(&base_config.session_dir, root.as_path());
                    let bound = projects.sessions_for_project(&project.id);
                    let public_project_id =
                        ProjectId::new(project.id.as_str()).map_err(|_| ServiceError::Internal)?;
                    let mut project_config = base_config.clone();
                    project_config.workspace = root.as_path().to_owned();
                    project_config.invocation_cwd = root.as_path().to_owned();
                    project_config.workspace_trusted = true;
                    for session_id_text in
                        sessions.session_ids_newest_first(bound.iter().map(String::as_str))
                    {
                        let Ok(session_id) = SessionId::new(session_id_text.clone()) else {
                            continue;
                        };
                        let Ok(path) = sessions.path_by_id(&session_id_text) else {
                            continue;
                        };
                        let Ok(session) = Session::open_read_only(&path) else {
                            continue;
                        };
                        let Ok(Some(meta)) =
                            sessions.meta_for_open_session(&session_id_text, &session)
                        else {
                            continue;
                        };
                        let selection = selection_from_session(&session, &catalog, &project_config)
                            .unwrap_or_else(|_| fallback.clone());
                        let seed = seed_from_session(
                            &session,
                            session_id.clone(),
                            SessionSeedOptions {
                                workspace: &project_config.workspace,
                                project_id: Some(public_project_id.clone()),
                                model: selection,
                                authority,
                                generation: 1,
                                meta: Some(meta),
                                attachment_store: attachments.as_ref(),
                                resource_store: resources.as_ref(),
                            },
                        )?;
                        rebuilt
                            .replace_session(session_id.as_str(), search_documents_for_seed(&seed))
                            .map_err(transcript_search_service_error)?;
                    }
                }
                *search_index = rebuilt;
                search_index_initialized.store(true, Ordering::Release);
            }
            search_index
                .search_request(&request)
                .map_err(transcript_search_service_error)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn repository_context_supported(&self) -> bool {
        cfg!(unix)
    }

    async fn repository_context(
        &self,
        project_id: &ProjectId,
    ) -> Result<RepositoryContextSnapshot, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        tokio::task::spawn_blocking(move || {
            let projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            refresh_repository_context(&projects, &project_id)
                .map_err(repository_context_service_error)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn resource_content(
        &self,
        session_id: &SessionId,
        handle: &str,
    ) -> Result<StoredResource, ServiceError> {
        self.resources
            .as_ref()
            .ok_or(ServiceError::Unavailable)?
            .content(session_id, handle)
            .map_err(resource_store_service_error)
    }

    async fn session_export(&self, session_id: &SessionId) -> Result<bytes::Bytes, ServiceError> {
        if session_id.as_str().starts_with(DELEGATED_SESSION_PREFIX) {
            let context = self.delegated_session_context(session_id)?;
            let path = context.meta.path;
            let fingerprint = context.fingerprint;
            let workspace = context.config.workspace;
            let session_id = session_id.clone();
            let serve_state_dir = self.serve_state_dir.clone();
            return tokio::task::spawn_blocking(move || {
                export_delegated_session_bytes(
                    &path,
                    fingerprint,
                    &session_id,
                    &workspace,
                    &serve_state_dir,
                    MAX_GRAPHICAL_SESSION_EXPORT_BYTES,
                )
            })
            .await
            .map_err(|_| ServiceError::Internal)?;
        }
        let sessions = self.project_context_for_session(session_id)?.sessions;
        let session_id = session_id.clone();
        let serve_state_dir = self.serve_state_dir.clone();
        tokio::task::spawn_blocking(move || {
            export_session_bytes(
                &sessions,
                &session_id,
                &serve_state_dir,
                MAX_GRAPHICAL_SESSION_EXPORT_BYTES,
            )
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn usage_stats(&self, period: UsagePeriod) -> Result<UsageStats, ServiceError> {
        Ok(self
            .usage
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .stats(period))
    }

    async fn usage_lifetime(&self) -> Result<LifetimeUsage, ServiceError> {
        Ok(self
            .usage
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .lifetime())
    }

    async fn usage_activity(&self) -> Result<UsageActivity, ServiceError> {
        Ok(self
            .usage
            .lock()
            .map_err(|_| ServiceError::Internal)?
            .activity())
    }

    fn authority_ceiling(&self) -> AuthorityProfile {
        authority_ceiling_from_sandbox(&self.config.sandbox)
    }

    fn authority_profiles(&self) -> Vec<AuthorityProfile> {
        authority_profiles_from_sandbox(&self.config.sandbox)
    }

    fn model_catalog(&self) -> Vec<ModelSummary> {
        self.models.clone()
    }

    fn theme_catalog(&self) -> Vec<ThemeOption> {
        self.themes.clone()
    }

    fn selected_theme_id(&self) -> ThemeId {
        self.selected_theme_id.clone()
    }

    async fn list_projects(&self) -> Result<Vec<ProjectSummary>, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            reconcile_session_bindings(&config, &mut projects, None)
                .map_err(project_registry_service_error)?;
            projects
                .list()
                .into_iter()
                .map(|project| public_project_summary(&projects, project))
                .collect()
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn project_lifecycle_mutations_supported(&self) -> bool {
        cfg!(unix)
    }

    fn project_import_supported(&self) -> bool {
        false
    }

    async fn import_project(
        &self,
        _candidate_id: &str,
        display_name: Option<&str>,
    ) -> Result<ProjectSummary, ServiceError> {
        let _ = display_name;
        // The browser transport has no native folder picker. Real roots are
        // imported from the trusted launch/CLI workspace; this command remains
        // unavailable until a host UI can mint one-use opaque candidates.
        Err(ServiceError::Unavailable)
    }

    async fn rename_project(
        &self,
        project_id: &ProjectId,
        display_name: &str,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        let display_name = display_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = projects
                .update_display_name(&project_id, &display_name)
                .map_err(project_registry_service_error)?;
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn set_default_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = projects
                .set_default(&project_id)
                .map_err(project_registry_service_error)?;
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn clear_default_project(&self) -> Result<(), ServiceError> {
        let projects = Arc::clone(&self.projects);
        tokio::task::spawn_blocking(move || {
            projects
                .lock()
                .map_err(|_| ServiceError::Internal)?
                .clear_default()
                .map_err(project_registry_service_error)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn set_project_trust(
        &self,
        project_id: &ProjectId,
        trusted: bool,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        let launch_project_id = self.launch_project_id.clone();
        let launch_workspace = self.config.workspace.clone();
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = if trusted {
                // A replaced checkout at the exact launch path can be restored
                // only by an explicit trust action. Never rebind another
                // project or accept a browser-supplied filesystem path.
                if project_id.as_str() == launch_project_id.as_str() {
                    projects
                        .rebind_root(&project_id, &launch_workspace)
                        .map_err(project_registry_service_error)?;
                }
                projects.grant_trust(&project_id)
            } else {
                projects.revoke_trust(&project_id)
            }
            .map_err(project_registry_service_error)?;
            if trusted {
                reconcile_session_bindings(&config, &mut projects, None)
                    .map_err(project_registry_service_error)?;
            }
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn archive_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectSummary, ServiceError> {
        let projects = Arc::clone(&self.projects);
        let project_id = registry_project_id(project_id)?;
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            let project = projects
                .archive(&project_id)
                .map_err(project_registry_service_error)?;
            public_project_summary(&projects, project)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    fn session_trash_supported(&self) -> bool {
        true
    }

    async fn set_session_lifecycle(
        &self,
        session_id: &SessionId,
        lifecycle: SessionCatalogState,
        changed_at_ms: u64,
    ) -> Result<SessionSummary, ServiceError> {
        let context = self.storage_context_for_session(session_id)?;
        let storage_lifecycle = match lifecycle {
            SessionCatalogState::Active => SessionStorageLifecycle::Active,
            SessionCatalogState::Archived => SessionStorageLifecycle::Archived,
            SessionCatalogState::Trash => SessionStorageLifecycle::Trash,
        };
        context
            .sessions
            .set_lifecycle(session_id.as_str(), storage_lifecycle, changed_at_ms)
            .map_err(|_| ServiceError::Internal)?;
        self.stored_session_summary(session_id)
    }

    async fn delete_session_permanently(
        &self,
        session_id: &SessionId,
        confirmation: &PermanentDeleteConfirmation,
    ) -> Result<(), ServiceError> {
        if &confirmation.session_id != session_id
            || confirmation.phrase != format!("permanently delete {}", session_id.as_str())
        {
            return Err(ServiceError::InvalidBoundary);
        }
        // Distinct idempotency keys may execute concurrently. Serialize the
        // destructive state machine so one request cannot overwrite or remove
        // another request's recovery journal.
        let _deletion_guard = self.session_deletion_lock.lock().await;
        let context = self.storage_context_for_session(session_id)?;
        if self.attachments.is_none() || self.documents.is_none() || self.resources.is_none() {
            return Err(ServiceError::Unavailable);
        }
        // The supervisor has quiesced the session owner. Preserve every last
        // ledger row and uncertainty marker before deleting their only source.
        let inspection = context
            .sessions
            .inspect_by_id(session_id.as_str())
            .map_err(|_| ServiceError::Unavailable)?;
        {
            let mut usage = self.usage.lock().map_err(|_| ServiceError::Internal)?;
            usage
                .ensure_available()
                .map_err(|_| ServiceError::Unavailable)?;
            if !inspection.usage_uncertainty_records.is_empty() {
                usage
                    .record_uncertainty(session_id.as_str())
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            usage
                .record_all(
                    project_catalog_usage(session_id.as_str(), &inspection.usage_records)
                        .map_err(|_| ServiceError::Unavailable)?,
                )
                .map_err(|_| ServiceError::Unavailable)?;
        }
        let mut deletion = PendingSessionDeletion::new(
            session_id,
            &context.project_id,
            confirmation.trashed_at_ms,
        );
        write_pending_session_deletion(&self.serve_state_dir, &deletion)
            .map_err(|_| ServiceError::Internal)?;

        let delete_result = context
            .sessions
            .delete_permanently(session_id.as_str(), confirmation.trashed_at_ms);
        if delete_result.is_err() {
            match context.sessions.session_file_exists(session_id.as_str()) {
                Ok(true) => {
                    context
                        .sessions
                        .rollback_permanent_delete(session_id.as_str())
                        .map_err(|_| ServiceError::Internal)?;
                    remove_pending_session_deletion(&self.serve_state_dir, session_id.as_str())
                        .map_err(|_| ServiceError::Internal)?;
                    return Err(ServiceError::InvalidBoundary);
                }
                Ok(false) => {}
                Err(_) => {
                    // Preserve the durable intent. Startup recovery must not
                    // infer commitment from a transcript it could not inspect.
                    return Err(ServiceError::Internal);
                }
            }
        }

        // The JSONL disappearance is the irreversible commit boundary. Every
        // later step is idempotent and journaled so interruption cannot turn a
        // completed user-visible delete into permanently leaked sidecars.
        deletion.committed = true;
        let marker_committed =
            write_pending_session_deletion(&self.serve_state_dir, &deletion).is_ok();
        let primary_clean = context
            .sessions
            .finish_permanent_delete(session_id.as_str())
            .is_ok();
        let unbound = self
            .projects
            .lock()
            .is_ok_and(|mut projects| projects.unbind_session(session_id.as_str()).is_ok());
        let sidecars_clean = self.cleanup_session_sidecars(&context.project_id, session_id);
        if marker_committed && primary_clean && unbound && sidecars_clean {
            let _ = remove_pending_session_deletion(&self.serve_state_dir, session_id.as_str());
        } else {
            crate::output::stderr_line(format!(
                "warning: permanent deletion cleanup for session {} will retry on startup",
                session_id.as_str()
            ));
        }
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, ServiceError> {
        let fallback = self.default_selection()?;
        let projects = Arc::clone(&self.projects);
        let pull_requests = Arc::clone(&self.pull_requests);
        let catalog = self.catalog.clone();
        let models = self.models.clone();
        let base_config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
            reconcile_session_bindings(&base_config, &mut projects, None)
                .map_err(project_registry_service_error)?;
            let mut summaries = Vec::new();
            for project in projects.list() {
                if summaries.len() >= 2_000 || project.state == RegistryProjectState::Archived {
                    continue;
                }
                let Ok(root) = projects.resolve_root(&project.id) else {
                    continue;
                };
                let sessions = SessionStore::new(&base_config.session_dir, root.as_path());
                let bound = projects.sessions_for_project(&project.id);
                let public_project_id =
                    ProjectId::new(project.id.as_str()).map_err(|_| ServiceError::Internal)?;
                let mut project_config = base_config.clone();
                project_config.workspace = root.as_path().to_owned();
                project_config.invocation_cwd = root.as_path().to_owned();
                project_config.workspace_trusted = project.state == RegistryProjectState::Trusted;
                let session_ids = sessions
                    .session_ids_newest_first(bound.iter().map(String::as_str))
                    .into_iter()
                    .take(2_000)
                    .collect::<Vec<_>>();
                let catalog_entries = sessions
                    .catalog_by_ids(session_ids.iter().map(String::as_str))
                    .unwrap_or_default();
                for (_session_id, catalog_entry) in catalog_entries {
                    if summaries.len() >= 2_000 {
                        break;
                    }
                    let Some(meta) = catalog_entry.meta.as_ref() else {
                        continue;
                    };
                    let selection = advertised_selection_from_catalog_entry(
                        &catalog_entry,
                        &catalog,
                        &project_config,
                        &models,
                    )
                    .unwrap_or_else(|| fallback.clone());
                    if let Ok(summary) =
                        summary_from_meta(meta, Some(public_project_id.clone()), selection)
                    {
                        summaries.push(summary);
                    }
                }
            }
            drop(projects);
            // Snapshot evidence only after the blocking inventory scan, without
            // holding its mutex across transcript I/O or waiting on the async
            // runtime when a persistence transaction is finishing.
            let pull_requests = pull_requests
                .lock()
                .map_err(|_| ServiceError::Internal)?
                .summaries();
            for summary in &mut summaries {
                summary.pull_request = pull_requests.get(summary.id.as_str()).cloned();
            }
            Ok(summaries)
        })
        .await
        .map_err(|_| ServiceError::Internal)?
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<Self::Driver, ServiceError> {
        self.driver_for_new(request)
    }

    async fn open_session(&self, session_id: &SessionId) -> Result<Self::Driver, ServiceError> {
        if session_id.as_str().starts_with(DELEGATED_SESSION_PREFIX) {
            self.driver_for_delegated_session(session_id)
        } else {
            self.driver_for_existing(session_id)
        }
    }
}

#[derive(Clone)]
struct DelegatedInspectionRefresh {
    path: PathBuf,
    workspace: PathBuf,
    project_id: ProjectId,
    model: ModelSelection,
    generation: u64,
    meta: SessionMeta,
}

impl DelegatedInspectionRefresh {
    fn load(
        &self,
        previous: DelegatedSessionFingerprint,
    ) -> Result<Option<(DelegatedSessionFingerprint, SessionSeed)>, ServiceError> {
        let file = octet_agent::secure_fs::open_private_file_for_read(&self.path)
            .map_err(|_| ServiceError::CorruptResource)?;
        let metadata = file.metadata().map_err(|_| ServiceError::CorruptResource)?;
        let fingerprint = DelegatedSessionFingerprint::from_metadata(&metadata)?;
        if !previous.same_file_as(&fingerprint) {
            return Err(ServiceError::CorruptResource);
        }
        if previous == fingerprint {
            return Ok(None);
        }
        let session = Session::open_read_only_with_file(self.path.clone(), file)
            .map_err(|_| ServiceError::CorruptResource)?;
        let mut meta = self.meta.clone();
        meta.modified = fingerprint.modified;
        let session_id = SessionId::new(meta.id.clone()).map_err(|_| ServiceError::InvalidSeed)?;
        let mut seed = seed_from_session(
            &session,
            session_id,
            SessionSeedOptions {
                workspace: &self.workspace,
                project_id: Some(self.project_id.clone()),
                model: self.model.clone(),
                authority: AuthorityProfile::ReadOnly,
                generation: self.generation,
                meta: Some(meta),
                attachment_store: None,
                resource_store: None,
            },
        )?;
        seed.summary.live_state = SessionLiveState::Locked;
        seed.summary.owner = ActorOwnerState::ExternallyLocked;
        seed.snapshot.live_state = SessionLiveState::Locked;
        Ok(Some((fingerprint, seed)))
    }
}

const MAX_DELEGATED_INSPECTION_EVENTS: usize = 256;

fn delegated_inspection_events(
    previous: &SessionSeed,
    next: &SessionSeed,
) -> Option<VecDeque<TimestampedEvent>> {
    let timestamp = now_ms();
    let mut payloads = Vec::new();
    let known_branches = previous
        .snapshot
        .branches
        .entries
        .iter()
        .map(|entry| entry.entry_id.clone())
        .collect::<BTreeSet<_>>();
    let appended = next
        .snapshot
        .branches
        .entries
        .iter()
        .filter(|entry| !known_branches.contains(&entry.entry_id))
        .cloned()
        .collect::<Vec<_>>();
    for entries in appended.chunks(MAX_BRANCH_DELTA_ENTRIES) {
        payloads.push(EventPayload::SessionBranchEntriesAppended {
            entries: entries.to_vec(),
        });
    }
    if previous.snapshot.durable_head != next.snapshot.durable_head {
        payloads.push(EventPayload::SessionDurableHeadChanged {
            durable_entry_id: next.snapshot.durable_head.clone(),
        });
    }

    let previous_items = previous
        .snapshot
        .items
        .iter()
        .map(|item| (&item.id, item))
        .collect::<BTreeMap<_, _>>();
    for item in &next.snapshot.items {
        if previous_items
            .get(&item.id)
            .is_none_or(|previous| *previous != item)
        {
            payloads.push(EventPayload::ItemCommitted { item: item.clone() });
        }
    }
    let previous_sources = previous
        .snapshot
        .sources
        .iter()
        .map(|source| (&source.id, source))
        .collect::<BTreeMap<_, _>>();
    for source in &next.snapshot.sources {
        if previous_sources
            .get(&source.id)
            .is_none_or(|previous| *previous != source)
        {
            payloads.push(EventPayload::SourceUpserted {
                source: source.clone(),
            });
        }
    }
    let previous_artifacts = previous
        .snapshot
        .artifacts
        .iter()
        .map(|artifact| (&artifact.id, artifact))
        .collect::<BTreeMap<_, _>>();
    for artifact in &next.snapshot.artifacts {
        if previous_artifacts
            .get(&artifact.id)
            .is_none_or(|previous| *previous != artifact)
        {
            payloads.push(EventPayload::ArtifactUpserted {
                artifact: artifact.clone(),
            });
        }
    }
    if payloads.len() > MAX_DELEGATED_INSPECTION_EVENTS {
        return None;
    }
    Some(
        payloads
            .into_iter()
            .map(|payload| TimestampedEvent::new(timestamp, payload))
            .collect(),
    )
}

struct DelegatedInspection {
    refresh: DelegatedInspectionRefresh,
    fingerprint: DelegatedSessionFingerprint,
    projection: SessionSeed,
}

struct OctetSessionDriver {
    seed: SessionSeed,
    commands: Option<mpsc::Sender<WorkerMessage>>,
    events: mpsc::Receiver<TimestampedEvent>,
    buffered_events: VecDeque<TimestampedEvent>,
    worker: Option<tokio::task::JoinHandle<()>>,
    inspect_only: bool,
    inspection: Option<DelegatedInspection>,
}

impl OctetSessionDriver {
    fn spawn(seed: SessionSeed, plan: WorkerPlan, known_entries: usize) -> Self {
        let (commands, command_receiver) = mpsc::channel(DRIVER_MAILBOX_CAPACITY);
        let (event_sender, events) = mpsc::channel(DRIVER_EVENT_CAPACITY);
        let worker = tokio::spawn(run_worker(
            plan,
            command_receiver,
            event_sender,
            known_entries,
        ));
        Self {
            seed,
            commands: Some(commands),
            events,
            buffered_events: VecDeque::new(),
            worker: Some(worker),
            inspect_only: false,
            inspection: None,
        }
    }

    fn inspect(
        seed: SessionSeed,
        refresh: DelegatedInspectionRefresh,
        fingerprint: DelegatedSessionFingerprint,
    ) -> Self {
        let (sender, events) = mpsc::channel(1);
        drop(sender);
        let projection = seed.clone();
        Self {
            seed,
            commands: None,
            events,
            buffered_events: VecDeque::new(),
            worker: None,
            inspect_only: true,
            inspection: Some(DelegatedInspection {
                refresh,
                fingerprint,
                projection,
            }),
        }
    }
}

#[async_trait]
impl SessionDriver for OctetSessionDriver {
    fn seed(&self) -> SessionSeed {
        self.seed.clone()
    }

    async fn dispatch(
        &mut self,
        command: SessionCommand,
    ) -> Result<DriverCommandOutcome, ServiceError> {
        if self.inspect_only {
            return Err(ServiceError::Unauthorized);
        }
        if let SessionCommand::SetAuthority { authority } = command {
            // Reject unsupported narrowing before the worker mailbox, even
            // during a run. The seed describes immutable host authority; a
            // repeated selection needs no rebuild, settings event, or effect.
            return if authority == self.seed.snapshot.authority {
                Ok(DriverCommandOutcome::default())
            } else {
                Err(ServiceError::Unauthorized)
            };
        }
        let (response, receiver) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(ServiceError::OwnerLost)?
            .send(WorkerMessage::Command(WorkerCommand { command, response }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        receiver.await.map_err(|_| ServiceError::Unavailable)?
    }

    async fn command_discovery(&mut self) -> Result<CommandDiscovery, ServiceError> {
        if self.inspect_only {
            return Ok(CommandDiscovery {
                protocol: PROTOCOL_VERSION,
                commands: Vec::new(),
                skills: Vec::new(),
            });
        }
        let (response, mut receiver) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(ServiceError::OwnerLost)?
            .send(WorkerMessage::CommandDiscovery { response })
            .await
            .map_err(|_| ServiceError::Unavailable)?;

        // The actor serializes this call with `next_event`. Keep receiving into
        // a private FIFO while the worker processes discovery so a busy stream
        // cannot fill the worker's event channel and block its command select.
        // If the FIFO reaches its bound, stop draining briefly so the worker can
        // answer; otherwise fail the discovery request and let the actor resume
        // normal event reduction without dropping stream events.
        let mut events_open = true;
        loop {
            if events_open && self.buffered_events.len() >= MAX_BUFFERED_DISCOVERY_EVENTS {
                let result = tokio::time::timeout(DISCOVERY_BACKPRESSURE_TIMEOUT, &mut receiver)
                    .await
                    .map_err(|_| ServiceError::Unavailable)?
                    .map_err(|_| ServiceError::Unavailable)?;
                return result;
            }
            tokio::select! {
                result = &mut receiver => return result.map_err(|_| ServiceError::Unavailable)?,
                event = self.events.recv(), if events_open => match event {
                    Some(event) => self.buffered_events.push_back(event),
                    None => events_open = false,
                },
            }
        }
    }

    async fn next_event(&mut self) -> Option<TimestampedEvent> {
        if self.inspect_only {
            loop {
                if let Some(event) = self.buffered_events.pop_front() {
                    return Some(event);
                }
                let inspection = self.inspection.as_ref()?;
                let refresh = inspection.refresh.clone();
                let fingerprint = inspection.fingerprint;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let loaded = tokio::task::spawn_blocking(move || refresh.load(fingerprint)).await;
                match loaded {
                    Ok(Ok(None)) => {}
                    Ok(Ok(Some((next_fingerprint, next)))) => {
                        let inspection = self.inspection.as_mut()?;
                        let Some(events) =
                            delegated_inspection_events(&inspection.projection, &next)
                        else {
                            self.inspection = None;
                            return Some(TimestampedEvent::new(
                                now_ms(),
                                EventPayload::SessionStateChanged {
                                    state: SessionLiveState::Offline,
                                    active_run_id: None,
                                },
                            ));
                        };
                        inspection.fingerprint = next_fingerprint;
                        inspection.projection = next;
                        self.seed = inspection.projection.clone();
                        self.buffered_events = events;
                    }
                    Ok(Err(_)) | Err(_) => {
                        self.inspection = None;
                        return Some(TimestampedEvent::new(
                            now_ms(),
                            EventPayload::SessionStateChanged {
                                state: SessionLiveState::Offline,
                                active_run_id: None,
                            },
                        ));
                    }
                }
            }
        }
        match self.buffered_events.pop_front() {
            Some(event) => Some(event),
            None => self.events.recv().await,
        }
    }

    async fn shutdown(&mut self) {
        self.commands.take();
        self.events.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.await;
        }
    }
}

#[derive(Clone)]
struct PullRequestRefreshPlan {
    workspace: PathBuf,
    session_id: SessionId,
    pull_requests: Arc<Mutex<PullRequestStore>>,
    projection: Arc<Mutex<Option<PullRequestSummary>>>,
    discovery_enabled: Arc<AtomicBool>,
    refresh_requested: Arc<tokio::sync::Notify>,
    process_execution_allowed: bool,
}

impl From<&WorkerPlan> for PullRequestRefreshPlan {
    fn from(plan: &WorkerPlan) -> Self {
        Self {
            workspace: plan.config.workspace.clone(),
            session_id: plan.session_id.clone(),
            pull_requests: Arc::clone(&plan.pull_requests),
            projection: Arc::clone(&plan.pull_request_projection),
            discovery_enabled: Arc::clone(&plan.pull_request_discovery_enabled),
            refresh_requested: Arc::clone(&plan.pull_request_refresh_requested),
            process_execution_allowed: plan.config.sandbox.process_execution_allowed(),
        }
    }
}

fn project_github_pull_request(bytes: &[u8]) -> PullRequestObservation {
    let Ok(pull_request) = serde_json::from_slice::<GitHubPullRequest>(bytes) else {
        return PullRequestObservation::Unavailable;
    };
    if pull_request.number == 0
        || !pull_request_url_is_valid(&pull_request.url, pull_request.number)
    {
        return PullRequestObservation::Unavailable;
    }
    let state = match pull_request.state.as_str() {
        "OPEN" if pull_request.is_draft => PullRequestState::InProgress,
        "OPEN" => PullRequestState::Ready,
        "MERGED" => PullRequestState::Merged,
        "CLOSED" => {
            return PullRequestObservation::Closed {
                number: pull_request.number,
                url: pull_request.url,
            };
        }
        _ => return PullRequestObservation::Unavailable,
    };
    PullRequestObservation::Trackable {
        number: pull_request.number,
        url: pull_request.url,
        state,
    }
}

async fn query_github_pull_request(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
) -> PullRequestObservation {
    query_github_pull_request_with_timeout(workspace, selector, executable, GITHUB_CLI_TIMEOUT)
        .await
}

async fn query_hosted_github_pull_request(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
) -> PullRequestObservation {
    query_github_pull_request_with_timeout_and_queued_permit(
        workspace,
        selector,
        executable,
        GITHUB_CLI_TIMEOUT,
        &GITHUB_QUERY_PERMITS,
    )
    .await
}

async fn query_github_pull_request_with_timeout(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
) -> PullRequestObservation {
    query_github_pull_request_with_timeout_and_permits(
        workspace,
        selector,
        executable,
        timeout,
        &GITHUB_QUERY_PERMITS,
    )
    .await
}

async fn query_github_pull_request_with_timeout_and_permits(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
    permits: &tokio::sync::Semaphore,
) -> PullRequestObservation {
    let Ok(_permit) = permits.try_acquire() else {
        return PullRequestObservation::Unavailable;
    };
    execute_github_pull_request_query(workspace, selector, executable, timeout).await
}

async fn query_github_pull_request_with_timeout_and_queued_permit(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
    permits: &tokio::sync::Semaphore,
) -> PullRequestObservation {
    let Ok(_permit) = permits.acquire().await else {
        return PullRequestObservation::Unavailable;
    };
    execute_github_pull_request_query(workspace, selector, executable, timeout).await
}

async fn terminate_github_process(child: &mut tokio::process::Child, process_tree: &ProcessTree) {
    process_tree.signal(TerminationSignal::Graceful);
    let graceful_deadline = Instant::now() + GITHUB_CLI_GRACE_PERIOD;
    while Instant::now() < graceful_deadline {
        let child_settled = child.try_wait().ok().flatten().is_some();
        if child_settled && !process_tree.is_alive() {
            process_tree.disarm();
            return;
        }
        tokio::time::sleep(GITHUB_CLI_CLEANUP_POLL_INTERVAL).await;
    }

    process_tree.signal(TerminationSignal::Force);
    // Keep the direct-child fallback for platforms without process groups.
    let _ = child.start_kill();
    let force_deadline = Instant::now() + GITHUB_CLI_FORCE_PERIOD;
    while Instant::now() < force_deadline {
        let child_settled = child.try_wait().ok().flatten().is_some();
        if child_settled && !process_tree.is_alive() {
            process_tree.disarm();
            return;
        }
        tokio::time::sleep(GITHUB_CLI_CLEANUP_POLL_INTERVAL).await;
    }
    // Keep the guard armed through return so Drop makes one final group-wide
    // kill attempt without allowing cleanup to wait on inherited descriptors.
}

async fn execute_github_pull_request_query(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
) -> PullRequestObservation {
    let environment = github_cli_environment(workspace);
    execute_github_pull_request_query_with_environment(
        workspace,
        selector,
        executable,
        timeout,
        &environment,
    )
    .await
}

async fn execute_github_pull_request_query_with_environment(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
    environment: &BTreeMap<OsString, OsString>,
) -> PullRequestObservation {
    let mut command = tokio::process::Command::new(executable);
    command
        .env_clear()
        .envs(environment)
        .args(["pr", "view"])
        .current_dir(workspace)
        .env_remove("GH_REPO")
        .env_remove("GH_FORCE_TTY")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(selector) = selector {
        command.arg(selector);
    }
    command.args(["--json", "number,url,state,isDraft"]);
    isolate_process_group(command.as_std_mut());
    let Ok(mut child) = command.spawn() else {
        return PullRequestObservation::Unavailable;
    };
    let process_tree = ProcessTree::from_process_id(child.id());
    let Some(stdout) = child.stdout.take() else {
        terminate_github_process(&mut child, &process_tree).await;
        return PullRequestObservation::Unavailable;
    };
    let result = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        let mut bounded = stdout.take(MAX_GITHUB_CLI_OUTPUT_BYTES + 1);
        bounded.read_to_end(&mut bytes).await?;
        drop(bounded);
        if bytes.len() as u64 > MAX_GITHUB_CLI_OUTPUT_BYTES {
            return Ok::<_, std::io::Error>(None);
        }
        let status = child.wait().await?;
        Ok(Some((status, bytes)))
    })
    .await;
    let Ok(Ok(Some((status, bytes)))) = result else {
        terminate_github_process(&mut child, &process_tree).await;
        return PullRequestObservation::Unavailable;
    };
    // A successful read and wait settle the direct child, but a helper may
    // still have escaped without retaining stdout. Do not let it escape.
    process_tree.signal(TerminationSignal::Force);
    process_tree.disarm();
    if !status.success() || bytes.len() as u64 > MAX_GITHUB_CLI_OUTPUT_BYTES {
        return PullRequestObservation::Unavailable;
    }
    project_github_pull_request(&bytes)
}

fn apply_pull_request_observation(
    store: &mut PullRequestStore,
    session_id: &SessionId,
    observation: PullRequestObservation,
    refreshed_at_ms: u64,
) -> anyhow::Result<Option<Option<PullRequestSummary>>> {
    store.transaction(|store| {
        apply_pull_request_observation_unpersisted(store, session_id, observation, refreshed_at_ms)
    })
}

fn apply_pull_request_observation_unpersisted(
    store: &mut PullRequestStore,
    session_id: &SessionId,
    observation: PullRequestObservation,
    refreshed_at_ms: u64,
) -> anyhow::Result<Option<Option<PullRequestSummary>>> {
    let previous = store.get(session_id);
    if previous
        .as_ref()
        .is_some_and(|pull_request| pull_request.state == PullRequestState::Merged)
    {
        return Ok(None);
    }
    if let Some(previous) = &previous {
        match &observation {
            PullRequestObservation::Trackable { number, url, .. }
            | PullRequestObservation::Closed { number, url }
                if pull_request_identity(&previous.url, previous.number)
                    != pull_request_identity(url, *number) =>
            {
                return Ok(None);
            }
            _ => {}
        }
    }
    let (next, summary) = match observation {
        PullRequestObservation::Trackable { number, url, state } => {
            let stored = StoredPullRequest {
                session_id: session_id.as_str().to_owned(),
                url,
                number,
                state,
                refreshed_at_ms,
            };
            let summary = Some(stored.summary());
            (Some(stored), summary)
        }
        PullRequestObservation::Closed { .. } if previous.is_some() => (None, None),
        PullRequestObservation::Closed { .. } | PullRequestObservation::Unavailable => {
            return Ok(None);
        }
    };
    let previous_summary = previous.as_ref().map(StoredPullRequest::summary);
    store.replace_unpersisted(session_id, next)?;
    Ok((previous_summary != summary).then_some(summary))
}

async fn publish_pull_request_projection(
    plan: &PullRequestRefreshPlan,
    events: &mpsc::Sender<TimestampedEvent>,
    summary: Option<PullRequestSummary>,
) -> Result<(), ServiceError> {
    {
        let mut projection = plan.projection.lock().map_err(|_| ServiceError::Internal)?;
        if projection.as_ref() == summary.as_ref() {
            return Ok(());
        }
        // Replacement commands read this projection independently of event
        // delivery. Advance it first so an event already observed by the actor
        // can never be overwritten by a replacement built from stale evidence.
        *projection = summary.clone();
    }
    events
        .send(event(EventPayload::SessionPullRequestChanged {
            pull_request: summary,
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

async fn refresh_pull_request_projection(
    plan: &PullRequestRefreshPlan,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let executable = plan
        .process_execution_allowed
        .then(|| resolve_github_cli_executable(&plan.workspace))
        .flatten();
    refresh_pull_request_projection_inner(plan, events, executable.as_deref()).await
}

async fn refresh_pull_request_projection_inner(
    plan: &PullRequestRefreshPlan,
    events: &mpsc::Sender<TimestampedEvent>,
    executable: Option<&Path>,
) -> Result<(), ServiceError> {
    let pull_requests = Arc::clone(&plan.pull_requests);
    let session_id = plan.session_id.clone();
    let previous = tokio::task::spawn_blocking(move || {
        pull_requests
            .lock()
            .map_err(|_| ServiceError::Internal)
            .map(|pull_requests| pull_requests.get(&session_id))
    })
    .await
    .map_err(|_| ServiceError::Internal)??;
    publish_pull_request_projection(
        plan,
        events,
        previous.as_ref().map(StoredPullRequest::summary),
    )
    .await?;
    if previous
        .as_ref()
        .is_some_and(|pull_request| pull_request.state == PullRequestState::Merged)
        || (previous.is_none() && !plan.discovery_enabled.load(Ordering::Acquire))
        || !plan.process_execution_allowed
    {
        return Ok(());
    }
    let Some(executable) = executable else {
        return Ok(());
    };
    let observation = query_hosted_github_pull_request(
        &plan.workspace,
        previous
            .as_ref()
            .map(|pull_request| pull_request.url.as_str()),
        executable,
    )
    .await;
    let pull_requests = Arc::clone(&plan.pull_requests);
    let session_id = plan.session_id.clone();
    let current = tokio::task::spawn_blocking(move || {
        let mut pull_requests = pull_requests.lock().map_err(|_| ServiceError::Internal)?;
        if pull_requests.get(&session_id) == previous {
            apply_pull_request_observation(&mut pull_requests, &session_id, observation, now_ms())
                .map_err(|_| ServiceError::Internal)?;
        }
        Ok::<_, ServiceError>(pull_requests.summary(&session_id))
    })
    .await
    .map_err(|_| ServiceError::Internal)??;
    publish_pull_request_projection(plan, events, current).await
}

async fn run_hosted_pull_request_refresh(
    plan: PullRequestRefreshPlan,
    events: mpsc::Sender<TimestampedEvent>,
) {
    if !plan.process_execution_allowed {
        return;
    }
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + PULL_REQUEST_REFRESH_INTERVAL,
        PULL_REQUEST_REFRESH_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = events.closed() => return,
            _ = interval.tick() => {}
            () = plan.refresh_requested.notified() => {}
        }
        let _ = refresh_pull_request_projection(&plan, &events).await;
    }
}

#[cfg(test)]
async fn refresh_pull_request_projection_with_executable(
    plan: &WorkerPlan,
    events: &mpsc::Sender<TimestampedEvent>,
    executable: &Path,
) -> Result<(), ServiceError> {
    refresh_pull_request_projection_inner(
        &PullRequestRefreshPlan::from(plan),
        events,
        Some(executable),
    )
    .await
}

fn select_inactive_pull_request_batch(
    refreshable: Vec<StoredPullRequest>,
    hosted: &BTreeSet<SessionId>,
    attempted: &mut BTreeSet<String>,
    capacity: usize,
) -> Vec<StoredPullRequest> {
    let inactive = refreshable
        .into_iter()
        .filter(|pull_request| {
            !hosted
                .iter()
                .any(|session_id| session_id.as_str() == pull_request.session_id)
        })
        .collect::<Vec<_>>();
    let inactive_ids = inactive
        .iter()
        .map(|pull_request| pull_request.session_id.as_str())
        .collect::<BTreeSet<_>>();
    attempted.retain(|session_id| inactive_ids.contains(session_id.as_str()));
    if capacity == 0 || inactive.is_empty() {
        return Vec::new();
    }
    if inactive
        .iter()
        .all(|pull_request| attempted.contains(&pull_request.session_id))
    {
        attempted.clear();
    }
    let batch = inactive
        .into_iter()
        .filter(|pull_request| !attempted.contains(&pull_request.session_id))
        .take(capacity)
        .collect::<Vec<_>>();
    attempted.extend(
        batch
            .iter()
            .map(|pull_request| pull_request.session_id.clone()),
    );
    batch
}

async fn refresh_inactive_pull_requests_once(
    host: &Arc<OctetHost>,
    supervisor: &Arc<SessionSupervisor<OctetHost>>,
    pending_catalog: &mut BTreeSet<SessionId>,
    attempted: &mut BTreeSet<String>,
    executable: &Path,
) {
    if !host.config.sandbox.process_execution_allowed() {
        return;
    }
    let hosted = supervisor.hosted_session_ids().await;
    let pull_requests = Arc::clone(&host.pull_requests);
    let refreshable = match tokio::task::spawn_blocking(move || {
        pull_requests
            .lock()
            .map_err(|_| ())
            .map(|pull_requests| pull_requests.refreshable())
    })
    .await
    {
        Ok(Ok(refreshable)) => refreshable,
        Ok(Err(())) | Err(_) => return,
    };
    let workspace = host.config.workspace.clone();
    let executable = executable.to_owned();
    // Match the one-shot batch width to permits available at its start. Keep a
    // round of attempted identities so temporary failures do not pin every
    // later inventory record behind the same oldest evidence.
    let query_concurrency = GITHUB_QUERY_PERMITS
        .available_permits()
        .min(MAX_CONCURRENT_GITHUB_QUERIES);
    let refreshable =
        select_inactive_pull_request_batch(refreshable, &hosted, attempted, query_concurrency);
    let observations = if query_concurrency == 0 {
        Vec::new()
    } else {
        futures_util::stream::iter(refreshable.into_iter().map(|stored| {
            let workspace = workspace.clone();
            let executable = executable.clone();
            async move {
                let observation =
                    query_github_pull_request(&workspace, Some(stored.url.as_str()), &executable)
                        .await;
                (stored, observation)
            }
        }))
        .buffer_unordered(query_concurrency)
        .collect::<Vec<_>>()
        .await
    };

    let refreshed_at_ms = now_ms();
    let pull_requests = Arc::clone(&host.pull_requests);
    let catalog_changes = tokio::task::spawn_blocking(move || {
        let Ok(mut pull_requests) = pull_requests.lock() else {
            return BTreeSet::new();
        };
        let _ = pull_requests.transaction(|pull_requests| {
            for (expected, observation) in observations {
                let session_id = SessionId::new(expected.session_id.clone())
                    .expect("stored pull-request session ID");
                if pull_requests.get(&session_id).as_ref() != Some(&expected) {
                    continue;
                }
                apply_pull_request_observation_unpersisted(
                    pull_requests,
                    &session_id,
                    observation,
                    refreshed_at_ms,
                )?;
            }
            Ok(())
        });
        pull_requests.take_catalog_changes()
    })
    .await
    .unwrap_or_default();
    // A hosted refresh can persist evidence just as its actor retires, after
    // the actor has stopped consuming driver events. Reconcile every durable
    // state change through the inactive ownership fence so such handoffs cannot
    // strand a stale catalog projection, including terminal merges or closure.
    pending_catalog.extend(catalog_changes);

    for session_id in pending_catalog.iter().cloned().collect::<Vec<_>>() {
        let summary_host = Arc::clone(host);
        let summary_session_id = session_id.clone();
        let summary = match tokio::task::spawn_blocking(move || {
            summary_host.stored_session_summary(&summary_session_id)
        })
        .await
        {
            Ok(Ok(summary)) => summary,
            Ok(Err(ServiceError::NotFound)) => {
                pending_catalog.remove(&session_id);
                continue;
            }
            Ok(Err(_)) | Err(_) => continue,
        };
        if let Ok(true) = supervisor.publish_inactive_catalog_summary(summary).await {
            pending_catalog.remove(&session_id);
        }
    }
}

async fn run_pull_request_catalog_refresh(
    host: Arc<OctetHost>,
    supervisor: Arc<SessionSupervisor<OctetHost>>,
) {
    if !host.config.sandbox.process_execution_allowed() {
        return;
    }
    let mut pending_catalog = BTreeSet::new();
    let mut attempted = BTreeSet::new();
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + PULL_REQUEST_REFRESH_INTERVAL,
        PULL_REQUEST_REFRESH_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some(executable) = resolve_github_cli_executable(&host.config.workspace) else {
            continue;
        };
        refresh_inactive_pull_requests_once(
            &host,
            &supervisor,
            &mut pending_catalog,
            &mut attempted,
            &executable,
        )
        .await;
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
struct CheckoutTestHooks {
    rollback_gate: Option<CheckoutRollbackGate>,
    corrupt_replacement_identity: bool,
    fail_seed_after_checkout: bool,
    fail_rollback: bool,
}

#[cfg(test)]
#[derive(Clone)]
struct CheckoutRollbackGate {
    entered: Arc<tokio::sync::Barrier>,
    release: Arc<tokio::sync::Barrier>,
}

enum PrivateResponse {
    Approval(Box<dyn FnOnce(bool) + Send + Sync>),
    Input(Box<dyn FnOnce(Option<Vec<u8>>) + Send + Sync>),
}

struct PrivateRequest {
    kind: RequestKind,
    response: PrivateResponse,
}

#[derive(Clone)]
struct ProjectedToolCall {
    name: String,
    arguments: serde_json::Value,
    activity: ToolActivity,
    result: Option<ToolResultSummary>,
    turn_id: TurnId,
}

#[derive(Clone, Default)]
struct ProjectedToolProgress {
    observed_output_bytes: u64,
    dropped_output_bytes: u64,
}

struct CompletedToolEvidence {
    tool_call_id: String,
    tool_item_id: ItemId,
    turn_id: TurnId,
    tool: ProjectedToolCall,
    output: ToolOutput,
}

struct PendingUserItem {
    id: ItemId,
    delivery: UserMessageDelivery,
    turn_id: TurnId,
    documents: Vec<DocumentReference>,
    project_files: Vec<TrustedFileEntry>,
    document_context_tokens: u64,
    project_file_context_tokens: u64,
    context_attributed: bool,
    branch_provenance: Option<ConversationBranchProvenance>,
}

struct RunContextProjection {
    usage_uncertain: bool,
    last_agent_snapshot: Option<AgentContextSnapshot>,
    last_published: Option<ContextUsage>,
    current_totals: Option<ContextTotals>,
    context_updated_at_ms: u64,
    active_compaction: Option<(u64, ActiveCompaction)>,
    last_compaction: Option<(u64, CompletedCompaction)>,
    project_instruction_tokens: u64,
    document_context_tokens: u64,
    project_file_context_tokens: u64,
}

impl RunContextProjection {
    fn new(
        project_instruction_tokens: u64,
        document_context_tokens: u64,
        project_file_context_tokens: u64,
    ) -> Self {
        Self {
            usage_uncertain: false,
            last_agent_snapshot: None,
            last_published: None,
            current_totals: None,
            context_updated_at_ms: 0,
            active_compaction: None,
            last_compaction: None,
            project_instruction_tokens,
            document_context_tokens,
            project_file_context_tokens,
        }
    }

    fn attribute_sources(&mut self, document_tokens: u64, project_file_tokens: u64) {
        self.document_context_tokens = self.document_context_tokens.saturating_add(document_tokens);
        self.project_file_context_tokens = self
            .project_file_context_tokens
            .saturating_add(project_file_tokens);
    }

    fn clear_auxiliary_sources(&mut self) {
        self.document_context_tokens = 0;
        self.project_file_context_tokens = 0;
    }
}

struct ResolvedPromptInput {
    display_text: String,
    model_text: String,
    attachments: Vec<AttachmentRef>,
    documents: Vec<DocumentReference>,
    project_files: Vec<TrustedFileEntry>,
    document_context_tokens: u64,
    project_file_context_tokens: u64,
}

enum RunPromptInput {
    New(PromptInput),
    Replay(ResolvedPromptInput),
}

enum RunDriveOutcome {
    Admitted {
        goal: Option<octet_agent::GoalDecision>,
    },
    Rejected {
        admission: Option<oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>>,
        error: ServiceError,
    },
}

struct ProjectionState {
    usage_uncertain: bool,
    last_context: Option<ContextUsage>,
    known_entries: usize,
    run_counter: u64,
    user_item_counter: u64,
    request_counter: u64,
    turn_counter: u64,
    provider_attempt: u32,
    assistant_item: Option<ItemId>,
    reasoning_item: Option<ItemId>,
    completed_assistant_items: VecDeque<Option<(ItemId, TurnId)>>,
    completed_reasoning_items: VecDeque<Option<(ItemId, TurnId)>>,
    tool_items: HashMap<String, ItemId>,
    tool_calls: HashMap<String, ProjectedToolCall>,
    pending_tool_evidence: VecDeque<CompletedToolEvidence>,
    tool_progress: HashMap<String, ProjectedToolProgress>,
    test_results: Vec<StructuredTestResults>,
    item_turns: HashMap<ItemId, TurnId>,
    run_started_at_ms: u64,
    private_requests: HashMap<RequestId, PrivateRequest>,
    pending_attachments: VecDeque<Vec<AttachmentRef>>,
    pending_user_items: VecDeque<PendingUserItem>,
    extension_presentations: Vec<ExtensionPresentation>,
}

impl ProjectionState {
    fn new(known_entries: usize) -> Self {
        Self {
            usage_uncertain: false,
            last_context: None,
            known_entries,
            run_counter: 0,
            user_item_counter: 0,
            request_counter: 0,
            turn_counter: 1,
            provider_attempt: 1,
            assistant_item: None,
            reasoning_item: None,
            completed_assistant_items: VecDeque::new(),
            completed_reasoning_items: VecDeque::new(),
            tool_items: HashMap::new(),
            tool_calls: HashMap::new(),
            pending_tool_evidence: VecDeque::new(),
            tool_progress: HashMap::new(),
            test_results: Vec::new(),
            item_turns: HashMap::new(),
            run_started_at_ms: now_ms(),
            private_requests: HashMap::new(),
            pending_attachments: VecDeque::new(),
            pending_user_items: VecDeque::new(),
            extension_presentations: Vec::new(),
        }
    }

    fn next_run_id(&mut self, generation: u64) -> Result<RunId, ServiceError> {
        self.run_counter = self
            .run_counter
            .checked_add(1)
            .ok_or(ServiceError::Internal)?;
        RunId::new(format!("run-{generation}-{}", self.run_counter))
            .map_err(|_| ServiceError::Internal)
    }

    fn begin_run(&mut self) {
        self.user_item_counter = 0;
        self.turn_counter = 1;
        self.provider_attempt = 1;
        self.assistant_item = None;
        self.reasoning_item = None;
        self.completed_assistant_items.clear();
        self.completed_reasoning_items.clear();
        self.tool_items.clear();
        self.tool_calls.clear();
        self.tool_progress.clear();
        self.test_results.clear();
        self.item_turns.clear();
        self.run_started_at_ms = now_ms();
        self.private_requests.clear();
        self.pending_attachments.clear();
        self.pending_user_items.clear();
    }

    fn next_user_item_id(&mut self, run_id: &RunId) -> Result<ItemId, ServiceError> {
        self.user_item_counter = self
            .user_item_counter
            .checked_add(1)
            .ok_or(ServiceError::Internal)?;
        self.provisional_id(run_id, "user", self.user_item_counter)
    }

    fn turn_id(&self, run_id: &RunId) -> Result<TurnId, ServiceError> {
        TurnId::new(format!("turn-{}-{}", run_id.as_str(), self.turn_counter))
            .map_err(|_| ServiceError::Internal)
    }

    fn provisional_id(
        &self,
        run_id: &RunId,
        kind: &str,
        suffix: u64,
    ) -> Result<ItemId, ServiceError> {
        ItemId::new(format!(
            "item-{}-{kind}-{}-{suffix}",
            run_id.as_str(),
            self.turn_counter
        ))
        .map_err(|_| ServiceError::Internal)
    }

    fn finish_turn(&mut self) {
        let turn_id = self
            .assistant_item
            .as_ref()
            .or(self.reasoning_item.as_ref())
            .and_then(|item_id| self.item_turns.get(item_id))
            .cloned();
        self.completed_assistant_items
            .push_back(self.assistant_item.take().zip(turn_id.clone()));
        self.completed_reasoning_items
            .push_back(self.reasoning_item.take().zip(turn_id));
        self.turn_counter = self.turn_counter.saturating_add(1);
        self.provider_attempt = 1;
    }
}

fn collect_extension_presentations(
    extensions: &mut crate::extensions::ExecutableExtensions,
) -> Vec<ExtensionPresentation> {
    let _ = extensions.drain_events();
    extensions
        .presentation_views()
        .into_iter()
        .map(|view| ExtensionPresentation {
            extension: view.extension,
            generation: view.generation,
            extension_instance_id: view.extension_instance_id,
            resource_owner: view.resource_owner,
            snapshot: view.snapshot,
        })
        .collect()
}

async fn publish_extension_presentations(
    extensions: &mut crate::extensions::ExecutableExtensions,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let presentations = collect_extension_presentations(extensions);
    if presentations == projection.extension_presentations {
        return Ok(());
    }
    projection.extension_presentations = presentations.clone();
    events
        .send(event(EventPayload::ExtensionPresentationsChanged {
            presentations,
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

fn schedule_goal(decision: Option<GoalDecision>) -> Option<tokio::time::Instant> {
    match decision {
        Some(GoalDecision::Wait { delay, .. }) => Some(tokio::time::Instant::now() + delay),
        _ => None,
    }
}

#[cfg(any())]
async fn run_worker(
    mut plan: WorkerPlan,
    mut commands: mpsc::Receiver<WorkerMessage>,
    events: mpsc::Sender<TimestampedEvent>,
    known_entries: usize,
) {
    let mut app: Option<App> = None;
    let mut projection = ProjectionState::new(known_entries);
    let pull_request_refresh = tokio::spawn(run_hosted_pull_request_refresh(
        PullRequestRefreshPlan::from(&plan),
        events.clone(),
    ));
    let goal_driver = plan.goal_store.as_ref().map(|store| {
        GoalDriver::new(
            Arc::new(ServeGoalStore {
                store: store.clone(),
            }),
            plan.session_id.as_str(),
        )
    });
    let mut goal_deadline = match goal_driver.as_ref() {
        Some(driver)
            if current_goal(plan.goal_store.as_ref(), &plan.session_id)
                .ok()
                .flatten()
                .is_some_and(|goal| matches!(goal.status, octet_agent::GoalStatus::Active)) =>
        {
            match driver.turn_settled(GoalTurnSource::User, "", false) {
                Ok(decision) => schedule_goal(Some(decision)),
                Err(_) => {
                    let _ = driver.session_error();
                    None
                }
            }
        }
        _ => None,
    };
    let mut extension_refresh = tokio::time::interval(std::time::Duration::from_millis(250));
    extension_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let message = tokio::select! {
            message = commands.recv() => message,
            _ = extension_refresh.tick() => {
                if let Some(owned_app) = app.as_mut() {
                    let _ = publish_extension_presentations(
                        &mut owned_app.executable_extensions,
                        &mut projection,
                        &events,
                    ).await;
                }
                continue;
            }
            _ = async {
                if let Some(deadline) = goal_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                goal_deadline = None;
                let Some(driver) = goal_driver.as_ref() else {
                    continue;
                };
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = driver.session_error();
                            continue;
                        }
                    },
                };
                let continuation = match driver.fire_continuation() {
                    Ok(continuation) => continuation,
                    Err(_) => {
                        let _ = driver.session_error();
                        None
                    }
                };
                let Some(continuation) = continuation else {
                    app = Some(owned_app);
                    continue;
                };
                if let Ok(goal_event) =
                    current_goal_event(plan.goal_store.as_ref(), &plan.session_id)
                {
                    let _ = events.send(goal_event).await;
                }
                let session_path = owned_app.agent.session().path().to_owned();
                plan.launch.session = SessionSelection::OpenExisting(session_path);
                let input = PromptInput {
                    text: continuation.prompt,
                    attachments: Vec::new(),
                    document_ids: Vec::new(),
                    project_file_ids: Vec::new(),
                };
                match start_and_drive_run(
                    &mut owned_app,
                    RunPromptInput::New(input),
                    None,
                    Some(driver),
                    GoalTurnSource::Continuation,
                    &plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    None,
                )
                .await
                {
                    Ok(RunDriveOutcome::Admitted { goal }) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                    }
                    Ok(RunDriveOutcome::Rejected { admission, error }) => {
                        let _ = driver.session_error();
                        if let Some(admission) = admission {
                            let _ = admission.send(Err(error));
                        }
                        app = Some(owned_app);
                    }
                    Err(_) => {
                        let _ = driver.session_error();
                        let _ = events
                            .send(event(EventPayload::SessionStateChanged {
                                state: SessionLiveState::Failed,
                                active_run_id: None,
                            }))
                            .await;
                        app = Some(owned_app);
                    }
                }
                continue;
            }
        };
        let Some(message) = message else {
            break;
        };
        let message = match message {
            WorkerMessage::Command(message) => message,
            WorkerMessage::CommandDiscovery { response } => {
                let result = match app.as_ref() {
                    Some(app) => build_command_discovery(app),
                    None => match build_worker_app(&mut plan) {
                        Ok(owned_app) => {
                            let discovery = build_command_discovery(&owned_app);
                            app = Some(owned_app);
                            discovery
                        }
                        Err(_) => Err(ServiceError::Internal),
                    },
                };
                let _ = response.send(result);
                continue;
            }
        };
        match message.command {
            command @ (SessionCommand::SetGoal { .. }
            | SessionCommand::PauseGoal
            | SessionCommand::ResumeGoal
            | SessionCommand::ClearGoal) => {
                let prior_goal_deadline = goal_deadline;
                let outcome = goal_mutation_outcome(&plan, command);
                goal_deadline = if outcome.is_ok() {
                    goal_deadline_after_user_change(goal_driver.as_ref()).unwrap_or_default()
                } else {
                    // Rejected mutations must not cancel a continuation that
                    // was already waiting for its grace-period deadline.
                    prior_goal_deadline
                };
                let _ = message.response.send(outcome);
            }
            SessionCommand::SubmitPrompt { input } => {
                goal_deadline = None;
                if let Some(driver) = goal_driver.as_ref() {
                    driver.user_spoke();
                }
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let session_path = owned_app.agent.session().path().to_owned();
                plan.launch.session = SessionSelection::OpenExisting(session_path);
                match start_and_drive_run(
                    &mut owned_app,
                    RunPromptInput::New(input),
                    None,
                    goal_driver.as_ref(),
                    GoalTurnSource::User,
                    &plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    Some(message.response),
                )
                .await
                {
                    Ok(RunDriveOutcome::Admitted { goal }) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                    }
                    Ok(RunDriveOutcome::Rejected { admission, error }) => {
                        if let Some(admission) = admission {
                            let _ = admission.send(Err(error));
                        }
                        app = Some(owned_app);
                    }
                    Err(_) => {
                        if let Some(driver) = goal_driver.as_ref() {
                            let _ = driver.session_error();
                        }
                        let _ = events
                            .send(event(EventPayload::SessionStateChanged {
                                state: SessionLiveState::Failed,
                                active_run_id: None,
                            }))
                            .await;
                        app = Some(owned_app);
                    }
                }
            }
            SessionCommand::EditUserTurn {
                source_user_entry_id,
                input,
            } => {
                goal_deadline = None;
                if let Some(driver) = goal_driver.as_ref() {
                    driver.user_spoke();
                }
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let source_entry = EntryId(source_user_entry_id.as_str().to_owned());
                if owned_app
                    .agent
                    .session()
                    .entry(&source_entry)
                    .is_none_or(|entry| !is_user_authored_entry(entry))
                {
                    app = Some(owned_app);
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                }
                let provenance = ConversationBranchProvenance {
                    operation: ConversationBranchOperation::EditUserTurn,
                    source_session_id: plan.session_id.clone(),
                    source_entry_id: source_user_entry_id,
                    originating_user_entry_id: None,
                    model_override: None,
                    external_effects_preserved: true,
                    warning: EXTERNAL_EFFECTS_WARNING.to_owned(),
                };
                match drive_sibling_conversation_branch(
                    owned_app,
                    source_entry,
                    RunPromptInput::New(input),
                    provenance,
                    None,
                    goal_driver.as_ref(),
                    &mut plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    message.response,
                )
                .await
                {
                    Ok((owned_app, post_ack_failed, goal)) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                        if post_ack_failed {
                            let _ = events
                                .send(event(EventPayload::SessionStateChanged {
                                    state: SessionLiveState::Failed,
                                    active_run_id: None,
                                }))
                                .await;
                        }
                    }
                    Err(_) => {
                        app = None;
                        break;
                    }
                }
            }
            SessionCommand::RetryResponse {
                source_assistant_entry_id,
                model,
            } => {
                goal_deadline = None;
                if let Some(driver) = goal_driver.as_ref() {
                    driver.user_spoke();
                }
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let assistant_entry = EntryId(source_assistant_entry_id.as_str().to_owned());
                let source_user_entry =
                    match retry_originating_user_entry(owned_app.agent.session(), &assistant_entry)
                    {
                        Ok(entry) => entry,
                        Err(error) => {
                            app = Some(owned_app);
                            let _ = message.response.send(Err(error));
                            continue;
                        }
                    };
                let replay =
                    match replay_prompt_input(owned_app.agent.session(), &source_user_entry, &plan)
                    {
                        Ok(replay) => replay,
                        Err(error) => {
                            app = Some(owned_app);
                            let _ = message.response.send(Err(error));
                            continue;
                        }
                    };
                let originating_user_entry_id =
                    match DurableEntryId::new(source_user_entry.0.clone()) {
                        Ok(entry) => entry,
                        Err(_) => {
                            app = Some(owned_app);
                            let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                            continue;
                        }
                    };
                let provenance = ConversationBranchProvenance {
                    operation: ConversationBranchOperation::RetryResponse,
                    source_session_id: plan.session_id.clone(),
                    source_entry_id: source_assistant_entry_id,
                    originating_user_entry_id: Some(originating_user_entry_id),
                    model_override: model.clone(),
                    external_effects_preserved: true,
                    warning: EXTERNAL_EFFECTS_WARNING.to_owned(),
                };
                match drive_sibling_conversation_branch(
                    owned_app,
                    source_user_entry,
                    RunPromptInput::Replay(replay),
                    provenance,
                    model,
                    goal_driver.as_ref(),
                    &mut plan,
                    &mut projection,
                    &mut commands,
                    &events,
                    message.response,
                )
                .await
                {
                    Ok((owned_app, post_ack_failed, goal)) => {
                        goal_deadline = schedule_goal(goal);
                        app = Some(owned_app);
                        if post_ack_failed {
                            let _ = events
                                .send(event(EventPayload::SessionStateChanged {
                                    state: SessionLiveState::Failed,
                                    active_run_id: None,
                                }))
                                .await;
                        }
                    }
                    Err(_) => {
                        app = None;
                        break;
                    }
                }
            }
            SessionCommand::ForkConversation { entry_id } => {
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let sessions = plan.sessions.clone();
                let source_session_id = plan.session_id.clone();
                let project_id = plan.project_id.clone();
                let projects = Arc::clone(&plan.projects);
                let fork = tokio::task::spawn_blocking(move || {
                    let result = create_conversation_fork(
                        &owned_app,
                        &sessions,
                        &source_session_id,
                        project_id.as_ref(),
                        &projects,
                        &entry_id,
                    );
                    (owned_app, result)
                })
                .await;
                let (owned_app, result) = match fork {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        app = None;
                        let _ = message.response.send(Err(ServiceError::Internal));
                        continue;
                    }
                };
                match result {
                    Ok(created_session_id) => {
                        let outcome = DriverCommandOutcome::fork(created_session_id.clone());
                        if message.response.send(Ok(outcome)).is_err() {
                            let _ = rollback_conversation_fork(&plan, &created_session_id);
                        }
                        app = Some(owned_app);
                    }
                    Err(error) => {
                        app = Some(owned_app);
                        let _ = message.response.send(Err(error));
                    }
                }
            }
            SessionCommand::Checkout { entry_id } => {
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let path = owned_app.agent.session().path().to_owned();
                let Some(previous_head) = owned_app.agent.session().head() else {
                    app = Some(owned_app);
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                };
                if owned_app
                    .agent
                    .session_mut()
                    .checkout(EntryId(entry_id.as_str().to_owned()))
                    .is_err()
                {
                    app = Some(owned_app);
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                }

                let selection = SessionSelection::OpenExisting(path.clone());
                let rebuilt =
                    match rebuild_app(owned_app, None, None, None, Some(selection.clone())) {
                        Ok(rebuilt) => rebuilt,
                        Err(_) => {
                            match checkout_rejection_after_rollback(
                                restore_checkout_owner(&path, previous_head, &mut plan),
                                ServiceError::Internal,
                            ) {
                                Ok((restored, rejection)) => {
                                    app = Some(restored);
                                    let _ = message.response.send(Err(rejection));
                                    continue;
                                }
                                Err(owner_lost) => {
                                    app = None;
                                    let _ = message.response.send(Err(owner_lost));
                                    break;
                                }
                            }
                        }
                    };
                let model = selection_for_model(&rebuilt.model, &rebuilt.reasoning, &plan.config);
                let mut replacement = seed_from_session(
                    rebuilt.agent.session(),
                    plan.session_id.clone(),
                    SessionSeedOptions {
                        workspace: &plan.config.workspace,
                        project_id: plan.project_id.clone(),
                        model,
                        authority: plan.authority,
                        generation: plan.actor_generation,
                        meta: plan
                            .sessions
                            .meta_for_open_session(
                                plan.session_id.as_str(),
                                rebuilt.agent.session(),
                            )
                            .ok()
                            .flatten(),
                        attachment_store: plan.attachments.as_ref(),
                        resource_store: plan.resources.as_ref(),
                    },
                );
                if let (Ok(seed), Ok(pull_request)) =
                    (replacement.as_mut(), plan.pull_request_projection.lock())
                {
                    // Projection replacement runs on the serialized command
                    // worker. Read its in-memory actor projection rather than
                    // contending with blocking-pool evidence persistence.
                    seed.summary.pull_request = pull_request.clone();
                }
                #[cfg(test)]
                {
                    if plan.checkout_hooks.fail_seed_after_checkout {
                        replacement = Err(ServiceError::InvalidSeed);
                    } else if plan.checkout_hooks.corrupt_replacement_identity {
                        if let Ok(seed) = replacement.as_mut() {
                            let wrong =
                                SessionId::new("test-corrupt-replacement").expect("test ID");
                            seed.summary.id = wrong.clone();
                            seed.snapshot.session_id = wrong;
                        }
                    }
                }
                match replacement {
                    Ok(seed) => {
                        let (outcome, mut finalizer) = DriverCommandOutcome::guarded_replace(seed);
                        let _ = message.response.send(Ok(outcome));
                        match finalizer.decision().await {
                            Ok(FinalizeDecision::Commit) => {
                                plan.launch.model = rebuilt.model.spec.id.clone();
                                plan.launch.reasoning = rebuilt.reasoning.clone();
                                plan.launch.reasoning_mode = rebuilt.reasoning_mode;
                                plan.launch.session = selection;
                                projection.begin_run();
                                projection.known_entries = rebuilt.agent.session().entries().len();
                                app = Some(rebuilt);
                                let _ = finalizer.complete(Ok(FinalizeCompletion::Committed));
                            }
                            Ok(FinalizeDecision::Rollback) => {
                                wait_for_checkout_rollback_gate(&plan).await;
                                match rollback_checkout_candidate(
                                    rebuilt,
                                    &path,
                                    previous_head,
                                    &mut plan,
                                ) {
                                    Ok(restored) => {
                                        app = Some(restored);
                                        let _ =
                                            finalizer.complete(Ok(FinalizeCompletion::RolledBack));
                                    }
                                    Err(_) => {
                                        app = None;
                                        let _ = finalizer.complete(Err(ServiceError::OwnerLost));
                                    }
                                }
                            }
                            Err(_) => {
                                wait_for_checkout_rollback_gate(&plan).await;
                                app = rollback_checkout_candidate(
                                    rebuilt,
                                    &path,
                                    previous_head,
                                    &mut plan,
                                )
                                .ok();
                                if app.is_none() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        match checkout_rejection_after_rollback(
                            rollback_checkout_candidate(rebuilt, &path, previous_head, &mut plan),
                            error,
                        ) {
                            Ok((restored, rejection)) => {
                                app = Some(restored);
                                let _ = message.response.send(Err(rejection));
                            }
                            Err(owner_lost) => {
                                app = None;
                                let _ = message.response.send(Err(owner_lost));
                                break;
                            }
                        }
                    }
                }
            }
            SessionCommand::InvokeExtensionAction {
                extension,
                extension_instance_id,
                generation,
                revision,
                action,
                confirmed,
            } => {
                let mut owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let result = owned_app
                    .executable_extensions
                    .execute_presentation_action_for_serve(
                        &extension,
                        &extension_instance_id,
                        generation,
                        revision,
                        &action,
                        confirmed,
                    )
                    .await
                    .map(|_| DriverCommandOutcome::default())
                    .map_err(|_| ServiceError::InvalidBoundary);
                if result.is_ok() {
                    let _ = publish_extension_presentations(
                        &mut owned_app.executable_extensions,
                        &mut projection,
                        &events,
                    )
                    .await;
                }
                app = Some(owned_app);
                let _ = message.response.send(result);
            }
            SessionCommand::InvokeSlashCommand { invocation } => {
                let owned_app = match app.take() {
                    Some(app) => app,
                    None => match build_worker_app(&mut plan) {
                        Ok(app) => app,
                        Err(_) => {
                            let _ = message.response.send(Err(ServiceError::Internal));
                            continue;
                        }
                    },
                };
                let (next_app, mut result) =
                    invoke_idle_slash_command(owned_app, invocation, &mut plan, &mut projection)
                        .await;
                if let Some(owned_app) = next_app.as_ref() {
                    projection.usage_uncertain |= owned_app.agent.session().has_uncertain_usage();
                    if let Err(error) =
                        publish_idle_accounting_context(&mut projection, &events).await
                    {
                        result = Err(error);
                    }
                }
                match (next_app, result) {
                    (Some(mut owned_app), Ok(SlashInvocationOutcome::Start(input))) => {
                        let session_path = owned_app.agent.session().path().to_owned();
                        plan.launch.session = SessionSelection::OpenExisting(session_path);
                        match start_and_drive_run(
                            &mut owned_app,
                            input,
                            None,
                            goal_driver.as_ref(),
                            GoalTurnSource::User,
                            &plan,
                            &mut projection,
                            &mut commands,
                            &events,
                            Some(message.response),
                        )
                        .await
                        {
                            Ok(RunDriveOutcome::Admitted { goal }) => {
                                goal_deadline = schedule_goal(goal);
                                app = Some(owned_app);
                            }
                            Ok(RunDriveOutcome::Rejected { admission, error }) => {
                                if let Some(admission) = admission {
                                    let _ = admission.send(Err(error));
                                }
                                app = Some(owned_app);
                            }
                            Err(_) => {
                                let _ = events
                                    .send(event(EventPayload::SessionStateChanged {
                                        state: SessionLiveState::Failed,
                                        active_run_id: None,
                                    }))
                                    .await;
                                app = Some(owned_app);
                            }
                        }
                    }
                    (Some(owned_app), Ok(SlashInvocationOutcome::Immediate(outcome))) => {
                        app = Some(owned_app);
                        let _ = message.response.send(Ok(*outcome));
                    }
                    (Some(owned_app), Err(error)) => {
                        app = Some(owned_app);
                        let _ = message.response.send(Err(error));
                    }
                    (None, _) => {
                        let _ = message.response.send(Err(ServiceError::OwnerLost));
                    }
                }
            }
            SessionCommand::ChangeModel { provider, model } => {
                let Some(summary) = plan
                    .available_models
                    .iter()
                    .find(|summary| summary.provider == provider && summary.id == model)
                    .cloned()
                else {
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                };
                let outcome = if let Some(owned_app) = app.take() {
                    match crate::app::apply_reconfig(
                        owned_app,
                        Reconfig::Model(ModelId(model.clone())),
                    ) {
                        Ok(rebuilt) => {
                            plan.launch.model = rebuilt.model.spec.id.clone();
                            plan.launch.reasoning = rebuilt.reasoning.clone();
                            plan.launch.session = SessionSelection::OpenExisting(
                                rebuilt.agent.session().path().to_owned(),
                            );
                            let selection = selection_for_model(
                                &rebuilt.model,
                                &rebuilt.reasoning,
                                &plan.config,
                            );
                            let outcome = reconfiguration_outcome(
                                &rebuilt,
                                &plan,
                                &mut projection,
                                selection,
                                plan.authority,
                            );
                            app = Some(rebuilt);
                            outcome
                        }
                        Err(_) => {
                            app = build_worker_app(&mut plan).ok();
                            Err(ServiceError::Internal)
                        }
                    }
                } else {
                    let next_reasoning_label = summary
                        .default_reasoning
                        .clone()
                        .or_else(|| summary.reasoning.first().cloned())
                        .unwrap_or_else(|| "off".into());
                    let next_reasoning = config::parse_reasoning(&next_reasoning_label)
                        .unwrap_or(ReasoningConfig::Off);
                    let previous_model = plan.launch.model.clone();
                    let previous_reasoning = plan.launch.reasoning.clone();
                    plan.launch.model = ModelId(model);
                    plan.launch.reasoning = next_reasoning;
                    let selection = ModelSelection {
                        provider,
                        model: plan.launch.model.0.clone(),
                        reasoning: next_reasoning_label,
                    };
                    match persist_idle_selection(&mut plan, &mut projection, selection) {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            plan.launch.model = previous_model;
                            plan.launch.reasoning = previous_reasoning;
                            Err(error)
                        }
                    }
                };
                let _ = message.response.send(outcome);
            }
            SessionCommand::ChangeReasoning { reasoning } => {
                let Some(summary) = plan
                    .available_models
                    .iter()
                    .find(|summary| summary.id == plan.launch.model.0)
                    .cloned()
                else {
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                };
                if !summary.reasoning.iter().any(|choice| choice == &reasoning) {
                    let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                    continue;
                }
                let provider = summary.provider.clone();
                let parsed = match config::parse_reasoning(&reasoning) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                        continue;
                    }
                };
                let outcome = if let Some(owned_app) = app.take() {
                    match crate::app::apply_reconfig(owned_app, Reconfig::Thinking(parsed.clone()))
                    {
                        Ok(rebuilt) => {
                            plan.launch.model = rebuilt.model.spec.id.clone();
                            plan.launch.reasoning = rebuilt.reasoning.clone();
                            plan.launch.session = SessionSelection::OpenExisting(
                                rebuilt.agent.session().path().to_owned(),
                            );
                            let selection = selection_for_model(
                                &rebuilt.model,
                                &rebuilt.reasoning,
                                &plan.config,
                            );
                            let outcome = reconfiguration_outcome(
                                &rebuilt,
                                &plan,
                                &mut projection,
                                selection,
                                plan.authority,
                            );
                            app = Some(rebuilt);
                            outcome
                        }
                        Err(_) => {
                            app = build_worker_app(&mut plan).ok();
                            Err(ServiceError::Internal)
                        }
                    }
                } else {
                    let previous_reasoning = plan.launch.reasoning.clone();
                    plan.launch.reasoning = parsed;
                    let selection = ModelSelection {
                        provider,
                        model: plan.launch.model.0.clone(),
                        reasoning,
                    };
                    match persist_idle_selection(&mut plan, &mut projection, selection) {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            plan.launch.reasoning = previous_reasoning;
                            Err(error)
                        }
                    }
                };
                let _ = message.response.send(outcome);
            }
            SessionCommand::Rename { title } => {
                let _ = message.response.send(rename_session_outcome(&plan, &title));
            }
            SessionCommand::SetPinned { pinned } => {
                let _ = message.response.send(pin_session_outcome(&plan, pinned));
            }
            SessionCommand::SetArchived { archived } => {
                let _ = message
                    .response
                    .send(archive_session_outcome(&plan, archived));
            }
            _ => {
                let _ = message.response.send(Err(ServiceError::InvalidBoundary));
            }
        }
    }
    pull_request_refresh.abort();
    let _ = pull_request_refresh.await;
    shutdown_worker_app(&mut app).await;
}

enum SlashInvocationOutcome {
    Start(RunPromptInput),
    Immediate(Box<DriverCommandOutcome>),
}

impl SlashInvocationOutcome {
    fn immediate(outcome: DriverCommandOutcome) -> Self {
        Self::Immediate(Box::new(outcome))
    }
}

fn self_help_prompt(topic: Option<&str>) -> String {
    let subject = topic
        .map(|topic| format!("the octet command or topic `{topic}`"))
        .unwrap_or_else(|| "octet's commands and workflow".to_owned());
    format!(
        "Give a concise self-help answer about {subject}. If this workspace is a octet source checkout, consult its README.md, docs/, examples/, and relevant Rust crates with the available tools before answering. Include practical details and mention how a user can inspect or extend octet when relevant."
    )
}

/// Executes one slash invocation at an idle worker boundary. The command is
/// parsed from the same grammar as the TUI, but only durable/session-safe
/// outcomes cross the graphical protocol boundary.
async fn invoke_idle_slash_command(
    app: App,
    invocation: SlashCommandInvocation,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
) -> (Option<App>, Result<SlashInvocationOutcome, ServiceError>) {
    let parsed = commands::parse(&invocation.invocation);
    match parsed {
        commands::Command::Changelog => (Some(app), Err(ServiceError::InvalidBoundary)),
        commands::Command::Help(topic) => (
            Some(app),
            Ok(SlashInvocationOutcome::Start(RunPromptInput::New(
                PromptInput {
                    text: self_help_prompt(topic.as_deref()),
                    attachments: Vec::new(),
                    document_ids: Vec::new(),
                    project_file_ids: Vec::new(),
                },
            ))),
        ),
        commands::Command::Compact => {
            let mut app = app;
            let original_keep_recent_tokens = app.config.compaction.keep_recent_tokens;
            app.config.compaction.keep_recent_tokens = 1;
            let result = attempt_compaction(&mut app).await;
            app.config.compaction.keep_recent_tokens = original_keep_recent_tokens;
            // Failed/cancelled compaction may still have provider accounting.
            // Reconcile it even when no conversation entries were appended.
            let outcome = finish_idle_compaction(&app, plan, projection, result.is_ok());
            (Some(app), outcome)
        }
        commands::Command::Model(Some(model)) => {
            let supported = plan
                .available_models
                .iter()
                .any(|summary| summary.id == model && summary.available);
            if !supported {
                return (Some(app), Err(ServiceError::InvalidBoundary));
            }
            apply_slash_reconfiguration(app, Reconfig::Model(ModelId(model)), plan, projection)
        }
        commands::Command::Thinking(Some(reasoning)) => {
            let level = match config::ThinkingLevel::parse(&reasoning) {
                Ok(level) => level,
                Err(_) => return (Some(app), Err(ServiceError::InvalidBoundary)),
            };
            let reasoning = match crate::app::thinking_to_reasoning_with_subagents(
                level,
                &app.model,
                app.subagents_available(),
            ) {
                Ok(reasoning) => reasoning,
                Err(_) => return (Some(app), Err(ServiceError::InvalidBoundary)),
            };
            apply_slash_reconfiguration(app, Reconfig::Thinking(reasoning), plan, projection)
        }
        commands::Command::Reload
        | commands::Command::Extensions(commands::ExtensionsSubcommand::Reload)
        | commands::Command::Skills(commands::SkillsSubcommand::Reload) => {
            reload_slash_resources(app, plan, projection)
        }
        commands::Command::Skills(subcommand) => {
            let mut app = app;
            let outcome = execute_slash_skills_command(&mut app, subcommand, plan, projection)
                .map(SlashInvocationOutcome::immediate);
            (Some(app), outcome)
        }
        commands::Command::Prompt(Some(invocation)) => {
            let mut app = app;
            let outcome = match slash_name_and_arguments(&invocation) {
                Some((name, arguments)) => start_prompt_template(&mut app, name, arguments)
                    .map(SlashInvocationOutcome::Start),
                None => Err(ServiceError::InvalidBoundary),
            };
            (Some(app), outcome)
        }
        commands::Command::Unknown(invocation) => {
            let mut app = app;
            let outcome = invoke_dynamic_slash_command(&mut app, &invocation).await;
            (Some(app), outcome)
        }
        commands::Command::Name(Some(title)) => (
            Some(app),
            rename_session_outcome(plan, &title).map(SlashInvocationOutcome::immediate),
        ),
        commands::Command::Name(None)
        | commands::Command::Prompt(None)
        | commands::Command::Extensions(
            commands::ExtensionsSubcommand::Menu | commands::ExtensionsSubcommand::Status,
        ) => (
            Some(app),
            Ok(SlashInvocationOutcome::immediate(
                DriverCommandOutcome::default(),
            )),
        ),
        _ => (Some(app), Err(ServiceError::InvalidBoundary)),
    }
}

fn apply_slash_reconfiguration(
    app: App,
    reconfig: Reconfig,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
) -> (Option<App>, Result<SlashInvocationOutcome, ServiceError>) {
    match crate::app::apply_reconfig(app, reconfig) {
        Ok(rebuilt) => {
            plan.launch.model = rebuilt.model.spec.id.clone();
            plan.launch.reasoning = rebuilt.reasoning.clone();
            plan.launch.session =
                SessionSelection::OpenExisting(rebuilt.agent.session().path().to_owned());
            let selection = selection_for_model(&rebuilt.model, &rebuilt.reasoning, &plan.config);
            let outcome =
                reconfiguration_outcome(&rebuilt, plan, projection, selection, plan.authority)
                    .map(SlashInvocationOutcome::immediate);
            (Some(rebuilt), outcome)
        }
        Err(_) => (build_worker_app(plan).ok(), Err(ServiceError::Internal)),
    }
}

fn reload_slash_resources(
    app: App,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
) -> (Option<App>, Result<SlashInvocationOutcome, ServiceError>) {
    let mut app = app;
    let system = match compose_instructions(&app.config) {
        Ok(system) => system,
        Err(_) => return (Some(app), Err(ServiceError::Internal)),
    };
    app.system_tokens = crate::compaction::estimate_text_tokens(&system);
    app.system = system;
    match rebuild_app(app, None, None, None, None) {
        Ok(rebuilt) => {
            plan.launch.model = rebuilt.model.spec.id.clone();
            plan.launch.reasoning = rebuilt.reasoning.clone();
            plan.launch.session =
                SessionSelection::OpenExisting(rebuilt.agent.session().path().to_owned());
            let outcome = idle_mutation_outcome(&rebuilt, plan, projection)
                .map(SlashInvocationOutcome::immediate);
            (Some(rebuilt), outcome)
        }
        Err(_) => (build_worker_app(plan).ok(), Err(ServiceError::Internal)),
    }
}

fn execute_slash_skills_command(
    app: &mut App,
    subcommand: commands::SkillsSubcommand,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
) -> Result<DriverCommandOutcome, ServiceError> {
    match subcommand {
        commands::SkillsSubcommand::Load(id) => {
            let loaded = app
                .skills
                .load(&id)
                .map_err(|_| ServiceError::InvalidBoundary)?;
            validate_skill_requirements(&loaded.descriptor, &app.agent.registered_tool_names())
                .map_err(|_| ServiceError::InvalidBoundary)?;
            app.agent
                .session_mut()
                .append(EntryValue::SkillActivated {
                    descriptor: loaded.descriptor,
                    instructions_hash: loaded.content_hash,
                    instructions: loaded.instructions,
                })
                .map_err(|_| ServiceError::Internal)?;
            idle_mutation_outcome(app, plan, projection)
        }
        commands::SkillsSubcommand::Off(id) => {
            let activation_id = app
                .agent
                .session()
                .head_ref()
                .and_then(|head| app.agent.session().resolve_active_skills(head).ok())
                .and_then(|state| {
                    state
                        .active_skills
                        .into_iter()
                        .find(|skill| skill.descriptor.id == id)
                        .map(|skill| skill.activation_id)
                })
                .ok_or(ServiceError::InvalidBoundary)?;
            app.agent
                .session_mut()
                .append(EntryValue::SkillDeactivated {
                    activation_id,
                    skill_id: id,
                })
                .map_err(|_| ServiceError::Internal)?;
            idle_mutation_outcome(app, plan, projection)
        }
        commands::SkillsSubcommand::List
        | commands::SkillsSubcommand::Show(_)
        | commands::SkillsSubcommand::Active
        | commands::SkillsSubcommand::Search(_) => Ok(DriverCommandOutcome::default()),
        commands::SkillsSubcommand::Reload => Err(ServiceError::InvalidBoundary),
    }
}

fn finish_idle_compaction(
    app: &App,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    succeeded: bool,
) -> Result<SlashInvocationOutcome, ServiceError> {
    projection.usage_uncertain |= app.agent.session().has_uncertain_usage();
    if let Err(error) = sync_session_usage(&plan.usage, &plan.session_id, app.agent.session()) {
        projection.usage_uncertain = true;
        return Err(error);
    }
    if !succeeded {
        return Err(ServiceError::Internal);
    }
    idle_mutation_outcome(app, plan, projection).map(SlashInvocationOutcome::immediate)
}

async fn publish_idle_accounting_context(
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    if !projection.usage_uncertain
        || projection
            .last_context
            .as_ref()
            .is_some_and(|context| context.usage_uncertain)
    {
        return Ok(());
    }
    let mut context = projection.last_context.clone().unwrap_or_default();
    context.usage_uncertain = true;
    events
        .send(event(EventPayload::ContextUpdated {
            context: context.clone(),
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    projection.last_context = Some(context);
    Ok(())
}

fn idle_mutation_outcome(
    app: &App,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
) -> Result<DriverCommandOutcome, ServiceError> {
    let branch_start = projection.known_entries;
    let items = project_new_entries(
        app.agent.session(),
        &plan.config.workspace,
        projection,
        None,
        None,
        plan.attachments.as_ref(),
        &plan.session_id,
    )?;
    if projection.known_entries == branch_start {
        return Ok(DriverCommandOutcome::default());
    }
    let mut events = items
        .into_iter()
        .map(|item| event(EventPayload::ItemCommitted { item }))
        .collect::<Vec<_>>();
    events.extend(branch_delta_events(app.agent.session(), branch_start)?);
    Ok(DriverCommandOutcome::with_events(events))
}

fn slash_name_and_arguments(invocation: &str) -> Option<(&str, &str)> {
    let invocation = invocation.trim().trim_start_matches('/');
    let end = invocation
        .find(char::is_whitespace)
        .unwrap_or(invocation.len());
    let name = &invocation[..end];
    (!name.is_empty()).then(|| (name, invocation[end..].trim_start()))
}

fn start_prompt_template(
    app: &mut App,
    name: &str,
    arguments: &str,
) -> Result<RunPromptInput, ServiceError> {
    if !app.prompts.contains(name) {
        return Err(ServiceError::InvalidBoundary);
    }
    let prompts = app.prompts.clone();
    let workspace = app.config.workspace.clone();
    let rendered = crate::prompts::render_and_record(
        &prompts,
        app.agent.session_mut(),
        &workspace,
        name,
        arguments,
        None,
    )
    .map_err(|_| ServiceError::InvalidBoundary)?;
    if rendered.text.len() > MAX_PROMPT_BYTES {
        return Err(ServiceError::InvalidBoundary);
    }
    Ok(RunPromptInput::New(PromptInput {
        text: rendered.text,
        attachments: Vec::new(),
        document_ids: Vec::new(),
        project_file_ids: Vec::new(),
    }))
}

async fn invoke_dynamic_slash_command(
    app: &mut App,
    invocation: &str,
) -> Result<SlashInvocationOutcome, ServiceError> {
    let (name, arguments) =
        slash_name_and_arguments(invocation).ok_or(ServiceError::InvalidBoundary)?;
    let extension_arguments = arguments
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match app
        .executable_extensions
        .execute_command_without_confirmation(name, extension_arguments)
        .await
    {
        Ok(Some(_)) => Ok(SlashInvocationOutcome::immediate(
            DriverCommandOutcome::default(),
        )),
        Ok(None) => start_prompt_template(app, name, arguments).map(SlashInvocationOutcome::Start),
        Err(_) => Err(ServiceError::InvalidBoundary),
    }
}

fn reconfiguration_outcome(
    app: &App,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    selection: ModelSelection,
    authority: AuthorityProfile,
) -> Result<DriverCommandOutcome, ServiceError> {
    let mut events = vec![event(EventPayload::SessionSettingsChanged {
        model: selection,
        authority,
    })];
    let branch_start = projection.known_entries;
    for item in project_new_entries(
        app.agent.session(),
        &plan.config.workspace,
        projection,
        None,
        None,
        plan.attachments.as_ref(),
        &plan.session_id,
    )? {
        events.push(event(EventPayload::ItemCommitted { item }));
    }
    events.extend(branch_delta_events(app.agent.session(), branch_start)?);
    Ok(DriverCommandOutcome::with_events(events))
}

fn persist_idle_selection(
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
    selection: ModelSelection,
) -> Result<DriverCommandOutcome, ServiceError> {
    let branch_start = projection.known_entries;
    let (path, session, newly_created) = match &plan.launch.session {
        SessionSelection::CreateNew(path) => (
            path.clone(),
            Session::create(path).map_err(|_| ServiceError::Internal)?,
            true,
        ),
        SessionSelection::OpenExisting(path) => (
            path.clone(),
            Session::open(path).map_err(|_| ServiceError::Internal)?,
            false,
        ),
    };
    let mut session = session;
    let append = session.append(EntryValue::Config {
        model: Some(plan.launch.model.0.clone()),
        reasoning: Some(reasoning_label(&plan.launch.reasoning)),
        reasoning_mode: Some(
            match plan.launch.reasoning_mode {
                octet_ai::ReasoningMode::Standard => "standard",
                octet_ai::ReasoningMode::Pro => "pro",
            }
            .to_owned(),
        ),
    });
    if append.is_err() {
        drop(session);
        if newly_created {
            let _ = std::fs::remove_file(&path);
        }
        return Err(ServiceError::Internal);
    }
    projection.known_entries = session.entries().len();
    plan.launch.session = SessionSelection::OpenExisting(path);
    let mut events = vec![event(EventPayload::SessionSettingsChanged {
        model: selection,
        authority: plan.authority,
    })];
    events.extend(branch_delta_events(&session, branch_start)?);
    Ok(DriverCommandOutcome::with_events(events))
}

fn rename_session_outcome(
    plan: &WorkerPlan,
    title: &str,
) -> Result<DriverCommandOutcome, ServiceError> {
    ensure_durable_session(plan)?;
    let metadata = plan
        .sessions
        .rename(plan.session_id.as_str(), title)
        .map_err(|_| ServiceError::InvalidBoundary)?;
    let title = metadata.name.ok_or(ServiceError::InvalidBoundary)?;
    if let Ok(mut search_index) = plan.search_index.lock() {
        let _ = search_index.update_session_title(plan.session_id.as_str(), &title);
    }
    Ok(session_metadata_outcome(Some(title), None, None))
}

fn pin_session_outcome(
    plan: &WorkerPlan,
    pinned: bool,
) -> Result<DriverCommandOutcome, ServiceError> {
    ensure_durable_session(plan)?;
    plan.sessions
        .set_pinned(plan.session_id.as_str(), pinned)
        .map_err(|_| ServiceError::Internal)?;
    Ok(session_metadata_outcome(None, Some(pinned), None))
}

fn archive_session_outcome(
    plan: &WorkerPlan,
    archived: bool,
) -> Result<DriverCommandOutcome, ServiceError> {
    ensure_durable_session(plan)?;
    plan.sessions
        .set_archived(plan.session_id.as_str(), archived)
        .map_err(|_| ServiceError::Internal)?;
    Ok(session_metadata_outcome(None, None, Some(archived)))
}

fn ensure_durable_session(plan: &WorkerPlan) -> Result<(), ServiceError> {
    match &plan.launch.session {
        SessionSelection::OpenExisting(path) if path.is_file() => Ok(()),
        SessionSelection::CreateNew(_) | SessionSelection::OpenExisting(_) => {
            Err(ServiceError::InvalidBoundary)
        }
    }
}

fn restore_session_head(path: &std::path::Path, head: EntryId) -> Result<(), ServiceError> {
    let mut session = Session::open(path).map_err(|_| ServiceError::Internal)?;
    session.checkout(head).map_err(|_| ServiceError::Internal)
}

fn checkout_before_user_entry(
    session: &mut Session,
    source_user_entry_id: &EntryId,
) -> Result<(), ServiceError> {
    let source = session
        .entry(source_user_entry_id)
        .ok_or(ServiceError::InvalidBoundary)?;
    if !is_user_authored_entry(source) {
        return Err(ServiceError::InvalidBoundary);
    }
    match source.parent.clone() {
        Some(parent) => session
            .checkout(parent)
            .map_err(|_| ServiceError::InvalidBoundary),
        None => session
            .checkout_root()
            .map_err(|_| ServiceError::InvalidBoundary),
    }
}

fn is_user_authored_entry(entry: &Entry) -> bool {
    matches!(
        &entry.value,
        EntryValue::Message(Message::User(message))
            if !message.content.is_empty()
                && message
                    .content
                    .iter()
                    .all(|part| matches!(part, UserPart::Text(_) | UserPart::Media(_)))
    )
}

fn retry_originating_user_entry(
    session: &Session,
    source_assistant_entry_id: &EntryId,
) -> Result<EntryId, ServiceError> {
    let assistant = session
        .entry(source_assistant_entry_id)
        .ok_or(ServiceError::InvalidBoundary)?;
    if !matches!(&assistant.value, EntryValue::Message(Message::Assistant(_))) {
        return Err(ServiceError::InvalidBoundary);
    }
    let mut cursor = assistant.parent.as_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id).ok_or(ServiceError::InvalidBoundary)?;
        if is_user_authored_entry(entry) {
            return Ok(entry.id.clone());
        }
        cursor = entry.parent.as_ref();
    }
    Err(ServiceError::InvalidBoundary)
}

fn replay_prompt_input(
    session: &Session,
    source_user_entry_id: &EntryId,
    plan: &WorkerPlan,
) -> Result<ResolvedPromptInput, ServiceError> {
    let entry = session
        .entry(source_user_entry_id)
        .ok_or(ServiceError::InvalidBoundary)?;
    let EntryValue::Message(Message::User(message)) = &entry.value else {
        return Err(ServiceError::InvalidBoundary);
    };
    if !is_user_authored_entry(entry) {
        return Err(ServiceError::InvalidBoundary);
    }
    let mut model_text = String::new();
    for part in &message.content {
        if let UserPart::Text(text) = part {
            model_text.push_str(text);
        }
    }
    let display_text = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.display_text.clone())
        .unwrap_or_else(|| model_text.clone());
    let attachments = if message
        .content
        .iter()
        .any(|part| matches!(part, UserPart::Media(_)))
    {
        let store = plan.attachments.as_ref().ok_or(ServiceError::Unavailable)?;
        store
            .refs_for_entry(&plan.session_id, &entry.id.0)
            .map_err(attachment_service_error)?
            .ok_or(ServiceError::InvalidBoundary)?
    } else {
        Vec::new()
    };
    let (documents, project_files) = stored_prompt_context_for_entry(
        session,
        plan.resources.as_ref(),
        &plan.session_id,
        &entry.id.0,
    );
    let document_context_tokens = documents
        .iter()
        .map(|document| document.extracted_text_byte_count)
        .fold(0_u64, u64::saturating_add)
        .div_ceil(4);
    let project_file_context_tokens = project_files
        .iter()
        .map(|file| file.byte_len)
        .fold(0_u64, u64::saturating_add)
        .div_ceil(4);
    Ok(ResolvedPromptInput {
        display_text,
        model_text,
        attachments,
        documents,
        project_files,
        document_context_tokens,
        project_file_context_tokens,
    })
}

fn stored_prompt_context_for_entry(
    session: &Session,
    resources: Option<&octet_serve_backend::ResourceStore>,
    session_id: &SessionId,
    durable_entry_id: &str,
) -> (Vec<DocumentReference>, Vec<TrustedFileEntry>) {
    let Some(resources) = resources else {
        return (Vec::new(), Vec::new());
    };
    for entry in session.entries().iter().rev() {
        if entry
            .metadata
            .as_ref()
            .is_none_or(|metadata| metadata.run_outcome.is_none())
        {
            continue;
        }
        let Ok(outcome_entry_id) = DurableEntryId::new(entry.id.0.clone()) else {
            continue;
        };
        let Some(record) = load_stored_run_record(resources, session_id, &outcome_entry_id) else {
            continue;
        };
        if let Some(item) = record
            .items
            .into_iter()
            .find(|item| item.durable_entry_id == durable_entry_id)
        {
            return (item.documents, item.project_files);
        }
    }
    (Vec::new(), Vec::new())
}

// These explicit actor-state and channel borrows document which branch owns
// each mutable subsystem; combining them into a broad context would weaken that boundary.
#[allow(clippy::too_many_arguments)]
async fn drive_sibling_conversation_branch(
    mut owned_app: App,
    source_user_entry_id: EntryId,
    input: RunPromptInput,
    provenance: ConversationBranchProvenance,
    model_override: Option<ModelSelection>,
    goal_driver: Option<&GoalDriver>,
    plan: &mut WorkerPlan,
    projection: &mut ProjectionState,
    commands: &mut mpsc::Receiver<WorkerMessage>,
    events: &mpsc::Sender<TimestampedEvent>,
    admission: oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>,
) -> Result<(App, bool, Option<GoalDecision>), ServiceError> {
    let path = owned_app.agent.session().path().to_owned();
    let previous_head = owned_app
        .agent
        .session()
        .head()
        .ok_or(ServiceError::InvalidBoundary)?;
    let (new_model, new_reasoning) = match model_override.as_ref() {
        Some(selection) => {
            let available = plan.available_models.iter().any(|model| {
                model.available
                    && model.provider == selection.provider
                    && model.id == selection.model
                    && model
                        .reasoning
                        .iter()
                        .any(|reasoning| reasoning == &selection.reasoning)
            });
            if !available {
                let _ = admission.send(Err(ServiceError::InvalidBoundary));
                return Ok((owned_app, false, None));
            }
            let model = match owned_app.catalog.resolve(&ModelId(selection.model.clone())) {
                Ok(model) => model,
                Err(_) => {
                    let _ = admission.send(Err(ServiceError::InvalidBoundary));
                    return Ok((owned_app, false, None));
                }
            };
            let reasoning = match config::parse_reasoning(&selection.reasoning) {
                Ok(reasoning) => reasoning,
                Err(_) => {
                    let _ = admission.send(Err(ServiceError::InvalidBoundary));
                    return Ok((owned_app, false, None));
                }
            };
            (Some(model), Some(reasoning))
        }
        None => (None, None),
    };
    if let Err(error) =
        checkout_before_user_entry(owned_app.agent.session_mut(), &source_user_entry_id)
    {
        let _ = admission.send(Err(error));
        return Ok((owned_app, false, None));
    }
    let selection = SessionSelection::OpenExisting(path.clone());
    let mut candidate = match rebuild_app(
        owned_app,
        new_model,
        new_reasoning,
        None,
        Some(selection.clone()),
    ) {
        Ok(candidate) => candidate,
        Err(_) => {
            let restored = restore_checkout_owner(&path, previous_head, plan)?;
            let _ = admission.send(Err(ServiceError::Internal));
            return Ok((restored, false, None));
        }
    };
    let previous_model = plan.launch.model.clone();
    let previous_reasoning = plan.launch.reasoning.clone();
    let previous_reasoning_mode = plan.launch.reasoning_mode;
    plan.launch.model = candidate.model.spec.id.clone();
    plan.launch.reasoning = candidate.reasoning.clone();
    plan.launch.reasoning_mode = candidate.reasoning_mode;
    plan.launch.session = selection;
    match start_and_drive_run(
        &mut candidate,
        input,
        Some(provenance),
        goal_driver,
        GoalTurnSource::User,
        plan,
        projection,
        commands,
        events,
        Some(admission),
    )
    .await
    {
        Ok(RunDriveOutcome::Admitted { goal }) => Ok((candidate, false, goal)),
        Ok(RunDriveOutcome::Rejected { admission, error }) => {
            plan.launch.model = previous_model;
            plan.launch.reasoning = previous_reasoning;
            plan.launch.reasoning_mode = previous_reasoning_mode;
            let restored = rollback_checkout_candidate(candidate, &path, previous_head, plan)?;
            if let Some(admission) = admission {
                let _ = admission.send(Err(error));
            }
            Ok((restored, false, None))
        }
        Err(_) => Ok((candidate, true, None)),
    }
}

fn create_conversation_fork(
    app: &App,
    sessions: &SessionStore,
    source_session_id: &SessionId,
    project_id: Option<&ProjectId>,
    projects: &Arc<Mutex<ProjectRegistry>>,
    source_entry_id: &DurableEntryId,
) -> Result<SessionId, ServiceError> {
    let source_entry = EntryId(source_entry_id.as_str().to_owned());
    let entry = app
        .agent
        .session()
        .entry(&source_entry)
        .ok_or(ServiceError::InvalidBoundary)?;
    if !matches!(
        &entry.value,
        EntryValue::Message(Message::User(_))
            | EntryValue::Message(Message::Assistant(_))
            | EntryValue::Compaction { .. }
    ) {
        return Err(ServiceError::InvalidBoundary);
    }
    let project_id = project_id
        .ok_or(ServiceError::InvalidBoundary)
        .and_then(registry_project_id)?;
    let destination = sessions.new_path(&crate::modes::timestamp());
    let created_session_id = session_id_from_path(&destination)?;
    let forked = app
        .agent
        .session()
        .fork_to(&destination, Some(&source_entry))
        .map_err(|_| ServiceError::Internal)?;
    drop(forked);
    if sessions
        .set_fork_provenance(
            created_session_id.as_str(),
            source_session_id.as_str(),
            source_entry_id.as_str(),
        )
        .is_err()
    {
        let _ = sessions.discard_unacknowledged(created_session_id.as_str());
        return Err(ServiceError::Internal);
    }
    let mut projects = projects.lock().map_err(|_| ServiceError::Internal)?;
    if let Err(error) = projects.bind_session(created_session_id.as_str(), &project_id) {
        drop(projects);
        let _ = sessions.discard_unacknowledged(created_session_id.as_str());
        return Err(project_registry_service_error(error));
    }
    Ok(created_session_id)
}

fn rollback_conversation_fork(
    plan: &WorkerPlan,
    created_session_id: &SessionId,
) -> Result<(), ServiceError> {
    let previous_project = {
        let mut projects = plan.projects.lock().map_err(|_| ServiceError::Internal)?;
        projects
            .unbind_session(created_session_id.as_str())
            .map_err(project_registry_service_error)?
    };
    if let Err(error) = plan
        .sessions
        .discard_unacknowledged(created_session_id.as_str())
    {
        if let Some(project_id) = previous_project {
            let mut projects = plan.projects.lock().map_err(|_| ServiceError::Internal)?;
            projects
                .bind_session(created_session_id.as_str(), &project_id)
                .map_err(project_registry_service_error)?;
        }
        let _ = error;
        return Err(ServiceError::Internal);
    }
    Ok(())
}

fn rollback_checkout_candidate(
    mut candidate: App,
    path: &Path,
    previous_head: EntryId,
    plan: &mut WorkerPlan,
) -> Result<App, ServiceError> {
    candidate.executable_extensions.shutdown_blocking();
    drop(candidate);
    restore_checkout_owner(path, previous_head, plan)
}

fn restore_checkout_owner(
    path: &Path,
    previous_head: EntryId,
    plan: &mut WorkerPlan,
) -> Result<App, ServiceError> {
    #[cfg(test)]
    if plan.checkout_hooks.fail_rollback {
        return Err(ServiceError::Internal);
    }
    restore_session_head(path, previous_head)?;
    build_worker_app(plan).map_err(|_| ServiceError::Internal)
}

#[cfg(test)]
async fn wait_for_checkout_rollback_gate(plan: &WorkerPlan) {
    if let Some(gate) = &plan.checkout_hooks.rollback_gate {
        gate.entered.wait().await;
        gate.release.wait().await;
    }
}

#[cfg(not(test))]
async fn wait_for_checkout_rollback_gate(_plan: &WorkerPlan) {}

fn checkout_rejection_after_rollback<T>(
    rollback: Result<T, ServiceError>,
    rejection: ServiceError,
) -> Result<(T, ServiceError), ServiceError> {
    rollback
        .map(|owner| (owner, rejection))
        .map_err(|_| ServiceError::OwnerLost)
}

fn session_metadata_outcome(
    title: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
) -> DriverCommandOutcome {
    DriverCommandOutcome::with_events(vec![event(EventPayload::SessionMetadataChanged {
        title,
        pinned,
        archived,
    })])
}

async fn shutdown_worker_app(app: &mut Option<App>) {
    if let Some(mut app) = app.take() {
        app.executable_extensions.shutdown().await;
    }
}

fn serve_runtime_manager(plan: &WorkerPlan) -> anyhow::Result<ExtensionRuntimeManager> {
    // Serve never reuses the ordinary-host partition. Hashing both stable
    // project identity and the finite authority profile provides an explicit,
    // path-free trust partition while the runtime domain independently binds
    // the canonical workspace.
    let project = plan
        .project_id
        .as_ref()
        .map(|project| project.as_str())
        .unwrap_or("unbound");
    let project_digest = format!("{:x}", Sha256::digest(project.as_bytes()));
    let authority = format!("{:?}", plan.authority);
    let authority_digest = format!("{:x}", Sha256::digest(authority.as_bytes()));
    let trust = ExtensionTrustDomain::new(format!(
        "serve-{}-{}",
        &project_digest[..32],
        &authority_digest[..32]
    ))
    .map_err(anyhow::Error::msg)?;
    let domain =
        ExtensionRuntimeDomain::serve(&plan.config.workspace, trust).map_err(anyhow::Error::msg)?;
    Ok(ExtensionRuntimeManager::new(domain))
}

fn build_worker_app(plan: &mut WorkerPlan) -> anyhow::Result<App> {
    anyhow::ensure!(
        plan.authority == authority_ceiling_from_sandbox(&plan.config.sandbox),
        "Serve session authority must match the immutable host policy"
    );
    let mut config = plan.config.clone();
    config.resume = match &plan.launch.session {
        SessionSelection::CreateNew(_) => crate::config::ResumeSelector::New,
        SessionSelection::OpenExisting(path) => crate::config::ResumeSelector::Resume(
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned),
        ),
    };
    let mut boot = crate::app::bootstrap::bootstrap(config)?;
    let system = compose_instructions(&boot.config)?;
    if let Some(session) = plan
        .prepared_session
        .get_mut()
        .map_err(|_| anyhow::anyhow!("prepared session lock poisoned"))?
        .take()
    {
        boot.set_prepared_session(session);
    }
    let app = build_app_with_runtime_manager(
        boot,
        plan.launch.clone(),
        system,
        Some(serve_runtime_manager(plan)?),
    )?;
    Ok(app)
}

fn command_name_is_claimed_by_builtin(name: &str) -> bool {
    !matches!(
        commands::parse(&format!("/{name}")),
        commands::Command::Unknown(_)
    )
}

fn extension_command_presentation(
    name: &str,
    declared_usage: Option<String>,
) -> (String, Option<String>) {
    let default_usage = format!("/{name}");
    let Some(declared_usage) = declared_usage else {
        return (default_usage, None);
    };
    let usage = octet_serve_backend::sanitize_public_text(declared_usage.trim(), 512, false);
    let Some(suffix) = usage.strip_prefix(&default_usage) else {
        return (default_usage, None);
    };
    if !suffix.is_empty()
        && !matches!(suffix.chars().next(), Some(character) if character.is_whitespace())
    {
        return (default_usage, None);
    }
    let argument_hint = suffix.trim();
    let argument_hint = (!argument_hint.is_empty()).then(|| argument_hint.to_owned());
    (usage, argument_hint)
}

fn build_command_discovery(app: &App) -> Result<CommandDiscovery, ServiceError> {
    const MAX_SUGGESTIONS: usize = 512;

    let mut commands = Vec::new();
    let mut command_names = BTreeSet::new();
    let mut push_command = |suggestion: CommandSuggestion| {
        if commands.len() >= MAX_SUGGESTIONS || !command_names.insert(suggestion.name.clone()) {
            return;
        }
        if suggestion.validate().is_ok() {
            commands.push(suggestion);
        } else {
            command_names.remove(&suggestion.name);
        }
    };

    for command in commands::slash_commands() {
        push_command(CommandSuggestion {
            name: command.name.to_owned(),
            usage: command.usage.to_owned(),
            description: command.description.to_owned(),
            argument_hint: None,
            accepts_argument: command.accepts_argument,
            kind: CommandSuggestionKind::BuiltIn,
        });
    }
    let extension_commands = app.executable_extensions.command_suggestions_with_usage();
    let extension_command_names = extension_commands
        .iter()
        .map(|(name, _, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    for template in app.prompts.descriptors().iter() {
        // `commands::parse` accepts unambiguous built-in prefixes. A dynamic
        // name claimed that way would execute the built-in instead.
        if command_name_is_claimed_by_builtin(&template.name) {
            continue;
        }
        // Dynamic dispatch gives executable extensions precedence over prompt
        // templates. Do not advertise a colliding template that would invoke
        // an extension instead.
        if extension_command_names.contains(template.name.as_str()) {
            continue;
        }
        push_command(CommandSuggestion {
            name: template.name.clone(),
            usage: format!("/{}", template.name),
            description: format!("prompt · {}", template.description),
            argument_hint: template.argument_hint.clone(),
            accepts_argument: true,
            kind: CommandSuggestionKind::Prompt,
        });
    }
    for (name, description, declared_usage) in extension_commands {
        if command_name_is_claimed_by_builtin(&name) {
            continue;
        }
        let (usage, argument_hint) = extension_command_presentation(&name, declared_usage);
        push_command(CommandSuggestion {
            usage,
            name,
            description: format!("extension · {description}"),
            argument_hint,
            accepts_argument: true,
            kind: CommandSuggestionKind::Extension,
        });
    }

    let active_skill_ids = app
        .agent
        .session()
        .head_ref()
        .and_then(|head| app.agent.session().resolve_active_skills(head).ok())
        .map(|state| {
            state
                .active_skills
                .into_iter()
                .map(|skill| skill.descriptor.id)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut skill_ids = BTreeSet::new();
    let mut skills = Vec::new();
    for descriptor in app.skills.descriptors().iter() {
        if skills.len() >= MAX_SUGGESTIONS || !skill_ids.insert(descriptor.id.clone()) {
            continue;
        }
        let suggestion = SkillSuggestion {
            id: descriptor.id.clone(),
            name: descriptor.name.clone(),
            description: descriptor.description.clone(),
            active: active_skill_ids.contains(&descriptor.id),
        };
        if suggestion.validate().is_ok() {
            skills.push(suggestion);
        } else {
            skill_ids.remove(&descriptor.id);
        }
    }

    let mut discovery = CommandDiscovery {
        protocol: PROTOCOL_VERSION,
        commands,
        skills,
    };
    trim_command_discovery_to_transport_bounds(&mut discovery);
    discovery.validate().map_err(|_| ServiceError::Internal)?;
    Ok(discovery)
}

fn trim_command_discovery_to_transport_bounds(discovery: &mut CommandDiscovery) {
    while discovery.validate().is_err() {
        if discovery.skills.pop().is_some() || discovery.commands.pop().is_some() {
            continue;
        }
        break;
    }
}

fn resolve_attachment_media(
    app: &App,
    plan: &WorkerPlan,
    references: &[AttachmentRef],
) -> Result<Vec<Media>, ServiceError> {
    let supports_images = app
        .model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Image);
    resolve_stored_media(supports_images, plan.attachments.as_ref(), references)
}

fn resolve_stored_media(
    supports_images: bool,
    store: Option<&AttachmentStore>,
    references: &[AttachmentRef],
) -> Result<Vec<Media>, ServiceError> {
    if references.is_empty() {
        return Ok(Vec::new());
    }
    if !supports_images {
        return Err(ServiceError::InvalidBoundary);
    }
    let store = store.ok_or(ServiceError::Unavailable)?;
    let resolved = store
        .resolve_many(references)
        .map_err(attachment_service_error)?;
    resolved
        .into_iter()
        .map(|attachment| {
            let media_type = attachment
                .reference
                .media_type
                .parse()
                .map_err(|_| ServiceError::InvalidBoundary)?;
            Ok(Media::image_bytes(attachment.bytes, media_type))
        })
        .collect()
}

fn token_hint_for_bytes(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

fn project_instruction_token_hint(system: &str) -> u64 {
    const START: &str = "<project_context>\n";
    const END: &str = "\n</project_context>";

    let Some(start) = system.find(START) else {
        return 0;
    };
    let section_start = start.saturating_add(START.len());
    let Some(relative_end) = system[section_start..].find(END) else {
        return 0;
    };
    token_hint_for_bytes(relative_end)
}

async fn resolve_prompt_input(
    plan: &WorkerPlan,
    input: PromptInput,
) -> Result<ResolvedPromptInput, ServiceError> {
    let PromptInput {
        text,
        attachments,
        document_ids,
        project_file_ids,
    } = input;
    let project_id = plan.project_id.as_ref();
    let document_context = if document_ids.is_empty() {
        None
    } else {
        let project_id = project_id.ok_or(ServiceError::Unauthorized)?.clone();
        let session_id = plan.session_id.clone();
        let store = plan.documents.clone().ok_or(ServiceError::Unavailable)?;
        Some(
            tokio::task::spawn_blocking(move || {
                store.prompt_context(project_id.as_str(), session_id.as_str(), &document_ids)
            })
            .await
            .map_err(|_| ServiceError::Internal)?
            .map_err(document_store_service_error)?,
        )
    };
    let project_file_context = if project_file_ids.is_empty() {
        None
    } else {
        let project_id = project_id.ok_or(ServiceError::Unauthorized)?.clone();
        let projects = Arc::clone(&plan.projects);
        let trusted_files = Arc::clone(&plan.trusted_files);
        Some(
            tokio::task::spawn_blocking(move || {
                with_trusted_project_files(
                    &projects,
                    &trusted_files,
                    &project_id,
                    |service, registry| service.attach_as_context(registry, &project_file_ids),
                )
            })
            .await
            .map_err(|_| ServiceError::Internal)??,
        )
    };
    let composed = octet_serve_backend::compose_prompt_text(
        &text,
        document_context
            .as_ref()
            .map(|context| context.text.as_str()),
        project_file_context
            .as_ref()
            .map(|context| context.text.as_str()),
    )
    .map_err(|error| match error {
        octet_serve_backend::PromptContextError::InvalidUserText
        | octet_serve_backend::PromptContextError::InvalidDocumentContext
        | octet_serve_backend::PromptContextError::InvalidProjectFileContext => {
            ServiceError::InvalidBoundary
        }
        octet_serve_backend::PromptContextError::DocumentContextTooLarge
        | octet_serve_backend::PromptContextError::ProjectFileContextTooLarge
        | octet_serve_backend::PromptContextError::AuxiliaryContextTooLarge
        | octet_serve_backend::PromptContextError::PromptTooLarge => ServiceError::PayloadTooLarge,
    })?;
    let document_context_tokens = token_hint_for_bytes(composed.document_context_bytes());
    let project_file_context_tokens = token_hint_for_bytes(composed.project_file_context_bytes());
    Ok(ResolvedPromptInput {
        display_text: text,
        model_text: composed.into_string(),
        attachments,
        documents: document_context
            .map(|context| context.documents)
            .unwrap_or_default(),
        project_files: project_file_context
            .map(|context| context.files)
            .unwrap_or_default(),
        document_context_tokens,
        project_file_context_tokens,
    })
}

fn resolve_control_input(
    plan: &WorkerPlan,
    text: String,
    references: &[AttachmentRef],
) -> Result<UserInput, ServiceError> {
    let mut parts = Vec::with_capacity(1 + references.len());
    if !text.is_empty() {
        parts.push(InputPart::Text(text));
    }
    let supports_images = plan
        .available_models
        .iter()
        .find(|summary| summary.id == plan.launch.model.0)
        .is_some_and(|summary| summary.input_modalities.contains(&InputModality::Image));
    parts.extend(
        resolve_stored_media(supports_images, plan.attachments.as_ref(), references)?
            .into_iter()
            .map(InputPart::Media),
    );
    Ok(UserInput::from(parts))
}

fn attachment_service_error(error: AttachmentError) -> ServiceError {
    match error {
        AttachmentError::Unavailable | AttachmentError::QuotaExceeded => ServiceError::Unavailable,
        AttachmentError::Storage => ServiceError::Internal,
        AttachmentError::InvalidName
        | AttachmentError::UnsupportedMediaType
        | AttachmentError::InvalidContent
        | AttachmentError::TooLarge
        | AttachmentError::NotFound
        | AttachmentError::MetadataMismatch => ServiceError::InvalidBoundary,
    }
}

fn resource_store_service_error(error: octet_serve_backend::ResourceStoreError) -> ServiceError {
    match error {
        octet_serve_backend::ResourceStoreError::InvalidBoundary => ServiceError::InvalidBoundary,
        octet_serve_backend::ResourceStoreError::QuotaExceeded => ServiceError::Unavailable,
        octet_serve_backend::ResourceStoreError::NotFound => ServiceError::NotFound,
        octet_serve_backend::ResourceStoreError::Corrupt => ServiceError::CorruptResource,
        octet_serve_backend::ResourceStoreError::Storage => ServiceError::Internal,
    }
}

// A run nests the agent's provider stream under the long-lived Serve worker.
// Keep the full run state machine on the heap, rather than adding its poll frame
// to the worker's stack on every prompt.
#[allow(clippy::too_many_arguments)]
fn start_and_drive_run<'a>(
    app: &'a mut App,
    input: RunPromptInput,
    branch_provenance: Option<ConversationBranchProvenance>,
    goal_driver: Option<&'a GoalDriver>,
    goal_source: GoalTurnSource,
    plan: &'a WorkerPlan,
    projection: &'a mut ProjectionState,
    commands: &'a mut mpsc::Receiver<WorkerMessage>,
    events: &'a mpsc::Sender<TimestampedEvent>,
    admission: Option<oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<RunDriveOutcome, ServiceError>> + Send + 'a>,
> {
    Box::pin(start_and_drive_run_inner(
        app,
        input,
        branch_provenance,
        goal_driver,
        goal_source,
        plan,
        projection,
        commands,
        events,
        admission,
    ))
}

// Run orchestration keeps its independently borrowed actor state and channels
// visible rather than hiding them behind a mutable catch-all context.
#[allow(clippy::too_many_arguments)]
async fn start_and_drive_run_inner(
    app: &mut App,
    input: RunPromptInput,
    branch_provenance: Option<ConversationBranchProvenance>,
    goal_driver: Option<&GoalDriver>,
    goal_source: GoalTurnSource,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    commands: &mut mpsc::Receiver<WorkerMessage>,
    events: &mpsc::Sender<TimestampedEvent>,
    admission: Option<oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>>,
) -> Result<RunDriveOutcome, ServiceError> {
    if let Some(limit) = app.config.max_cost_microdollars {
        if app.agent.session().total_cost_microdollars() >= limit {
            return Ok(RunDriveOutcome::Rejected {
                admission,
                error: ServiceError::InvalidBoundary,
            });
        }
    }
    let (resolved, replay_exact) = match input {
        RunPromptInput::New(input) => match resolve_prompt_input(plan, input).await {
            Ok(resolved) => (resolved, false),
            Err(error) => {
                return Ok(RunDriveOutcome::Rejected { admission, error });
            }
        },
        RunPromptInput::Replay(resolved) => (resolved, true),
    };
    let ResolvedPromptInput {
        display_text,
        model_text,
        attachments,
        documents,
        project_files,
        document_context_tokens,
        project_file_context_tokens,
    } = resolved;
    let media = match resolve_attachment_media(app, plan, &attachments) {
        Ok(media) => media,
        Err(error) => {
            return Ok(RunDriveOutcome::Rejected { admission, error });
        }
    };
    let prompt = if replay_exact {
        model_text
    } else {
        match crate::prompts::render_configured(app, &model_text) {
            Err(_) => {
                return Ok(RunDriveOutcome::Rejected {
                    admission,
                    error: ServiceError::Internal,
                });
            }
            Ok(Some(rendered)) => rendered.text,
            Ok(None) => model_text,
        }
    };
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    let command_discovery = match build_command_discovery(app) {
        Ok(discovery) => discovery,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    let (pending_context_count, model_prompt, project_instruction_tokens) = if replay_exact {
        let project_instruction_tokens = project_instruction_token_hint(&app.system);
        app.agent.set_system_prompt(app.system.clone());
        (0, prompt, project_instruction_tokens)
    } else {
        let composition = match app
            .executable_extensions
            .compose_prompt(&app.system, prompt.clone())
            .await
        {
            Ok(composition) => composition,
            Err(_) => {
                return Ok(RunDriveOutcome::Rejected {
                    admission,
                    error: ServiceError::Internal,
                });
            }
        };
        let pending_context_count = composition.pending_context_count;
        let model_prompt = composition.prompt;
        let project_instruction_tokens = project_instruction_token_hint(&composition.system);
        app.agent.set_system_prompt(composition.system);
        (
            pending_context_count,
            model_prompt,
            project_instruction_tokens,
        )
    };
    app.agent
        .set_prompt_display_text(Some(display_text.clone()));
    projection.begin_run();
    let run_id = match projection.next_run_id(plan.actor_generation) {
        Ok(run_id) => run_id,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    let turn_id = match projection.turn_id(&run_id) {
        Ok(turn_id) => turn_id,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    let user_item_id = match projection.provisional_id(&run_id, "user", 0) {
        Ok(item_id) => item_id,
        Err(error) => return Ok(RunDriveOutcome::Rejected { admission, error }),
    };
    projection
        .item_turns
        .insert(user_item_id.clone(), turn_id.clone());
    let mut input_parts = Vec::with_capacity(1 + media.len());
    if !model_prompt.is_empty() {
        input_parts.push(InputPart::Text(model_prompt));
    }
    input_parts.extend(media.into_iter().map(InputPart::Media));
    let title_before_prompt =
        session_meta_for_open_session(&plan.sessions, &plan.session_id, app.agent.session())
            .map(|metadata| metadata.title);
    projection.usage_uncertain |= app.agent.session().has_uncertain_usage();
    let run_model = app.model.clone();
    let mut run = match app.agent.prompt(UserInput::from(input_parts)).await {
        Ok(run) => run,
        Err(_) => {
            return Ok(RunDriveOutcome::Rejected {
                admission,
                error: ServiceError::Internal,
            });
        }
    };
    let extension_turn = app.executable_extensions.begin_turn().await;
    let mut context_projection = RunContextProjection::new(
        project_instruction_tokens,
        document_context_tokens,
        project_file_context_tokens,
    );
    context_projection.usage_uncertain = projection.usage_uncertain;
    if !attachments.is_empty() {
        projection
            .pending_attachments
            .push_back(attachments.clone());
    }
    projection.pending_user_items.push_back(PendingUserItem {
        id: user_item_id.clone(),
        delivery: UserMessageDelivery::Submit,
        turn_id: turn_id.clone(),
        documents: documents.clone(),
        project_files: project_files.clone(),
        document_context_tokens,
        project_file_context_tokens,
        context_attributed: true,
        branch_provenance: branch_provenance.clone(),
    });
    app.executable_extensions
        .commit_prompt_context(pending_context_count);
    let control = run.control();
    let mut immediate = Vec::with_capacity(3);
    let title_after_prompt = title_before_prompt.clone().or_else(|| {
        plan.sessions
            .load_metadata(plan.session_id.as_str())
            .ok()
            .and_then(|metadata| metadata.name)
            .or_else(|| {
                let title = crate::session_store::trim_title(&display_text);
                (!title.trim().is_empty()).then_some(title)
            })
    });
    if let Some(title) = title_after_prompt.filter(|title| {
        title != "(empty session)"
            && !title.trim().is_empty()
            && title_before_prompt.as_deref() != Some(title.as_str())
    }) {
        immediate.push(event(EventPayload::SessionMetadataChanged {
            title: Some(title),
            pinned: None,
            archived: None,
        }));
    }
    immediate.extend([
        event(EventPayload::ItemStarted {
            item: SessionItem {
                id: user_item_id.clone(),
                run_id: Some(run_id.clone()),
                turn_id: Some(turn_id),
                provider_attempt: None,
                lifecycle: ItemLifecycle::Provisional,
                durable_entry_id: None,
                payload: ItemPayload::UserMessage {
                    text: bounded_text(&display_text, MAX_PROMPT_BYTES),
                    attachments: attachments.clone(),
                    documents,
                    project_files,
                    delivery: Some(UserMessageDelivery::Submit),
                    branch_provenance,
                },
            },
        }),
        event(EventPayload::SessionStateChanged {
            state: SessionLiveState::Working,
            active_run_id: Some(run_id.clone()),
        }),
    ]);
    if let Some(admission) = admission {
        if admission
            .send(Ok(DriverCommandOutcome::run(run_id.clone(), immediate)))
            .is_err()
        {
            control.abort();
        }
    }
    if plan.config.sandbox.process_execution_allowed() {
        plan.pull_request_discovery_enabled
            .store(true, Ordering::Release);
        plan.pull_request_refresh_requested.notify_one();
    }

    let mut response_text = String::new();
    let mut extension_refresh = tokio::time::interval(std::time::Duration::from_millis(250));
    extension_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome;
    loop {
        tokio::select! {
            _ = extension_refresh.tick() => {
                publish_extension_presentations(
                    &mut app.executable_extensions,
                    projection,
                    events,
                ).await?;
            }
            event = run.next() => {
                let Some(agent_event) = event else {
                    outcome = HostRunOutcome::stream_lost();
                    break;
                };
                let projected_outcome = project_agent_event(
                    agent_event,
                    &run_id,
                    plan,
                    &run_model,
                    projection,
                    &mut context_projection,
                    events,
                    &mut response_text,
                )
                .await?;
                publish_context_snapshot(
                    run.context_snapshot(),
                    &run_id,
                    &mut context_projection,
                    events,
                )
                .await?;
                projection.last_context = context_projection.last_published.clone();
                if let Some(projected_outcome) = projected_outcome {
                    outcome = projected_outcome;
                    break;
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    control.abort();
                    outcome = HostRunOutcome::shutdown();
                    break;
                };
                match command {
                    WorkerMessage::Command(command) => {
                        handle_active_command(
                            command,
                            &run_id,
                            &control,
                            plan,
                            projection,
                            events,
                        )
                        .await;
                    }
                    WorkerMessage::CommandDiscovery { response } => {
                        let _ = response.send(Ok(command_discovery.clone()));
                    }
                }
            }
        }
    }
    let final_context_snapshot = run.into_context_snapshot();
    app.executable_extensions
        .settle_turn(extension_turn, &outcome)
        .await;
    publish_extension_presentations(&mut app.executable_extensions, projection, events).await?;
    publish_context_snapshot(
        final_context_snapshot,
        &run_id,
        &mut context_projection,
        events,
    )
    .await?;
    projection.last_context = context_projection.last_published.clone();
    let completed = outcome.allows_after_response();
    let terminal = TerminalProjection::from_host_outcome(&outcome);
    if let Err(error) = sync_session_usage(&plan.usage, &plan.session_id, app.agent.session()) {
        projection.usage_uncertain = true;
        publish_idle_accounting_context(projection, events).await?;
        return Err(error);
    }
    let settled_at_ms = now_ms();
    let unfinished = projection
        .tool_calls
        .iter()
        .filter(|(_, tool)| tool.activity.status == ToolActivityStatus::Running)
        .map(|(tool_call_id, _)| tool_call_id.clone())
        .collect::<Vec<_>>();
    let mut stopped_updates = Vec::new();
    for tool_call_id in unfinished {
        let Some(item_id) = projection.tool_items.get(&tool_call_id).cloned() else {
            continue;
        };
        let progress = projection
            .tool_progress
            .remove(&tool_call_id)
            .unwrap_or_default();
        let Some(tool) = projection.tool_calls.get_mut(&tool_call_id) else {
            continue;
        };
        tool.activity.status = ToolActivityStatus::Stopped;
        tool.activity.summary = Some("Stopped".into());
        tool.activity.completed_at_ms = Some(settled_at_ms.max(tool.activity.started_at_ms));
        tool.activity.duration_ms = Some(settled_at_ms.saturating_sub(tool.activity.started_at_ms));
        tool.activity.output_summary = Some("Tool stopped before completion".into());
        tool.activity.observed_output_bytes = progress.observed_output_bytes;
        tool.activity.dropped_output_bytes = progress.dropped_output_bytes;
        tool.result = Some(ToolResultSummary {
            tool_call_item_id: item_id.clone(),
            status: ToolActivityStatus::Stopped,
            summary: "Stopped".into(),
            output_summary: tool.activity.output_summary.clone(),
            output_handle: None,
            exit_code: None,
            signal: None,
            completed_at_ms: tool.activity.completed_at_ms.unwrap_or(settled_at_ms),
            duration_ms: tool.activity.duration_ms.unwrap_or_default(),
            observed_output_bytes: tool.activity.observed_output_bytes,
            dropped_output_bytes: tool.activity.dropped_output_bytes,
        });
        stopped_updates.push((item_id, tool.activity.clone()));
    }
    for (item_id, activity) in stopped_updates {
        events
            .send(event(EventPayload::ItemDelta {
                item_id,
                delta: ItemDelta::ToolActivity { activity },
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    let mut changed_file_item_ids = BTreeSet::new();
    let mut source_ids = BTreeSet::new();
    let mut output_ids = BTreeSet::new();
    if let Some(resources) = plan.resources.as_ref() {
        let completed = std::mem::take(&mut projection.pending_tool_evidence);
        for completed in completed {
            let payloads = project_tool_evidence(
                app.agent.session(),
                &plan.config.workspace,
                resources,
                &plan.session_id,
                &run_id,
                &completed.turn_id,
                &completed.tool_call_id,
                &completed.tool_item_id,
                &completed.tool,
                &completed.output,
            );
            let mut changed_paths = BTreeSet::new();
            let mut linked_sources = BTreeSet::new();
            let mut linked_outputs = BTreeSet::new();
            for payload in &payloads {
                match payload {
                    EventPayload::SourceUpserted { source } => {
                        source_ids.insert(source.id.clone());
                        linked_sources.insert(source.id.clone());
                    }
                    EventPayload::ArtifactUpserted { artifact } => {
                        output_ids.insert(artifact.id.clone());
                        linked_outputs.insert(artifact.id.clone());
                    }
                    EventPayload::ItemCommitted { item } => match &item.payload {
                        ItemPayload::FileChange(change) => {
                            changed_file_item_ids.insert(item.id.clone());
                            changed_paths.insert(change.display_path.clone());
                        }
                        ItemPayload::Source(source) => {
                            source_ids.insert(source.id.clone());
                            linked_sources.insert(source.id.clone());
                        }
                        ItemPayload::Artifact(artifact) => {
                            output_ids.insert(artifact.id.clone());
                            linked_outputs.insert(artifact.id.clone());
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            if let Some(tool) = projection.tool_calls.get_mut(&completed.tool_call_id) {
                tool.activity.changed_paths = changed_paths.into_iter().collect();
                tool.activity.source_ids = linked_sources.into_iter().collect();
                tool.activity.artifact_ids = linked_outputs.into_iter().collect();
                events
                    .send(event(EventPayload::ItemDelta {
                        item_id: completed.tool_item_id.clone(),
                        delta: ItemDelta::ToolActivity {
                            activity: tool.activity.clone(),
                        },
                    }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            for payload in payloads {
                events
                    .send(event(payload))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
        }
    } else {
        projection.pending_tool_evidence.clear();
    }
    let review = build_completion_review(
        &terminal,
        projection.run_started_at_ms,
        settled_at_ms,
        projection,
        changed_file_item_ids,
        source_ids,
        output_ids,
    );
    app.agent.set_system_prompt(app.system.clone());
    app.agent
        .record_run_outcome(SessionRunOutcome {
            status: match terminal.outcome {
                octet_serve_backend::RunOutcome::Completed => SessionRunOutcomeStatus::Completed,
                octet_serve_backend::RunOutcome::Stopped => SessionRunOutcomeStatus::Stopped,
                octet_serve_backend::RunOutcome::Failed => SessionRunOutcomeStatus::Failed,
            },
            message: terminal.message.clone(),
        })
        .map_err(|_| ServiceError::Internal)?;

    let branch_start = projection.known_entries;
    let committed = project_new_entries(
        app.agent.session(),
        &plan.config.workspace,
        projection,
        Some(&run_id),
        Some(&review),
        plan.attachments.as_ref(),
        &plan.session_id,
    )?;
    let search_title =
        session_meta_for_open_session(&plan.sessions, &plan.session_id, app.agent.session())
            .map(|meta| meta.name.unwrap_or(meta.title))
            .unwrap_or_else(|| "Session".to_owned());
    if let Ok(mut search_index) = plan.search_index.lock() {
        for item in &committed {
            if let Some(document) =
                search_document_for_item(&plan.session_id, &search_title, settled_at_ms, item)
            {
                let _ = search_index.upsert_document(document);
            }
        }
    }
    if let Some(resources) = plan.resources.as_ref() {
        persist_run_projection(
            resources,
            &plan.session_id,
            &run_id,
            projection.run_started_at_ms,
            settled_at_ms,
            projection,
            &committed,
            &review,
        )?;
    }
    for item in committed {
        events
            .send(event(EventPayload::ItemCommitted { item }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    for pending in projection.pending_user_items.drain(..) {
        events
            .send(event(EventPayload::ItemRetracted {
                item_id: pending.id,
                provider_attempt: 1,
                reason: "Input was not delivered before the run ended.".into(),
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    projection.pending_attachments.clear();
    for branch_event in branch_delta_events(app.agent.session(), branch_start)? {
        events
            .send(branch_event)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    expire_private_requests(projection, events, plan.actor_generation).await?;
    events
        .send(event(EventPayload::SessionStateChanged {
            state: terminal.state,
            active_run_id: None,
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    if completed {
        let _ = app
            .executable_extensions
            .after_response(&response_text)
            .await;
        publish_extension_presentations(&mut app.executable_extensions, projection, events).await?;
    }
    let goal = match goal_driver {
        Some(driver) if completed => match driver.turn_settled(
            goal_source,
            &response_text,
            !projection.tool_calls.is_empty(),
        ) {
            Ok(goal) => Some(goal),
            Err(_) => {
                let _ = driver.session_error();
                None
            }
        },
        Some(driver) => {
            let _ = driver.session_error();
            None
        }
        None => None,
    };
    if goal_driver.is_some() {
        events
            .send(current_goal_event(
                plan.goal_store.as_ref(),
                &plan.session_id,
            )?)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    Ok(RunDriveOutcome::Admitted { goal })
}

async fn handle_active_command(
    message: WorkerCommand,
    run_id: &RunId,
    control: &RunControl,
    plan: &WorkerPlan,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) {
    let outcome = match message.command {
        SessionCommand::Steer { input } => match resolve_prompt_input(plan, input).await {
            Ok(resolved) => match resolve_control_input(
                plan,
                resolved.model_text.clone(),
                &resolved.attachments,
            ) {
                Ok(input) => match control.steer(input).await {
                    Ok(()) => {
                        publish_control_user_item(
                            run_id,
                            resolved,
                            UserMessageDelivery::Steer,
                            projection,
                            events,
                        )
                        .await
                    }
                    Err(_) => Err(ServiceError::InvalidBoundary),
                },
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        SessionCommand::FollowUp { input } => match resolve_prompt_input(plan, input).await {
            Ok(resolved) => match resolve_control_input(
                plan,
                resolved.model_text.clone(),
                &resolved.attachments,
            ) {
                Ok(input) => match control.follow_up(input).await {
                    Ok(()) => {
                        publish_control_user_item(
                            run_id,
                            resolved,
                            UserMessageDelivery::FollowUp,
                            projection,
                            events,
                        )
                        .await
                    }
                    Err(_) => Err(ServiceError::InvalidBoundary),
                },
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        command @ (SessionCommand::SetGoal { .. }
        | SessionCommand::PauseGoal
        | SessionCommand::ResumeGoal
        | SessionCommand::ClearGoal) => goal_mutation_outcome(plan, command),
        SessionCommand::Rename { title } => rename_session_outcome(plan, &title),
        SessionCommand::SetPinned { pinned } => pin_session_outcome(plan, pinned),
        SessionCommand::SetArchived { archived } => archive_session_outcome(plan, archived),
        SessionCommand::Abort { run_id: expected }
            if expected.as_ref().is_none_or(|expected| expected == run_id) =>
        {
            control.abort();
            Ok(DriverCommandOutcome::default())
        }
        SessionCommand::AnswerRequest { request_id, answer } => {
            match projection.private_requests.remove(&request_id) {
                Some(PrivateRequest {
                    kind,
                    response: PrivateResponse::Approval(respond),
                }) => {
                    let (allowed, state) = match answer {
                        RequestAnswer::Approval { allowed } => (
                            allowed,
                            if allowed {
                                RequestState::Resolved
                            } else {
                                RequestState::Denied
                            },
                        ),
                        _ => {
                            projection.private_requests.insert(
                                request_id,
                                PrivateRequest {
                                    kind,
                                    response: PrivateResponse::Approval(respond),
                                },
                            );
                            let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                            return;
                        }
                    };
                    respond(allowed);
                    let changed = PendingRequest {
                        id: request_id,
                        actor_generation: projection_actor_generation(run_id),
                        kind,
                        state,
                    };
                    let _ = events
                        .send(event(EventPayload::PendingRequestChanged {
                            request: changed,
                        }))
                        .await;
                    let _ = events
                        .send(event(EventPayload::SessionStateChanged {
                            state: SessionLiveState::Working,
                            active_run_id: Some(run_id.clone()),
                        }))
                        .await;
                    Ok(DriverCommandOutcome::default())
                }
                Some(PrivateRequest {
                    kind,
                    response: PrivateResponse::Input(respond),
                }) => {
                    let answer = match answer {
                        RequestAnswer::Text { text } => text,
                        RequestAnswer::Choice { choice } => choice,
                        _ => {
                            projection.private_requests.insert(
                                request_id,
                                PrivateRequest {
                                    kind,
                                    response: PrivateResponse::Input(respond),
                                },
                            );
                            let _ = message.response.send(Err(ServiceError::InvalidBoundary));
                            return;
                        }
                    };
                    respond(Some(answer.into_bytes()));
                    let changed = PendingRequest {
                        id: request_id,
                        actor_generation: projection_actor_generation(run_id),
                        kind,
                        state: RequestState::Resolved,
                    };
                    let _ = events
                        .send(event(EventPayload::PendingRequestChanged {
                            request: changed,
                        }))
                        .await;
                    let _ = events
                        .send(event(EventPayload::SessionStateChanged {
                            state: SessionLiveState::Working,
                            active_run_id: Some(run_id.clone()),
                        }))
                        .await;
                    Ok(DriverCommandOutcome::default())
                }
                None => Err(ServiceError::InvalidBoundary),
            }
        }
        _ => Err(ServiceError::InvalidBoundary),
    };
    let _ = message.response.send(outcome);
}

async fn publish_control_user_item(
    run_id: &RunId,
    resolved: ResolvedPromptInput,
    delivery: UserMessageDelivery,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<DriverCommandOutcome, ServiceError> {
    let ResolvedPromptInput {
        display_text,
        model_text: _,
        attachments,
        documents,
        project_files,
        document_context_tokens,
        project_file_context_tokens,
    } = resolved;
    let item_id = projection.next_user_item_id(run_id)?;
    let turn_id = projection.turn_id(run_id)?;
    projection.pending_user_items.push_back(PendingUserItem {
        id: item_id.clone(),
        delivery,
        turn_id: turn_id.clone(),
        documents: documents.clone(),
        project_files: project_files.clone(),
        document_context_tokens,
        project_file_context_tokens,
        context_attributed: false,
        branch_provenance: None,
    });
    projection
        .item_turns
        .insert(item_id.clone(), turn_id.clone());
    if !attachments.is_empty() {
        projection
            .pending_attachments
            .push_back(attachments.clone());
    }
    events
        .send(event(EventPayload::ItemStarted {
            item: SessionItem {
                id: item_id,
                run_id: Some(run_id.clone()),
                turn_id: Some(turn_id),
                provider_attempt: None,
                lifecycle: ItemLifecycle::Provisional,
                durable_entry_id: None,
                payload: ItemPayload::UserMessage {
                    text: bounded_text(&display_text, MAX_PROMPT_BYTES),
                    attachments,
                    documents,
                    project_files,
                    delivery: Some(delivery),
                    branch_provenance: None,
                },
            },
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    Ok(DriverCommandOutcome::default())
}

fn projection_actor_generation(run_id: &RunId) -> u64 {
    run_id
        .as_str()
        .split('-')
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}

fn normalized_tool_name(name: &str) -> String {
    let normalized = name
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.trim_matches('_').is_empty() {
        "tool".into()
    } else {
        normalized
    }
}

fn safe_relative_path(value: &str) -> Option<String> {
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    let mut components = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if components.is_empty() {
        Some(".".into())
    } else {
        Some(components.join("/"))
    }
}

fn safe_public_target(workspace: &Path, value: &str) -> Option<String> {
    if value.contains("://") {
        let url = url::Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        let host = url.host_str()?;
        let port = url
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        return Some(bounded_text(
            &format!("{}://{host}{port}{path}", url.scheme()),
            1024,
        ));
    }
    let source = Path::new(value);
    if !source.is_absolute() {
        return safe_relative_path(value);
    }
    let workspace = workspace.canonicalize().ok()?;
    let candidate = source.canonicalize().ok()?;
    let relative = candidate.strip_prefix(workspace).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(".".into());
    }
    safe_relative_path(&relative.to_string_lossy())
}

fn safe_workspace_path(workspace: &Path, value: &str) -> Option<String> {
    if value.contains("://") {
        return None;
    }
    safe_public_target(workspace, value)
}

fn safe_public_query(workspace: &Path, value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("://") && url::Url::parse(&normalized).is_ok() {
        return safe_public_target(workspace, &normalized);
    }

    let lower = normalized.to_ascii_lowercase();
    let sensitive_assignments = [
        "api_key=",
        "api_key:",
        "api-key=",
        "api-key:",
        "apikey=",
        "apikey:",
        "access_token=",
        "access_token:",
        "access-token=",
        "access-token:",
        "auth_token=",
        "auth_token:",
        "authorization=",
        "authorization:",
        "bearer ",
        "basic ",
        "client_secret=",
        "client_secret:",
        "cookie=",
        "cookie:",
        "credential=",
        "credential:",
        "password=",
        "password:",
        "password ",
        "secret=",
        "secret:",
        "secret ",
        "session_token=",
        "session_token:",
        "token=",
        "token:",
    ];
    let known_token_prefixes = [
        "akia",
        "asia",
        "aiza",
        "dop_v1_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "hf_",
        "npm_",
        "pypi-",
        "sk-",
        "sk_live_",
        "rk_live_",
        "xoxb-",
        "xoxa-",
        "xoxp-",
        "xoxr-",
        "xoxs-",
        "xoxapp-",
        "ya29.",
    ];
    let contains_known_token = normalized.split_ascii_whitespace().any(|word| {
        let word = word.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.')
        });
        let word = word.to_ascii_lowercase();
        word.len() >= 12
            && known_token_prefixes
                .iter()
                .any(|prefix| word.starts_with(prefix))
    });
    if sensitive_assignments
        .iter()
        .any(|needle| lower.contains(needle))
        || contains_known_token
    {
        return Some("[redacted query]".into());
    }

    Some(bounded_text(&normalized, 512))
}

fn search_target(query: Option<String>, path: Option<String>) -> Option<String> {
    match (query, path) {
        (Some(query), Some(path)) => Some(bounded_text(&format!("{query} in {path}"), 1024)),
        (Some(query), None) => Some(query),
        (None, Some(path)) => Some(path),
        (None, None) => None,
    }
}

fn command_activity_details(
    name: &str,
    arguments: &serde_json::Value,
    workspace: &Path,
) -> (Option<String>, bool) {
    if !matches!(name, "bash" | "exec") {
        return (None, false);
    }
    let raw_command = arguments
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if raw_command.is_empty() {
        return (None, false);
    }

    // Keep the complete command visible. The command is already bounded and
    // control-safe at the public boundary; only credential-like values are
    // collapsed so observability does not become an accidental secret leak.
    let command_preview =
        if safe_public_query(workspace, raw_command).as_deref() == Some("[redacted query]") {
            let context = raw_command
                .split_ascii_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            if context.is_empty() {
                "[redacted command]".into()
            } else {
                format!("{context} [redacted arguments]")
            }
        } else {
            raw_command.to_owned()
        };

    // Verification classification remains intentionally conservative and is
    // independent from command visibility. A compound or quoted shell command
    // is still shown in full, but is not promoted to a verified-test phase
    // unless its shape can be classified deterministically.
    let normalized =
        crate::presentation::summarize_tool_with_workspace(name, arguments, Some(workspace))
            .shell_command
            .unwrap_or_default();
    let simple_command = !normalized.chars().any(|character| {
        matches!(
            character,
            '\n' | '\r' | ';' | '|' | '&' | '>' | '<' | '`' | '$' | '\'' | '"'
        )
    });
    let words = normalized.split_ascii_whitespace().collect::<Vec<_>>();
    let program = words
        .first()
        .and_then(|word| Path::new(word).file_name())
        .and_then(|word| word.to_str())
        .unwrap_or_default();
    let subcommand = words.get(1).copied().unwrap_or_default();
    let verification = simple_command
        && matches!(
            (program, subcommand, words.get(2).copied()),
            (
                "cargo",
                "test" | "check" | "clippy" | "fmt" | "build" | "doc",
                _
            ) | (
                "npm" | "pnpm" | "yarn" | "bun",
                "test" | "build" | "lint" | "check",
                _
            ) | (
                "npm" | "pnpm" | "yarn" | "bun",
                "run",
                Some("test" | "build" | "lint" | "check" | "typecheck")
            ) | ("pytest", _, _)
                | ("python" | "python3", "-m", Some("pytest" | "unittest"))
                | ("go", "test" | "vet" | "build", _)
                | ("rustc", _, _)
        );
    (Some(bounded_text(&command_preview, 1024)), verification)
}

fn semantic_tool_activity(
    name: &str,
    arguments: &serde_json::Value,
    workspace: &Path,
    started_at_ms: u64,
) -> ToolActivity {
    let path = arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_public_target(workspace, value));
    let resource_path = arguments
        .get("resource_path")
        .and_then(serde_json::Value::as_str)
        .and_then(safe_relative_path);
    let query = arguments
        .get("query")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_public_query(workspace, value));
    let url = arguments
        .get("url")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_public_target(workspace, value));
    let cwd = arguments
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| safe_workspace_path(workspace, value));
    let (command_preview, verification) = command_activity_details(name, arguments, workspace);
    let remote_read = name == "read"
        && arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.starts_with("http://") || value.starts_with("https://"));
    let (kind, phase, title, target) = match name {
        "read" if remote_read => (
            ToolKind::Web,
            ActivityPhase::Investigated,
            path.as_ref()
                .map(|target| format!("Read {target}"))
                .unwrap_or_else(|| "Read remote resource".into()),
            path,
        ),
        "read" => (
            ToolKind::Read,
            ActivityPhase::Investigated,
            path.as_ref()
                .map(|target| format!("Read {target}"))
                .unwrap_or_else(|| "Read file".into()),
            path,
        ),
        "search" => {
            let target = search_target(query, path);
            (
                ToolKind::Search,
                ActivityPhase::Investigated,
                target
                    .as_ref()
                    .map(|target| format!("Search {target}"))
                    .unwrap_or_else(|| "Search workspace".into()),
                target,
            )
        }
        "edit" => (
            ToolKind::Edit,
            ActivityPhase::Changed,
            path.as_ref()
                .map(|target| format!("Update {target}"))
                .unwrap_or_else(|| "Update file".into()),
            path,
        ),
        "write" => (
            ToolKind::Write,
            ActivityPhase::Changed,
            path.as_ref()
                .map(|target| format!("Write {target}"))
                .unwrap_or_else(|| "Write file".into()),
            path,
        ),
        "bash" | "exec" => (
            ToolKind::Command,
            if verification {
                ActivityPhase::Verified
            } else {
                ActivityPhase::Other
            },
            command_preview
                .as_ref()
                .map(|command| {
                    let single_line = command.replace(['\r', '\n', '\t'], " ");
                    format!("Run {single_line}")
                })
                .unwrap_or_else(|| "Run command".into()),
            None,
        ),
        "read_skill_resource" => (
            ToolKind::Skill,
            ActivityPhase::Investigated,
            resource_path
                .as_ref()
                .map(|target| format!("Read skill resource {target}"))
                .unwrap_or_else(|| "Read skill resource".into()),
            resource_path,
        ),
        "web_search" => {
            let target = url.or(query);
            (
                ToolKind::Web,
                ActivityPhase::Investigated,
                target
                    .as_ref()
                    .map(|target| format!("Search the web for {target}"))
                    .unwrap_or_else(|| "Search the web".into()),
                target,
            )
        }
        _ => (
            ToolKind::Other,
            ActivityPhase::Other,
            format!(
                "Run {}",
                normalized_tool_name(name).replace(['_', '-'], " ")
            ),
            None,
        ),
    };
    ToolActivity {
        raw_tool_name: normalized_tool_name(name),
        kind,
        phase,
        status: ToolActivityStatus::Running,
        title: bounded_single_line_text(&title, 512),
        summary: Some("Running".into()),
        target,
        cwd,
        command_preview,
        exit_code: None,
        signal: None,
        started_at_ms: started_at_ms.max(1),
        completed_at_ms: None,
        duration_ms: None,
        output_summary: None,
        output_handle: None,
        observed_output_bytes: 0,
        dropped_output_bytes: 0,
        changed_paths: Vec::new(),
        source_ids: Vec::new(),
        artifact_ids: Vec::new(),
    }
}

fn parse_process_metadata(text: &str) -> (Option<i32>, Option<i32>, Option<u64>) {
    let mut exit_code = None;
    let mut signal = None;
    let mut duration_ms = None;
    for token in text.lines().take(4).flat_map(str::split_ascii_whitespace) {
        if let Some(value) = token.strip_prefix("exit=") {
            if let Some(value) = value.strip_prefix("signal:") {
                signal = value.parse::<i32>().ok();
            } else if value != "unknown" {
                exit_code = value.parse::<i32>().ok();
            }
        } else if let Some(value) = token
            .strip_prefix("duration=")
            .and_then(|value| value.strip_suffix('s'))
        {
            duration_ms = value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|seconds| (seconds * 1_000.0).round().min(u64::MAX as f64) as u64);
        }
    }
    (exit_code, signal, duration_ms)
}

fn complete_tool_activity(
    mut activity: ToolActivity,
    name: &str,
    result: &Result<ToolOutput, ToolError>,
    completed_at_ms: u64,
    progress: ProjectedToolProgress,
) -> (ToolActivity, ToolResultSummary) {
    let raw_result = match result {
        Ok(output) => output.text.as_str(),
        Err(error) => error.message.as_str(),
    };
    let (exit_code, signal, parsed_duration_ms) = if matches!(name, "bash" | "exec") {
        parse_process_metadata(raw_result)
    } else {
        (None, None, None)
    };
    let failed = crate::presentation::tool_result_is_failure(name, result);
    let status = if failed {
        ToolActivityStatus::Failed
    } else {
        ToolActivityStatus::Succeeded
    };
    let duration_ms = parsed_duration_ms
        .unwrap_or_else(|| completed_at_ms.saturating_sub(activity.started_at_ms));
    let output_summary = match status {
        ToolActivityStatus::Succeeded if activity.phase == ActivityPhase::Verified => {
            Some("Verification completed".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Read => {
            Some("Read completed".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Search => {
            Some("Search completed".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Edit => {
            Some("File updated".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Write => {
            Some("File written".into())
        }
        ToolActivityStatus::Succeeded if activity.kind == ToolKind::Web => {
            Some("Remote lookup completed".into())
        }
        ToolActivityStatus::Succeeded => Some("Tool completed".into()),
        ToolActivityStatus::Failed if activity.phase == ActivityPhase::Verified => {
            Some("Verification failed".into())
        }
        ToolActivityStatus::Failed if exit_code.is_some() => {
            Some(format!("Command exited {}", exit_code.unwrap_or_default()))
        }
        ToolActivityStatus::Failed if signal.is_some() => Some(format!(
            "Command stopped by signal {}",
            signal.unwrap_or_default()
        )),
        ToolActivityStatus::Failed => Some("Tool failed".into()),
        ToolActivityStatus::Running | ToolActivityStatus::Stopped => None,
    };
    let final_bytes = raw_result.len().min(u64::MAX as usize) as u64;
    activity.status = status;
    activity.summary = Some(match status {
        ToolActivityStatus::Succeeded => "Completed".into(),
        ToolActivityStatus::Failed => "Failed".into(),
        ToolActivityStatus::Stopped => "Stopped".into(),
        ToolActivityStatus::Running => "Running".into(),
    });
    activity.exit_code = exit_code;
    activity.signal = signal;
    activity.completed_at_ms = Some(completed_at_ms.max(activity.started_at_ms));
    activity.duration_ms = Some(duration_ms);
    activity.output_summary = output_summary.clone();
    activity.observed_output_bytes = progress.observed_output_bytes.max(final_bytes);
    activity.dropped_output_bytes = progress.dropped_output_bytes;
    let summary = activity
        .summary
        .clone()
        .unwrap_or_else(|| "Completed".into());
    let result = ToolResultSummary {
        tool_call_item_id: ItemId::new("placeholder").expect("static item ID is valid"),
        status,
        summary,
        output_summary,
        output_handle: activity.output_handle.clone(),
        exit_code,
        signal,
        completed_at_ms: activity.completed_at_ms.unwrap_or(completed_at_ms),
        duration_ms,
        observed_output_bytes: activity.observed_output_bytes,
        dropped_output_bytes: activity.dropped_output_bytes,
    };
    (activity, result)
}

fn test_framework_hint(activity: &ToolActivity) -> Option<TestFramework> {
    let command = activity.command_preview.as_deref()?;
    let words = command.split_ascii_whitespace().collect::<Vec<_>>();
    let program = words
        .first()
        .and_then(|word| Path::new(word).file_name())
        .and_then(|word| word.to_str())
        .unwrap_or_default();
    match (program, words.get(1).copied(), words.get(2).copied()) {
        ("cargo", Some("test"), _) => Some(TestFramework::CargoLibtest),
        ("pytest", _, _) | ("python" | "python3", Some("-m"), Some("pytest" | "unittest")) => {
            Some(TestFramework::Pytest)
        }
        ("go", Some("test"), _) => Some(TestFramework::GoTest),
        // Package runners may dispatch either Vitest or Jest, so their
        // deterministic reporter markers select the parser.
        _ => None,
    }
}

fn project_test_results(
    item_id: &ItemId,
    activity: &ToolActivity,
    output: &ToolOutput,
) -> Option<StructuredTestResults> {
    if activity.kind != ToolKind::Command || activity.phase != ActivityPhase::Verified {
        return None;
    }
    let status = match activity.status {
        ToolActivityStatus::Succeeded => TestCommandStatus::Succeeded,
        ToolActivityStatus::Failed => TestCommandStatus::Failed,
        ToolActivityStatus::Stopped => TestCommandStatus::Stopped,
        ToolActivityStatus::Running => return None,
    };
    let bytes = output.text.as_bytes();
    let retained_len = bytes.len().min(MAX_TEST_OUTPUT_BYTES);
    parse_test_output(TestOutputInput {
        origin_item_id: item_id.clone(),
        output: &bytes[..retained_len],
        input_truncated: bytes.len() > retained_len || activity.dropped_output_bytes > 0,
        command: TestCommandOutcome {
            status,
            exit_code: activity.exit_code,
            signal: activity.signal,
        },
        framework_hint: test_framework_hint(activity),
    })
    .ok()
}

fn stable_tool_item_id(tool_call_id: &str) -> Result<ItemId, ServiceError> {
    let hash = stable_hash(tool_call_id.as_bytes());
    ItemId::new(format!("item-tool-{}", &hash[..24])).map_err(|_| ServiceError::Internal)
}

fn public_compaction_reason(reason: CompactionReason) -> ContextCompactionReason {
    match reason {
        CompactionReason::Threshold => ContextCompactionReason::Threshold,
        CompactionReason::Overflow => ContextCompactionReason::Overflow,
    }
}

fn public_run_phase(phase: AgentRunPhase) -> ServeRunPhase {
    match phase {
        AgentRunPhase::Preparing => ServeRunPhase::Preparing,
        AgentRunPhase::Responding => ServeRunPhase::Responding,
        AgentRunPhase::Retrying => ServeRunPhase::Retrying,
        AgentRunPhase::Compacting => ServeRunPhase::Compacting,
        AgentRunPhase::ExecutingTool => ServeRunPhase::ExecutingTool,
        AgentRunPhase::Finished => ServeRunPhase::Finished,
    }
}

fn public_terminal_state(state: AgentRunTerminalState) -> ServeRunTerminalState {
    match state {
        AgentRunTerminalState::Completed => ServeRunTerminalState::Completed,
        AgentRunTerminalState::Aborted => ServeRunTerminalState::Aborted,
        AgentRunTerminalState::Failed => ServeRunTerminalState::Failed,
        AgentRunTerminalState::MaxTurns => ServeRunTerminalState::MaxTurns,
        AgentRunTerminalState::Dropped => ServeRunTerminalState::Dropped,
    }
}

fn public_context_totals(
    context: &AgentContextBreakdown,
    projection: &RunContextProjection,
) -> Result<ContextTotals, ServiceError> {
    if context.categorized_tokens() != context.total_tokens {
        return Err(ServiceError::Internal);
    }
    let project_instructions = projection
        .project_instruction_tokens
        .min(context.instruction_tokens);
    let base_instructions = context
        .instruction_tokens
        .saturating_sub(project_instructions);
    let system = context
        .system_tokens
        .checked_add(base_instructions)
        .ok_or(ServiceError::Internal)?;
    let documents = projection
        .document_context_tokens
        .min(context.conversation_tokens);
    let remaining_conversation = context.conversation_tokens.saturating_sub(documents);
    let project_files = projection
        .project_file_context_tokens
        .min(remaining_conversation);
    let conversation = remaining_conversation.saturating_sub(project_files);
    ContextTotals::try_new(
        vec![
            ContextCategoryTotal {
                category: ContextCategory::System,
                tokens: system,
            },
            ContextCategoryTotal {
                category: ContextCategory::ProjectInstructions,
                tokens: project_instructions,
            },
            ContextCategoryTotal {
                category: ContextCategory::Conversation,
                tokens: conversation,
            },
            ContextCategoryTotal {
                category: ContextCategory::ToolResults,
                tokens: context.tool_result_tokens,
            },
            ContextCategoryTotal {
                category: ContextCategory::Attachments,
                tokens: context.attachment_tokens,
            },
            ContextCategoryTotal {
                category: ContextCategory::Documents,
                tokens: documents,
            },
            ContextCategoryTotal {
                category: ContextCategory::ProjectFiles,
                tokens: project_files,
            },
            ContextCategoryTotal {
                category: ContextCategory::CompactionSummaries,
                tokens: context.compaction_summary_tokens,
            },
            ContextCategoryTotal {
                category: ContextCategory::Other,
                tokens: context.other_tokens,
            },
        ],
        context.total_tokens,
    )
    .map_err(|_| ServiceError::Internal)
}

fn public_compaction_id(run_id: &RunId, compaction_id: u64) -> Result<RuntimeId, ServiceError> {
    let source = format!("{}:{compaction_id}", run_id.as_str());
    RuntimeId::new(format!(
        "compaction.{}",
        &stable_hash(source.as_bytes())[..24]
    ))
    .map_err(|_| ServiceError::Internal)
}

fn project_context_snapshot(
    snapshot: AgentContextSnapshot,
    run_id: &RunId,
    projection: &mut RunContextProjection,
) -> Result<Option<ContextUsage>, ServiceError> {
    if let Some(previous) = &projection.last_agent_snapshot {
        if snapshot.revision < previous.revision {
            return Err(ServiceError::Internal);
        }
        if snapshot.revision == previous.revision {
            return if &snapshot == previous {
                Ok(None)
            } else {
                Err(ServiceError::Internal)
            };
        }
    }
    let observed_at_ms = now_ms().max(projection.context_updated_at_ms);
    let new_finished = snapshot.last_compaction.as_ref().is_some_and(|finished| {
        projection
            .last_compaction
            .as_ref()
            .is_none_or(|(id, _)| *id != finished.id)
    });

    if new_finished {
        let finished = snapshot
            .last_compaction
            .as_ref()
            .ok_or(ServiceError::Internal)?;
        let (started_at_ms, before) = match projection.active_compaction.as_ref() {
            Some((id, active)) if *id == finished.id => {
                (active.started_at_ms, active.before.clone())
            }
            Some(_) => return Err(ServiceError::Internal),
            None => (
                observed_at_ms,
                public_context_totals(&finished.before, projection)?,
            ),
        };
        if finished.succeeded {
            projection.clear_auxiliary_sources();
        }
        let after = public_context_totals(&finished.after, projection)?;
        let reclaimed_tokens = before
            .total_tokens
            .checked_sub(after.total_tokens)
            .ok_or(ServiceError::Internal)?;
        if !finished.succeeded && (before != after || reclaimed_tokens != 0) {
            return Err(ServiceError::Internal);
        }
        let finished_at_ms = observed_at_ms.max(started_at_ms);
        projection.last_compaction = Some((
            finished.id,
            CompletedCompaction {
                id: public_compaction_id(run_id, finished.id)?,
                reason: public_compaction_reason(finished.reason),
                before,
                after,
                reclaimed_tokens,
                succeeded: finished.succeeded,
                started_at_ms,
                finished_at_ms,
            },
        ));
        projection.active_compaction = None;
        projection.context_updated_at_ms = finished_at_ms;
    }

    let current = public_context_totals(&snapshot.context, projection)?;
    if new_finished
        && projection
            .last_compaction
            .as_ref()
            .is_none_or(|(_, completed)| completed.after != current)
    {
        return Err(ServiceError::Internal);
    }
    if projection.current_totals.as_ref() != Some(&current) {
        projection.current_totals = Some(current.clone());
        let mut updated_at_ms = observed_at_ms;
        if !new_finished {
            if let Some((_, completed)) = &projection.last_compaction {
                if updated_at_ms <= completed.finished_at_ms && current != completed.after {
                    updated_at_ms = completed
                        .finished_at_ms
                        .checked_add(1)
                        .ok_or(ServiceError::Internal)?;
                }
            }
        }
        projection.context_updated_at_ms = updated_at_ms;
    }

    if let Some(active) = &snapshot.active_compaction {
        let before = public_context_totals(&active.before, projection)?;
        if before != current {
            return Err(ServiceError::Internal);
        }
        match projection.active_compaction.as_ref() {
            Some((id, existing)) if *id == active.id => {
                if existing.before != before
                    || existing.reason != public_compaction_reason(active.reason)
                {
                    return Err(ServiceError::Internal);
                }
            }
            Some(_) => return Err(ServiceError::Internal),
            None => {
                let started_at_ms = observed_at_ms.max(projection.context_updated_at_ms);
                projection.active_compaction = Some((
                    active.id,
                    ActiveCompaction {
                        id: public_compaction_id(run_id, active.id)?,
                        reason: public_compaction_reason(active.reason),
                        before,
                        started_at_ms,
                    },
                ));
            }
        }
    } else if !new_finished && projection.active_compaction.is_some() {
        return Err(ServiceError::Internal);
    }

    let compactions =
        u32::try_from(snapshot.compactions_completed).map_err(|_| ServiceError::Internal)?;
    let usage = &snapshot.run_usage;
    let context = ContextUsage {
        usage_uncertain: projection.usage_uncertain,
        usage: UsageSnapshot {
            input_tokens: usage
                .input_tokens
                .saturating_add(usage.cache_read_tokens)
                .saturating_add(usage.cache_write_tokens),
            output_tokens: usage.output_tokens,
            context_tokens: current.total_tokens,
            context_limit: Some(snapshot.context.context_limit),
        },
        compactions,
        status: ContextStatus {
            current,
            updated_at_ms: projection.context_updated_at_ms,
            active_compaction: projection
                .active_compaction
                .as_ref()
                .map(|(_, active)| active.clone()),
            last_compaction: projection
                .last_compaction
                .as_ref()
                .map(|(_, completed)| completed.clone()),
        },
        run: Some(AgentRunTelemetry {
            phase: public_run_phase(snapshot.phase),
            terminal_state: snapshot.terminal_state.map(public_terminal_state),
            responses_started: snapshot.responses_started,
            responses_finished: snapshot.responses_finished,
            responses_discarded: snapshot.responses_discarded,
            response_active: snapshot.response_active,
            tool_calls_started: snapshot.tool_calls_started,
            tool_calls_finished: snapshot.tool_calls_finished,
            tool_executions_started: snapshot.tool_executions_started,
            tool_executions_finished: snapshot.tool_executions_finished,
            compactions_started: snapshot.compactions_started,
            compactions_completed: snapshot.compactions_completed,
            compactions_failed: snapshot.compactions_failed,
        }),
    };
    context.validate().map_err(|_| ServiceError::Internal)?;
    projection.last_agent_snapshot = Some(snapshot);
    if projection.last_published.as_ref() == Some(&context) {
        return Ok(None);
    }
    projection.last_published = Some(context.clone());
    Ok(Some(context))
}

async fn publish_context_snapshot(
    snapshot: AgentContextSnapshot,
    run_id: &RunId,
    projection: &mut RunContextProjection,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let Some(context) = project_context_snapshot(snapshot, run_id, projection)? else {
        return Ok(());
    };
    events
        .send(event(EventPayload::ContextUpdated { context }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

fn attribute_delivered_prompt_context(
    projection: &mut ProjectionState,
    context_projection: &mut RunContextProjection,
    delivery: UserMessageDelivery,
    delivered_count: usize,
) -> Result<(), ServiceError> {
    let mut attributed = 0usize;
    for pending in &mut projection.pending_user_items {
        if attributed == delivered_count {
            break;
        }
        if pending.delivery != delivery || pending.context_attributed {
            continue;
        }
        context_projection.attribute_sources(
            pending.document_context_tokens,
            pending.project_file_context_tokens,
        );
        pending.context_attributed = true;
        attributed = attributed.saturating_add(1);
    }
    if attributed != delivered_count {
        return Err(ServiceError::Internal);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn project_agent_event(
    agent_event: AgentEvent,
    run_id: &RunId,
    plan: &WorkerPlan,
    provider_model: &Model,
    projection: &mut ProjectionState,
    context_projection: &mut RunContextProjection,
    events: &mpsc::Sender<TimestampedEvent>,
    response_text: &mut String,
) -> Result<Option<HostRunOutcome>, ServiceError> {
    match agent_event {
        AgentEvent::ProviderUsageUncertain => {
            projection.usage_uncertain = true;
            context_projection.usage_uncertain = true;
            // Publish independently of host-marker persistence: a failed append
            // must not hide the session's already-known accounting uncertainty.
            let mut context = context_projection
                .last_published
                .clone()
                .unwrap_or_default();
            context.usage_uncertain = true;
            context_projection.last_published = Some(context.clone());
            projection.last_context = Some(context.clone());
            let persisted = plan
                .usage
                .lock()
                .map_err(|_| ServiceError::Internal)
                .and_then(|mut usage| {
                    usage
                        .record_uncertainty(plan.session_id.as_str())
                        .map_err(usage_store_service_error)
                });
            events
                .send(event(EventPayload::ContextUpdated { context }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            crate::output::stderr!("warning: provider usage and cost are uncertain; displayed numeric usage is a known subtotal, not a complete total.");
            persisted?;
        }
        AgentEvent::TurnStarted => {}
        AgentEvent::ProviderLifecycle { .. }
        | AgentEvent::ProviderWaitingForNetwork { .. }
        | AgentEvent::ProviderOperationRetry { .. } => {
            // Serve's durable item protocol intentionally has no endpoint-status
            // item. Keep transport readiness out of session projections. A
            // pre-send wait neither settles the run nor retracts committed items.
        }
        AgentEvent::OutputDelta { channel, text } => {
            let text = bounded_text(&text, MAX_ITEM_TEXT_BYTES);
            let turn_id = projection.turn_id(run_id)?;
            let (slot, kind, payload, delta) = match channel {
                OutputChannel::Text => (
                    &mut projection.assistant_item,
                    "assistant",
                    ItemPayload::AssistantMessage {
                        text: String::new(),
                    },
                    ItemDelta::AssistantText {
                        append: text.clone(),
                    },
                ),
                OutputChannel::Reasoning => (
                    &mut projection.reasoning_item,
                    "reasoning",
                    ItemPayload::Reasoning {
                        text: String::new(),
                    },
                    ItemDelta::ReasoningText {
                        append: text.clone(),
                    },
                ),
            };
            if let Some(item_id) = slot.clone() {
                events
                    .send(event(EventPayload::ItemDelta { item_id, delta }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            } else {
                let item_id = ItemId::new(format!(
                    "item-{}-{kind}-{}-{}",
                    run_id.as_str(),
                    projection.turn_counter,
                    projection.provider_attempt
                ))
                .map_err(|_| ServiceError::Internal)?;
                let payload = match payload {
                    ItemPayload::AssistantMessage { .. } => ItemPayload::AssistantMessage { text },
                    ItemPayload::Reasoning { .. } => ItemPayload::Reasoning { text },
                    _ => unreachable!(),
                };
                events
                    .send(event(EventPayload::ItemStarted {
                        item: SessionItem {
                            id: item_id.clone(),
                            run_id: Some(run_id.clone()),
                            turn_id: Some(turn_id.clone()),
                            provider_attempt: Some(projection.provider_attempt),
                            lifecycle: ItemLifecycle::Provisional,
                            durable_entry_id: None,
                            payload,
                        },
                    }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
                projection.item_turns.insert(item_id.clone(), turn_id);
                *slot = Some(item_id);
            }
        }
        AgentEvent::OutputMedia { .. } => {
            // The shipped Serve protocol has no generated-media item type.
            // TurnFinished still carries the durable assistant message.
        }
        AgentEvent::ProviderRetry { .. } | AgentEvent::CandidateRejected { .. } => {
            retract_attempt(run_id, projection, events).await?;
            projection.provider_attempt = projection.provider_attempt.saturating_add(1);
        }
        AgentEvent::ToolStarted { id, name, args } => {
            let item_id = stable_tool_item_id(&id.0)?;
            let turn_id = projection.turn_id(run_id)?;
            let started_at_ms = now_ms();
            projection.tool_items.insert(id.0.clone(), item_id.clone());
            let arguments = if octet_serve_backend::validate_json(
                "tool.arguments",
                &args,
                256 * 1024,
            )
            .is_ok()
            {
                args
            } else {
                serde_json::Value::Null
            };
            let activity =
                semantic_tool_activity(&name, &arguments, &plan.config.workspace, started_at_ms);
            projection.tool_calls.insert(
                id.0.clone(),
                ProjectedToolCall {
                    name: name.clone(),
                    arguments,
                    activity: activity.clone(),
                    result: None,
                    turn_id: turn_id.clone(),
                },
            );
            projection
                .item_turns
                .insert(item_id.clone(), turn_id.clone());
            events
                .send(event(EventPayload::ItemStarted {
                    item: SessionItem {
                        id: item_id,
                        run_id: Some(run_id.clone()),
                        turn_id: Some(turn_id),
                        provider_attempt: Some(projection.provider_attempt),
                        lifecycle: ItemLifecycle::Provisional,
                        durable_entry_id: None,
                        payload: ItemPayload::ToolCall(activity),
                    },
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
        }
        AgentEvent::ToolPolicyDecision { .. } => {
            // The Serve protocol currently has no policy-decision item or
            // delta. ToolFinished still projects the corresponding denial;
            // keep this explicit so adding agent-side policy provenance does
            // not silently change the graphical projection contract.
        }
        AgentEvent::ToolProgress { id, progress } => {
            project_tool_progress(id, progress, run_id, projection, events).await?;
        }
        AgentEvent::ToolFinished {
            id,
            result,
            duration: _,
        } => {
            let tool_item_id = projection.tool_items.get(&id.0).cloned();
            let projected = projection.tool_calls.get(&id.0).cloned();
            if let (Some(item_id), Some(mut tool)) = (tool_item_id.clone(), projected) {
                let progress = projection.tool_progress.remove(&id.0).unwrap_or_default();
                let (activity, mut semantic_result) = complete_tool_activity(
                    tool.activity.clone(),
                    &tool.name,
                    &result,
                    now_ms(),
                    progress,
                );
                semantic_result.tool_call_item_id = item_id.clone();
                tool.activity = activity.clone();
                tool.result = Some(semantic_result);
                projection.tool_calls.insert(id.0.clone(), tool);
                if let Ok(output) = result.as_ref() {
                    if let Some(test_results) = project_test_results(&item_id, &activity, output) {
                        projection.test_results.push(test_results);
                    }
                }
                events
                    .send(event(EventPayload::ItemDelta {
                        item_id,
                        delta: ItemDelta::ToolActivity { activity },
                    }))
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            if let (Some(_), Some(tool), Some(tool_item_id), Ok(output)) = (
                plan.resources.as_ref(),
                projection.tool_calls.get(&id.0).cloned(),
                tool_item_id,
                result.as_ref(),
            ) {
                projection
                    .pending_tool_evidence
                    .push_back(CompletedToolEvidence {
                        tool_call_id: id.0,
                        tool_item_id,
                        turn_id: projection.turn_id(run_id)?,
                        tool,
                        output: output.clone(),
                    });
            }
        }
        AgentEvent::TurnFinished { message, .. } => {
            response_text.clear();
            response_text.push_str(&super::assistant_text(&message));
            projection.finish_turn();
        }
        AgentEvent::RunFinished { reason, .. } => {
            return Ok(Some(HostRunOutcome::from_finish_reason(
                &reason,
                &provider_model.endpoint.id.0,
                &provider_model.spec.id.0,
            )));
        }
        AgentEvent::SteeringDelivered { messages } => {
            attribute_delivered_prompt_context(
                projection,
                context_projection,
                UserMessageDelivery::Steer,
                messages.len(),
            )?;
        }
        AgentEvent::FollowUpDelivered { messages } => {
            attribute_delivered_prompt_context(
                projection,
                context_projection,
                UserMessageDelivery::FollowUp,
                messages.len(),
            )?;
        }
        AgentEvent::RecoveredOutput { .. } | AgentEvent::DelegationUpdated { .. } => {
            // Serve projects owner-fenced subagent state through extension
            // presentation snapshots; native telemetry is TUI-local run chrome.
        }
        AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionFinished { .. } => {}
    }
    Ok(None)
}

struct WorkspaceFileSnapshot {
    display_path: String,
    display_name: String,
    bytes: bytes::Bytes,
    media_type: &'static str,
    artifact_kind: ArtifactKind,
}

const STORED_EVIDENCE_VERSION: u16 = 2;
const STORED_RUN_RECORD_VERSION: u16 = 1;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRunItemAttribution {
    durable_entry_id: String,
    ordinal: u32,
    item_id: String,
    turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_delivery: Option<UserMessageDelivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    documents: Vec<DocumentReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    project_files: Vec<TrustedFileEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch_provenance: Option<ConversationBranchProvenance>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRunTool {
    tool_call_id: String,
    item_id: String,
    turn_id: String,
    activity: ToolActivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<ToolResultSummary>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRunRecord {
    version: u16,
    session_id: String,
    run_id: String,
    outcome_entry_id: String,
    started_at_ms: u64,
    completed_at_ms: u64,
    items: Vec<StoredRunItemAttribution>,
    tools: Vec<StoredRunTool>,
    review: CompletionReview,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredToolEvidence {
    version: u16,
    session_id: String,
    tool_call_id: String,
    call_entry_id: String,
    result_entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin_item_id: Option<String>,
    entries: Vec<StoredEvidenceEntry>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StoredEvidenceEntry {
    Source {
        item_id: String,
        source_id: String,
        source_kind: SourceKind,
        title: String,
        handle: String,
        consulted_at_ms: u64,
    },
    FileChange {
        item_id: String,
        diff_handle: String,
        result_handle: String,
        display_path: String,
        additions: u32,
        deletions: u32,
    },
    Artifact {
        item_id: String,
        artifact_id: String,
        artifact_kind: ArtifactKind,
        name: String,
        media_type: String,
        handle: String,
        byte_len: u64,
        content_hash: String,
    },
}

struct EvidenceProjection {
    items: Vec<SessionItem>,
    sources: Vec<SourceRef>,
    artifacts: Vec<ArtifactRef>,
}

#[allow(clippy::too_many_arguments)]
fn project_tool_evidence(
    session: &Session,
    workspace: &Path,
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    run_id: &RunId,
    turn_id: &TurnId,
    tool_call_id: &str,
    tool_item_id: &ItemId,
    tool: &ProjectedToolCall,
    output: &ToolOutput,
) -> Vec<EventPayload> {
    match project_tool_evidence_inner(
        session,
        workspace,
        resources,
        session_id,
        run_id,
        turn_id,
        tool_call_id,
        tool_item_id,
        tool,
        output,
    ) {
        Ok(events) => events,
        Err(_) => {
            let _ = resources.rollback_uncommitted_tool_resources(session_id, tool_call_id);
            Vec::new()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn project_tool_evidence_inner(
    session: &Session,
    workspace: &Path,
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    run_id: &RunId,
    turn_id: &TurnId,
    tool_call_id: &str,
    tool_item_id: &ItemId,
    tool: &ProjectedToolCall,
    output: &ToolOutput,
) -> Result<Vec<EventPayload>, ServiceError> {
    let (call_entry_id, result_entry_id) =
        durable_tool_anchor(session, tool_call_id).ok_or(ServiceError::InvalidBoundary)?;
    let identity = stable_hash(
        format!(
            "{}\0{}\0{}\0{}",
            session_id.as_str(),
            call_entry_id.as_str(),
            result_entry_id.as_str(),
            tool_call_id
        )
        .as_bytes(),
    );
    let Some(short_identity) = identity.get(..24) else {
        return Err(ServiceError::Internal);
    };

    let entries = match tool.name.as_str() {
        "read" => {
            let Some(path) = tool
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
            else {
                return Ok(Vec::new());
            };
            let Some(snapshot) = snapshot_workspace_file(workspace, path) else {
                return Ok(Vec::new());
            };
            if trusted_output_hash(&output.text).as_deref()
                != Some(stable_hash(&snapshot.bytes).as_str())
            {
                return Ok(Vec::new());
            }
            let stored = resources
                .register(
                    session_id,
                    tool_call_id,
                    "source",
                    &snapshot.display_name,
                    snapshot.media_type,
                    snapshot.bytes,
                )
                .map_err(resource_store_service_error)?;
            vec![StoredEvidenceEntry::Source {
                item_id: format!("item-source-{short_identity}"),
                source_id: format!("source-{short_identity}"),
                source_kind: SourceKind::File,
                title: snapshot.display_path,
                handle: stored.handle,
                consulted_at_ms: now_ms(),
            }]
        }
        "read_skill_resource" => {
            let Some(title) = tool
                .arguments
                .get("resource_path")
                .and_then(serde_json::Value::as_str)
                .and_then(safe_relative_path)
            else {
                return Ok(Vec::new());
            };
            let stored = resources
                .register(
                    session_id,
                    tool_call_id,
                    "source",
                    &title,
                    "text/plain",
                    bytes::Bytes::copy_from_slice(output.text.as_bytes()),
                )
                .map_err(resource_store_service_error)?;
            vec![StoredEvidenceEntry::Source {
                item_id: format!("item-source-{short_identity}"),
                source_id: format!("source-{short_identity}"),
                source_kind: SourceKind::Resource,
                title: bounded_text(&title, 512),
                handle: stored.handle,
                consulted_at_ms: now_ms(),
            }]
        }
        "edit" | "write" => {
            let Some(path) = tool
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
            else {
                return Ok(Vec::new());
            };
            let Some(snapshot) = snapshot_workspace_file(workspace, path) else {
                return Ok(Vec::new());
            };
            let snapshot_hash = stable_hash(&snapshot.bytes);
            if trusted_output_hash(&output.text).as_deref() != Some(snapshot_hash.as_str()) {
                return Ok(Vec::new());
            }
            let write_created = tool.name == "write" && output_reports_created(&output.text);
            if tool.name == "write" && output.text.contains("\n(no change)") {
                return Ok(Vec::new());
            }
            let diff = if write_created {
                let Some(content) = tool
                    .arguments
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(Vec::new());
                };
                creation_diff(&snapshot.display_path, content)
            } else {
                let Some(detail) = output.text.splitn(3, '\n').nth(2) else {
                    return Ok(Vec::new());
                };
                if !detail.starts_with("--- ") {
                    return Ok(Vec::new());
                }
                detail.to_owned()
            };
            if diff.is_empty() || diff.len() > MAX_OPAQUE_RESOURCE_BYTES {
                return Ok(Vec::new());
            }
            let diff_name = format!("{}.diff", snapshot.display_name);
            let stored_diff = resources
                .register(
                    session_id,
                    tool_call_id,
                    "diff",
                    &diff_name,
                    "text/plain",
                    bytes::Bytes::from(diff.clone()),
                )
                .map_err(resource_store_service_error)?;
            let stored_result = resources
                .register(
                    session_id,
                    tool_call_id,
                    "result",
                    &snapshot.display_name,
                    snapshot.media_type,
                    snapshot.bytes,
                )
                .map_err(resource_store_service_error)?;
            let (additions, deletions) = if tool.name == "edit" {
                (
                    line_count(
                        tool.arguments
                            .get("new")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    ),
                    line_count(
                        tool.arguments
                            .get("old")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    ),
                )
            } else {
                diff_line_counts(&diff)
            };
            let mut entries = vec![StoredEvidenceEntry::FileChange {
                item_id: format!("item-file-change-{short_identity}"),
                diff_handle: stored_diff.handle,
                result_handle: stored_result.handle.clone(),
                display_path: snapshot.display_path,
                additions,
                deletions,
            }];
            if write_created && is_deliverable_artifact(snapshot.artifact_kind) {
                entries.push(StoredEvidenceEntry::Artifact {
                    item_id: format!("item-artifact-{short_identity}"),
                    artifact_id: format!("artifact-{short_identity}"),
                    artifact_kind: snapshot.artifact_kind,
                    name: snapshot.display_name,
                    media_type: snapshot.media_type.to_owned(),
                    handle: stored_result.handle,
                    byte_len: stored_result.byte_len,
                    content_hash: stored_result.sha256,
                });
            }
            entries
        }
        _ => return Ok(Vec::new()),
    };

    let record = StoredToolEvidence {
        version: STORED_EVIDENCE_VERSION,
        session_id: session_id.as_str().to_owned(),
        tool_call_id: tool_call_id.to_owned(),
        call_entry_id: call_entry_id.as_str().to_owned(),
        result_entry_id: result_entry_id.as_str().to_owned(),
        run_id: Some(run_id.as_str().to_owned()),
        turn_id: Some(turn_id.as_str().to_owned()),
        origin_item_id: Some(tool_item_id.as_str().to_owned()),
        entries,
    };
    let record_bytes = serde_json::to_vec(&record).map_err(|_| ServiceError::Internal)?;
    resources
        .persist_record(session_id, &result_entry_id, tool_call_id, &record_bytes)
        .map_err(resource_store_service_error)?;
    let projection = project_stored_evidence(
        resources,
        session_id,
        &record,
        Some(run_id.clone()),
        Some(turn_id.clone()),
        Some(tool_item_id.clone()),
    )?;
    let mut events = Vec::new();
    for source in projection.sources {
        events.push(EventPayload::SourceUpserted { source });
    }
    for artifact in projection.artifacts {
        events.push(EventPayload::ArtifactUpserted { artifact });
    }
    for item in projection.items {
        events.push(EventPayload::ItemCommitted { item });
    }
    Ok(events)
}

fn durable_tool_anchor(
    session: &Session,
    tool_call_id: &str,
) -> Option<(DurableEntryId, DurableEntryId)> {
    let mut cursor = session.head_ref();
    let mut result_entry_id = None;
    while let Some(entry_id) = cursor {
        let entry = session.entry(entry_id)?;
        match &entry.value {
            EntryValue::Message(Message::User(message))
                if result_entry_id.is_none()
                    && message.content.iter().any(|part| {
                        matches!(
                            part,
                            UserPart::ToolResult(result)
                                if result.tool_call_id.0 == tool_call_id && !result.is_error
                        )
                    }) =>
            {
                result_entry_id = DurableEntryId::new(entry.id.0.clone()).ok();
            }
            EntryValue::Message(Message::Assistant(message))
                if result_entry_id.is_some()
                    && message.content.iter().any(|part| {
                        matches!(
                            part,
                            AssistantPart::ToolCall(call) if call.id.0 == tool_call_id
                        )
                    }) =>
            {
                return Some((
                    DurableEntryId::new(entry.id.0.clone()).ok()?,
                    result_entry_id?,
                ));
            }
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    None
}

fn trusted_output_hash(text: &str) -> Option<String> {
    text.split_ascii_whitespace()
        .find_map(|token| token.strip_prefix("hash="))
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .map(str::to_owned)
}

fn output_reports_created(text: &str) -> bool {
    text.lines()
        .nth(1)
        .is_some_and(|line| line.contains("  created hash="))
}

fn creation_diff(path: &str, content: &str) -> String {
    let total = content.lines().count();
    let mut diff = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{total} @@\n");
    for line in content.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn diff_line_counts(diff: &str) -> (u32, u32) {
    let additions = diff
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count()
        .min(u32::MAX as usize) as u32;
    let deletions = diff
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count()
        .min(u32::MAX as usize) as u32;
    (additions, deletions)
}

fn is_deliverable_artifact(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::Site
            | ArtifactKind::Document
            | ArtifactKind::Spreadsheet
            | ArtifactKind::Presentation
    )
}

fn project_stored_evidence(
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    record: &StoredToolEvidence,
    run_id: Option<RunId>,
    turn_id: Option<TurnId>,
    origin_item_id: Option<ItemId>,
) -> Result<EvidenceProjection, ServiceError> {
    if !matches!(record.version, 1 | STORED_EVIDENCE_VERSION)
        || record.session_id != session_id.as_str()
        || record.entries.is_empty()
    {
        return Err(ServiceError::InvalidSeed);
    }
    let durable_entry_id = DurableEntryId::new(record.result_entry_id.clone())
        .map_err(|_| ServiceError::InvalidSeed)?;
    let run_id = record
        .run_id
        .clone()
        .and_then(|value| RunId::new(value).ok())
        .or(run_id);
    let turn_id = record
        .turn_id
        .clone()
        .and_then(|value| TurnId::new(value).ok())
        .or(turn_id);
    let origin_item_id = record
        .origin_item_id
        .clone()
        .and_then(|value| ItemId::new(value).ok())
        .or(origin_item_id);
    let mut projection = EvidenceProjection {
        items: Vec::new(),
        sources: Vec::new(),
        artifacts: Vec::new(),
    };
    for entry in &record.entries {
        let (item_id, payload) = match entry {
            StoredEvidenceEntry::Source {
                item_id,
                source_id,
                source_kind,
                title,
                handle,
                consulted_at_ms,
            } => {
                let source = SourceRef {
                    id: SourceId::new(source_id.clone()).map_err(|_| ServiceError::InvalidSeed)?,
                    kind: *source_kind,
                    title: bounded_text(title, 512),
                    handle: handle.clone(),
                    origin_item_id: origin_item_id.clone(),
                    consulted_at_ms: *consulted_at_ms,
                    cited: false,
                    available: resources.content(session_id, handle).is_ok(),
                };
                projection.sources.push(source.clone());
                (item_id, ItemPayload::Source(source))
            }
            StoredEvidenceEntry::FileChange {
                item_id,
                diff_handle,
                result_handle,
                display_path,
                additions,
                deletions,
            } => (
                item_id,
                ItemPayload::FileChange(FileChange {
                    handle: diff_handle.clone(),
                    result_handle: Some(result_handle.clone()),
                    display_path: bounded_text(display_path, 1024),
                    origin_item_id: origin_item_id.clone(),
                    additions: *additions,
                    deletions: *deletions,
                }),
            ),
            StoredEvidenceEntry::Artifact {
                item_id,
                artifact_id,
                artifact_kind,
                name,
                media_type,
                handle,
                byte_len,
                content_hash,
            } => {
                let artifact = ArtifactRef {
                    id: ArtifactId::new(artifact_id.clone())
                        .map_err(|_| ServiceError::InvalidSeed)?,
                    kind: *artifact_kind,
                    name: bounded_text(name, 512),
                    media_type: media_type.clone(),
                    handle: handle.clone(),
                    byte_len: *byte_len,
                    content_hash: Some(content_hash.clone()),
                    origin_item_id: origin_item_id.clone(),
                    available: resources.content(session_id, handle).is_ok(),
                };
                projection.artifacts.push(artifact.clone());
                (item_id, ItemPayload::Artifact(artifact))
            }
        };
        projection.items.push(SessionItem {
            id: ItemId::new(item_id.clone()).map_err(|_| ServiceError::InvalidSeed)?,
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
            provider_attempt: None,
            lifecycle: ItemLifecycle::Committed,
            durable_entry_id: Some(durable_entry_id.clone()),
            payload,
        });
    }
    Ok(projection)
}

fn snapshot_workspace_file(workspace: &Path, requested: &str) -> Option<WorkspaceFileSnapshot> {
    let workspace = workspace.canonicalize().ok()?;
    let requested_path = if requested.contains("://") || requested.starts_with("file:") {
        let url = url::Url::parse(requested).ok()?;
        if url.scheme() != "file"
            || url.fragment().is_some()
            || (!url.username().is_empty() || url.password().is_some())
            || !matches!(url.host_str(), None | Some("") | Some("localhost"))
        {
            return None;
        }
        url.to_file_path().ok()?
    } else {
        PathBuf::from(requested)
    };
    let candidate = if requested_path.is_absolute() {
        requested_path
    } else {
        workspace.join(requested_path)
    };
    let link_metadata = candidate.symlink_metadata().ok()?;
    if link_metadata.file_type().is_symlink() {
        return None;
    }
    let canonical = candidate.canonicalize().ok()?;
    if canonical == workspace || !canonical.starts_with(&workspace) {
        return None;
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&canonical).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_OPAQUE_RESOURCE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(MAX_OPAQUE_RESOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_OPAQUE_RESOURCE_BYTES {
        return None;
    }
    let relative = canonical.strip_prefix(&workspace).ok()?;
    let display_path = bounded_text(&relative.to_string_lossy().replace('\\', "/"), 512);
    let display_name = canonical.file_name()?.to_str()?.to_owned();
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(WorkspaceFileSnapshot {
        display_path,
        display_name,
        media_type: workspace_media_type(&extension, &bytes),
        artifact_kind: artifact_kind_for_extension(&extension),
        bytes: bytes::Bytes::from(bytes),
    })
}

fn workspace_media_type(extension: &str, bytes: &[u8]) -> &'static str {
    if std::str::from_utf8(bytes).is_err() {
        return match extension {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "pdf" => "application/pdf",
            _ => "application/octet-stream",
        };
    }
    match extension {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "jsx" | "mjs" | "cjs" => "text/javascript",
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "svg" => "image/svg+xml",
        _ => "text/plain",
    }
}

fn artifact_kind_for_extension(extension: &str) -> ArtifactKind {
    match extension {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => ArtifactKind::Image,
        "pdf" | "doc" | "docx" | "md" | "txt" => ArtifactKind::Document,
        "csv" | "tsv" | "xls" | "xlsx" => ArtifactKind::Spreadsheet,
        "ppt" | "pptx" | "key" => ArtifactKind::Presentation,
        "html" | "htm" => ArtifactKind::Site,
        "rs" | "js" | "jsx" | "ts" | "tsx" | "css" | "json" | "toml" | "yaml" | "yml" | "py"
        | "go" | "java" | "kt" | "swift" | "c" | "h" | "cpp" | "hpp" | "sh" => ArtifactKind::File,
        _ => ArtifactKind::Other,
    }
}

fn line_count(value: &str) -> u32 {
    value.lines().count().min(u32::MAX as usize) as u32
}

struct TerminalProjection {
    state: SessionLiveState,
    outcome: octet_serve_backend::RunOutcome,
    message: Option<String>,
}

impl TerminalProjection {
    fn from_host_outcome(outcome: &HostRunOutcome) -> Self {
        match outcome {
            HostRunOutcome::Completed => Self::completed(),
            HostRunOutcome::Aborted | HostRunOutcome::Shutdown => Self::stopped(),
            HostRunOutcome::Failed(error) => Self::failed(error.clone()),
            HostRunOutcome::MaxTurns => Self::failed("The maximum model-turn limit was reached."),
            HostRunOutcome::StreamLost => Self::failed(crate::modes::RUN_STREAM_LOST_MESSAGE),
        }
    }

    fn completed() -> Self {
        Self {
            state: SessionLiveState::Done,
            outcome: octet_serve_backend::RunOutcome::Completed,
            message: None,
        }
    }

    fn stopped() -> Self {
        Self {
            state: SessionLiveState::Stopped,
            outcome: octet_serve_backend::RunOutcome::Stopped,
            message: None,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            state: SessionLiveState::Failed,
            outcome: octet_serve_backend::RunOutcome::Failed,
            message: Some(message.into()),
        }
    }
}

fn default_completion_review(
    outcome: octet_serve_backend::RunOutcome,
    duration_ms: u64,
    message: Option<&str>,
) -> CompletionReview {
    let summary = message.map_or_else(
        || match outcome {
            octet_serve_backend::RunOutcome::Completed => "Run completed.".into(),
            octet_serve_backend::RunOutcome::Stopped => "Run stopped.".into(),
            octet_serve_backend::RunOutcome::Failed => "Run failed.".into(),
        },
        |message| bounded_text(message, 2 * 1024),
    );
    CompletionReview {
        summary,
        duration_ms,
        action_count: 0,
        phases: Vec::new(),
        changed_file_item_ids: Vec::new(),
        verification_action_item_ids: Vec::new(),
        failed_action_item_ids: Vec::new(),
        warning_action_item_ids: Vec::new(),
        source_ids: Vec::new(),
        output_ids: Vec::new(),
        test_results: Vec::new(),
        evidence_coverage: EvidenceCoverage::None,
        open_questions: Vec::new(),
    }
}

fn build_completion_review(
    terminal: &TerminalProjection,
    started_at_ms: u64,
    completed_at_ms: u64,
    projection: &ProjectionState,
    changed_file_item_ids: BTreeSet<ItemId>,
    source_ids: BTreeSet<SourceId>,
    output_ids: BTreeSet<ArtifactId>,
) -> CompletionReview {
    let mut phases = BTreeMap::<ActivityPhase, ActivityPhaseSummary>::new();
    let mut verification_action_item_ids = Vec::new();
    let mut failed_action_item_ids = Vec::new();
    let mut warning_action_item_ids = Vec::new();
    let mut activities = projection
        .tool_items
        .iter()
        .filter_map(|(call_id, item_id)| {
            projection
                .tool_calls
                .get(call_id)
                .map(|tool| (item_id.clone(), tool.activity.clone()))
        })
        .collect::<Vec<_>>();
    activities.sort_by(|left, right| {
        left.1
            .started_at_ms
            .cmp(&right.1.started_at_ms)
            .then_with(|| left.0.as_str().cmp(right.0.as_str()))
    });
    for (item_id, activity) in &activities {
        let phase = phases
            .entry(activity.phase)
            .or_insert(ActivityPhaseSummary {
                phase: activity.phase,
                action_count: 0,
                succeeded_count: 0,
                failed_count: 0,
                stopped_count: 0,
            });
        phase.action_count = phase.action_count.saturating_add(1);
        match activity.status {
            ToolActivityStatus::Succeeded => {
                phase.succeeded_count = phase.succeeded_count.saturating_add(1)
            }
            ToolActivityStatus::Failed => {
                phase.failed_count = phase.failed_count.saturating_add(1);
                failed_action_item_ids.push(item_id.clone());
                if terminal.outcome == octet_serve_backend::RunOutcome::Completed {
                    warning_action_item_ids.push(item_id.clone());
                }
            }
            ToolActivityStatus::Stopped | ToolActivityStatus::Running => {
                phase.stopped_count = phase.stopped_count.saturating_add(1)
            }
        }
        if activity.phase == ActivityPhase::Verified {
            verification_action_item_ids.push(item_id.clone());
        }
    }
    let phase_summaries = phases.into_values().collect::<Vec<_>>();
    let changed_file_item_ids = changed_file_item_ids.into_iter().collect::<Vec<_>>();
    let source_ids = source_ids.into_iter().collect::<Vec<_>>();
    let output_ids = output_ids.into_iter().collect::<Vec<_>>();
    let action_count = activities.len().min(u32::MAX as usize) as u32;
    let has_unbounded_mutator = activities
        .iter()
        .any(|(_, activity)| matches!(activity.kind, ToolKind::Command | ToolKind::Other));
    let linked_evidence =
        !changed_file_item_ids.is_empty() || !source_ids.is_empty() || !output_ids.is_empty();
    let evidence_coverage = if has_unbounded_mutator {
        EvidenceCoverage::Partial
    } else if !linked_evidence {
        EvidenceCoverage::None
    } else if activities.iter().all(|(_, activity)| match activity.kind {
        ToolKind::Read | ToolKind::Web | ToolKind::Skill => !activity.source_ids.is_empty(),
        ToolKind::Edit | ToolKind::Write => {
            activity.status != ToolActivityStatus::Succeeded || !activity.changed_paths.is_empty()
        }
        ToolKind::Search => false,
        ToolKind::Command | ToolKind::Other => false,
    }) {
        EvidenceCoverage::Complete
    } else {
        EvidenceCoverage::Partial
    };
    let summary = format!(
        "{} {} action{}, {} changed file{}, {} verification{}, {} failure{}, {} warning{}, and {} output{}.",
        match terminal.outcome {
            octet_serve_backend::RunOutcome::Completed => "Completed",
            octet_serve_backend::RunOutcome::Stopped => "Stopped after",
            octet_serve_backend::RunOutcome::Failed => "Failed after",
        },
        action_count,
        if action_count == 1 { "" } else { "s" },
        changed_file_item_ids.len(),
        if changed_file_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        verification_action_item_ids.len(),
        if verification_action_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        failed_action_item_ids.len(),
        if failed_action_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        warning_action_item_ids.len(),
        if warning_action_item_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        output_ids.len(),
        if output_ids.len() == 1 { "" } else { "s" },
    );
    let summary = if projection.usage_uncertain {
        format!("Provider usage and cost are uncertain; numeric usage values are known subtotals, not complete totals. {summary}")
    } else {
        summary
    };
    CompletionReview {
        summary: bounded_text(&summary, 2 * 1024),
        duration_ms: completed_at_ms.saturating_sub(started_at_ms),
        action_count,
        phases: phase_summaries,
        changed_file_item_ids,
        verification_action_item_ids,
        failed_action_item_ids,
        warning_action_item_ids,
        source_ids,
        output_ids,
        test_results: projection.test_results.clone(),
        evidence_coverage,
        // The adapter cannot infer unresolved questions from prose safely.
        open_questions: Vec::new(),
    }
}

// Keep the immutable run identity, timing, projection, and review inputs explicit
// at the one durable serialization boundary.
#[allow(clippy::too_many_arguments)]
fn persist_run_projection(
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    run_id: &RunId,
    started_at_ms: u64,
    completed_at_ms: u64,
    projection: &ProjectionState,
    committed: &[SessionItem],
    review: &CompletionReview,
) -> Result<(), ServiceError> {
    let outcome_entry_id = committed
        .iter()
        .find_map(|item| {
            matches!(&item.payload, ItemPayload::RunOutcome { .. })
                .then(|| item.durable_entry_id.clone())
                .flatten()
        })
        .ok_or(ServiceError::Internal)?;
    let fallback_turn = projection.turn_id(run_id)?;
    let mut ordinals = HashMap::<String, u32>::new();
    let mut items = Vec::with_capacity(committed.len());
    for item in committed {
        let Some(durable_entry_id) = item.durable_entry_id.as_ref() else {
            continue;
        };
        let ordinal = ordinals
            .entry(durable_entry_id.as_str().to_owned())
            .or_default();
        items.push(StoredRunItemAttribution {
            durable_entry_id: durable_entry_id.as_str().to_owned(),
            ordinal: *ordinal,
            item_id: item.id.as_str().to_owned(),
            turn_id: item
                .turn_id
                .as_ref()
                .unwrap_or(&fallback_turn)
                .as_str()
                .to_owned(),
            user_delivery: match &item.payload {
                ItemPayload::UserMessage { delivery, .. } => *delivery,
                _ => None,
            },
            documents: match &item.payload {
                ItemPayload::UserMessage { documents, .. } => documents.clone(),
                _ => Vec::new(),
            },
            project_files: match &item.payload {
                ItemPayload::UserMessage { project_files, .. } => project_files.clone(),
                _ => Vec::new(),
            },
            branch_provenance: match &item.payload {
                ItemPayload::UserMessage {
                    branch_provenance, ..
                } => branch_provenance.clone(),
                _ => None,
            },
        });
        *ordinal = ordinal.saturating_add(1);
    }
    let mut tools = projection
        .tool_items
        .iter()
        .filter_map(|(tool_call_id, item_id)| {
            let tool = projection.tool_calls.get(tool_call_id)?;
            Some(StoredRunTool {
                tool_call_id: tool_call_id.clone(),
                item_id: item_id.as_str().to_owned(),
                turn_id: tool.turn_id.as_str().to_owned(),
                activity: tool.activity.clone(),
                result: tool.result.clone(),
            })
        })
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        left.activity
            .started_at_ms
            .cmp(&right.activity.started_at_ms)
            .then_with(|| left.item_id.cmp(&right.item_id))
    });
    let record = StoredRunRecord {
        version: STORED_RUN_RECORD_VERSION,
        session_id: session_id.as_str().to_owned(),
        run_id: run_id.as_str().to_owned(),
        outcome_entry_id: outcome_entry_id.as_str().to_owned(),
        started_at_ms,
        completed_at_ms,
        items,
        tools,
        review: review.clone(),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| ServiceError::Internal)?;
    resources
        .persist_run_record(session_id, &outcome_entry_id, &bytes)
        .map_err(resource_store_service_error)
}

fn load_stored_run_record(
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    outcome_entry_id: &DurableEntryId,
) -> Option<StoredRunRecord> {
    let bytes = resources.run_record(session_id, outcome_entry_id).ok()?;
    let record = serde_json::from_slice::<StoredRunRecord>(&bytes).ok()?;
    if record.version != STORED_RUN_RECORD_VERSION
        || record.session_id != session_id.as_str()
        || record.outcome_entry_id != outcome_entry_id.as_str()
        || record.started_at_ms == 0
        || record.completed_at_ms < record.started_at_ms
        || RunId::new(record.run_id.clone()).is_err()
        || record.review.validate().is_err()
    {
        return None;
    }
    for item in &record.items {
        if DurableEntryId::new(item.durable_entry_id.clone()).is_err()
            || ItemId::new(item.item_id.clone()).is_err()
            || TurnId::new(item.turn_id.clone()).is_err()
            || (ItemPayload::UserMessage {
                text: String::new(),
                attachments: Vec::new(),
                documents: item.documents.clone(),
                project_files: item.project_files.clone(),
                delivery: item.user_delivery,
                branch_provenance: item.branch_provenance.clone(),
            })
            .validate()
            .is_err()
        {
            return None;
        }
    }
    for tool in &record.tools {
        if tool.tool_call_id.len() > 512
            || ItemId::new(tool.item_id.clone()).is_err()
            || TurnId::new(tool.turn_id.clone()).is_err()
            || tool.activity.validate().is_err()
            || tool
                .result
                .as_ref()
                .is_some_and(|result| result.validate().is_err())
        {
            return None;
        }
    }
    Some(record)
}

async fn expire_private_requests(
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
    actor_generation: u64,
) -> Result<(), ServiceError> {
    for (id, request) in projection.private_requests.drain() {
        match request.response {
            PrivateResponse::Approval(respond) => respond(false),
            PrivateResponse::Input(respond) => respond(None),
        }
        events
            .send(event(EventPayload::PendingRequestChanged {
                request: PendingRequest {
                    id,
                    actor_generation,
                    kind: request.kind,
                    state: RequestState::Expired,
                },
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    projection.tool_items.clear();
    projection.tool_calls.clear();
    projection.tool_progress.clear();
    Ok(())
}

async fn retract_attempt(
    _run_id: &RunId,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    for item_id in [
        projection.assistant_item.take(),
        projection.reasoning_item.take(),
    ]
    .into_iter()
    .flatten()
    {
        events
            .send(event(EventPayload::ItemRetracted {
                item_id,
                provider_attempt: projection.provider_attempt,
                reason: "The provider attempt was replaced before commit.".into(),
            }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
    }
    Ok(())
}

fn approval_action(prompt: &str, detail: Option<&str>) -> String {
    let mut action = prompt.to_owned();
    if let Some(detail) = detail {
        action.push_str("\n\n");
        action.push_str(detail);
    }
    action
}

async fn project_tool_progress(
    id: ToolCallId,
    progress: ToolProgress,
    run_id: &RunId,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    match progress {
        ToolProgress::Output { bytes, .. } => {
            let entry = projection.tool_progress.entry(id.0.clone()).or_default();
            entry.observed_output_bytes = entry
                .observed_output_bytes
                .saturating_add(bytes.len() as u64);
            publish_tool_progress(&id.0, projection, events).await?;
        }
        ToolProgress::Status(_) | ToolProgress::Decoration(_) => {}
        ToolProgress::Dropped { bytes, .. } => {
            let entry = projection.tool_progress.entry(id.0.clone()).or_default();
            entry.dropped_output_bytes = entry.dropped_output_bytes.saturating_add(bytes);
            publish_tool_progress(&id.0, projection, events).await?;
        }
        ToolProgress::Confirmation(request) => {
            projection.request_counter = projection.request_counter.saturating_add(1);
            let request_id = RequestId::new(format!(
                "request-{}-{}",
                run_id.as_str(),
                projection.request_counter
            ))
            .map_err(|_| ServiceError::Internal)?;
            let action = approval_action(&request.prompt, request.detail.as_deref());
            let pending = PendingRequest {
                id: request_id.clone(),
                actor_generation: projection_actor_generation(run_id),
                kind: RequestKind::Approval {
                    action: bounded_text(&action, 8 * 1024),
                    item_id: projection.tool_items.get(&id.0).cloned(),
                },
                state: RequestState::Pending,
            };
            let kind = pending.kind.clone();
            projection.private_requests.insert(
                request_id,
                PrivateRequest {
                    kind,
                    response: PrivateResponse::Approval(Box::new(move |allowed| {
                        request.respond(allowed);
                    })),
                },
            );
            events
                .send(event(EventPayload::PendingRequestChanged {
                    request: pending,
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            events
                .send(event(EventPayload::SessionStateChanged {
                    state: SessionLiveState::NeedsApproval,
                    active_run_id: Some(run_id.clone()),
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
        }
        ToolProgress::Input(request) => {
            projection.request_counter = projection.request_counter.saturating_add(1);
            let request_id = RequestId::new(format!(
                "request-{}-{}",
                run_id.as_str(),
                projection.request_counter
            ))
            .map_err(|_| ServiceError::Internal)?;
            let pending = PendingRequest {
                id: request_id.clone(),
                actor_generation: projection_actor_generation(run_id),
                kind: RequestKind::UserInput {
                    // The extension-owned prompt is private tool progress. Do
                    // not forward it verbatim across the public boundary.
                    prompt: "A tool needs additional input to continue.".into(),
                    choices: Vec::new(),
                },
                state: RequestState::Pending,
            };
            let kind = pending.kind.clone();
            projection.private_requests.insert(
                request_id,
                PrivateRequest {
                    kind,
                    response: PrivateResponse::Input(Box::new(move |answer| match answer {
                        Some(answer) => request.respond(answer),
                        None => request.cancel(),
                    })),
                },
            );
            events
                .send(event(EventPayload::PendingRequestChanged {
                    request: pending,
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            events
                .send(event(EventPayload::SessionStateChanged {
                    state: SessionLiveState::NeedsInput,
                    active_run_id: Some(run_id.clone()),
                }))
                .await
                .map_err(|_| ServiceError::Unavailable)?;
        }
        ToolProgress::SessionEvent(_, _) => {}
    }
    Ok(())
}

async fn publish_tool_progress(
    tool_call_id: &str,
    projection: &ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let Some(item_id) = projection.tool_items.get(tool_call_id).cloned() else {
        return Ok(());
    };
    let Some(tool) = projection.tool_calls.get(tool_call_id) else {
        return Ok(());
    };
    let progress = projection
        .tool_progress
        .get(tool_call_id)
        .cloned()
        .unwrap_or_default();
    let mut activity = tool.activity.clone();
    activity.observed_output_bytes = progress.observed_output_bytes;
    activity.dropped_output_bytes = progress.dropped_output_bytes;
    events
        .send(event(EventPayload::ItemDelta {
            item_id,
            delta: ItemDelta::ToolActivity { activity },
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

fn is_local_synthetic_assistant(entry: &Entry) -> bool {
    entry
        .metadata
        .as_ref()
        .is_some_and(|metadata| metadata.local_synthetic_assistant)
}

fn project_new_entries(
    session: &Session,
    workspace: &Path,
    projection: &mut ProjectionState,
    run_id: Option<&RunId>,
    completion_review: Option<&CompletionReview>,
    attachment_store: Option<&AttachmentStore>,
    session_id: &SessionId,
) -> Result<Vec<SessionItem>, ServiceError> {
    let entries = session.entries();
    let start = projection.known_entries.min(entries.len());
    let mut items = Vec::new();
    for entry in &entries[start..] {
        if is_local_synthetic_assistant(entry) {
            continue;
        }
        let attachments = attachment_refs_for_entry(
            entry,
            attachment_store,
            session_id,
            &mut projection.pending_attachments,
        )?;
        let (
            preferred,
            user_delivery,
            preferred_reasoning,
            resolved_documents,
            resolved_project_files,
            branch_provenance,
        ) = match &entry.value {
            EntryValue::Message(Message::User(message))
                if message
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::Text(_) | UserPart::Media(_))) =>
            {
                match projection.pending_user_items.pop_front() {
                    Some(pending) => {
                        projection
                            .item_turns
                            .insert(pending.id.clone(), pending.turn_id);
                        (
                            Some(pending.id),
                            Some(pending.delivery),
                            None,
                            pending.documents,
                            pending.project_files,
                            pending.branch_provenance,
                        )
                    }
                    None => (None, None, None, Vec::new(), Vec::new(), None),
                }
            }
            EntryValue::Message(Message::Assistant(_)) => (
                projection
                    .completed_assistant_items
                    .pop_front()
                    .flatten()
                    .map(|(item_id, turn_id)| {
                        projection.item_turns.insert(item_id.clone(), turn_id);
                        item_id
                    }),
                None,
                projection
                    .completed_reasoning_items
                    .pop_front()
                    .flatten()
                    .map(|(item_id, turn_id)| {
                        projection.item_turns.insert(item_id.clone(), turn_id);
                        item_id
                    }),
                Vec::new(),
                Vec::new(),
                None,
            ),
            _ => (None, None, None, Vec::new(), Vec::new(), None),
        };
        let mut projected = project_entry(
            entry,
            workspace,
            run_id.cloned(),
            preferred,
            user_delivery,
            preferred_reasoning,
            &mut projection.tool_items,
            &mut projection.tool_calls,
            completion_review,
            attachments,
        )?;
        if let Some(user_item) = projected
            .iter_mut()
            .find(|item| matches!(item.payload, ItemPayload::UserMessage { .. }))
        {
            if let ItemPayload::UserMessage {
                documents,
                project_files,
                branch_provenance: projected_provenance,
                ..
            } = &mut user_item.payload
            {
                *documents = resolved_documents;
                *project_files = resolved_project_files;
                *projected_provenance = branch_provenance;
            }
        }
        for item in &mut projected {
            let turn_id =
                projection
                    .item_turns
                    .get(&item.id)
                    .cloned()
                    .or_else(|| match &item.payload {
                        ItemPayload::ToolCall(_) => {
                            projection.tool_items.iter().find_map(|(call_id, item_id)| {
                                if item_id == &item.id {
                                    projection
                                        .tool_calls
                                        .get(call_id)
                                        .map(|tool| tool.turn_id.clone())
                                } else {
                                    None
                                }
                            })
                        }
                        ItemPayload::ToolResult(result) => projection
                            .item_turns
                            .get(&result.tool_call_item_id)
                            .cloned(),
                        _ => run_id.and_then(|run_id| projection.turn_id(run_id).ok()),
                    });
            item.turn_id = turn_id;
        }
        items.extend(projected);
    }
    projection.known_entries = entries.len();
    Ok(items)
}

// Entry projection has several independent identity hints and output indexes;
// keeping them explicit avoids an ambiguous partially populated parameter bag.
#[allow(clippy::too_many_arguments)]
fn project_entry(
    entry: &Entry,
    workspace: &Path,
    run_id: Option<RunId>,
    preferred: Option<ItemId>,
    user_delivery: Option<UserMessageDelivery>,
    preferred_reasoning: Option<ItemId>,
    tool_items: &mut HashMap<String, ItemId>,
    tool_calls: &mut HashMap<String, ProjectedToolCall>,
    completion_review: Option<&CompletionReview>,
    attachments: Vec<AttachmentRef>,
) -> Result<Vec<SessionItem>, ServiceError> {
    let durable_id =
        DurableEntryId::new(entry.id.0.clone()).map_err(|_| ServiceError::InvalidSeed)?;
    let mut items = Vec::new();
    match &entry.value {
        EntryValue::Message(Message::User(message)) => {
            let mut user_text = Vec::new();
            for part in &message.content {
                match part {
                    UserPart::Text(text) => user_text.push(text.as_str()),
                    UserPart::Media(_) => {}
                    UserPart::ToolResult(result) => {
                        let content = result
                            .content
                            .iter()
                            .map(|part| match part {
                                ToolResultPart::Text(text) => text.as_str(),
                                ToolResultPart::Media(_) => "[media output]",
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let tool_call_item_id = tool_items
                            .get(&result.tool_call_id.0)
                            .cloned()
                            .unwrap_or(stable_tool_item_id(&result.tool_call_id.0)?);
                        let durable_result = if result.is_error {
                            Err(ToolError::new(content.clone()))
                        } else {
                            Ok(ToolOutput::new(content.clone()))
                        };
                        let semantic_result = if let Some(mut tool) =
                            tool_calls.get(&result.tool_call_id.0).cloned()
                        {
                            if let Some(summary) = tool.result.clone() {
                                summary
                            } else {
                                let (activity, mut summary) = complete_tool_activity(
                                    tool.activity,
                                    &tool.name,
                                    &durable_result,
                                    1,
                                    ProjectedToolProgress::default(),
                                );
                                summary.tool_call_item_id = tool_call_item_id.clone();
                                tool.activity = activity;
                                tool.result = Some(summary.clone());
                                tool_calls.insert(result.tool_call_id.0.clone(), tool);
                                summary
                            }
                        } else {
                            let fallback_turn = TurnId::new("turn-history")
                                .map_err(|_| ServiceError::InvalidSeed)?;
                            let mut fallback = ProjectedToolCall {
                                name: "tool".into(),
                                arguments: serde_json::Value::Null,
                                activity: semantic_tool_activity(
                                    "tool",
                                    &serde_json::Value::Null,
                                    workspace,
                                    1,
                                ),
                                result: None,
                                turn_id: fallback_turn,
                            };
                            let (activity, mut summary) = complete_tool_activity(
                                fallback.activity,
                                &fallback.name,
                                &durable_result,
                                1,
                                ProjectedToolProgress::default(),
                            );
                            summary.tool_call_item_id = tool_call_item_id.clone();
                            fallback.activity = activity;
                            fallback.result = Some(summary.clone());
                            tool_calls.insert(result.tool_call_id.0.clone(), fallback);
                            summary
                        };
                        items.push(committed_item(
                            item_id_for_entry(entry, items.len())?,
                            run_id.clone(),
                            durable_id.clone(),
                            ItemPayload::ToolResult(ToolResultSummary {
                                tool_call_item_id,
                                ..semantic_result
                            }),
                        ));
                    }
                }
            }
            if !user_text.is_empty()
                || message
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::Media(_)))
            {
                let visible = entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.display_text.as_deref())
                    .unwrap_or_else(|| user_text.first().copied().unwrap_or(""));
                let text = if entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.display_text.as_ref())
                    .is_some()
                {
                    visible.to_owned()
                } else {
                    user_text.join("\n")
                };
                items.insert(
                    0,
                    committed_item(
                        preferred.unwrap_or(item_id_for_entry(entry, items.len())?),
                        run_id,
                        durable_id,
                        ItemPayload::UserMessage {
                            text: bounded_text(&text, MAX_PROMPT_BYTES),
                            attachments,
                            documents: Vec::new(),
                            project_files: Vec::new(),
                            delivery: user_delivery,
                            branch_provenance: None,
                        },
                    ),
                );
            }
        }
        EntryValue::Message(Message::Assistant(message)) => {
            let text = message
                .content
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                items.push(committed_item(
                    preferred.unwrap_or(item_id_for_entry(entry, items.len())?),
                    run_id.clone(),
                    durable_id.clone(),
                    ItemPayload::AssistantMessage {
                        text: bounded_text(&text, MAX_ITEM_TEXT_BYTES),
                    },
                ));
            }
            let reasoning = message
                .content
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Reasoning(reasoning) => reasoning.text.as_deref(),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if !reasoning.is_empty() {
                items.push(committed_item(
                    preferred_reasoning.unwrap_or(item_id_for_entry(entry, items.len())?),
                    run_id.clone(),
                    durable_id.clone(),
                    ItemPayload::Reasoning {
                        text: bounded_text(&reasoning, MAX_ITEM_TEXT_BYTES),
                    },
                ));
            }
            for call in message.content.iter().filter_map(|part| match part {
                AssistantPart::ToolCall(call) => Some(call),
                _ => None,
            }) {
                let arguments = call.arguments_value().unwrap_or(serde_json::Value::Null);
                let arguments =
                    if octet_serve_backend::validate_json("tool.arguments", &arguments, 256 * 1024)
                        .is_ok()
                    {
                        arguments
                    } else {
                        serde_json::Value::Null
                    };
                let item_id = tool_items
                    .get(&call.id.0)
                    .cloned()
                    .unwrap_or(stable_tool_item_id(&call.id.0)?);
                tool_items.insert(call.id.0.clone(), item_id.clone());
                let projected =
                    tool_calls
                        .entry(call.id.0.clone())
                        .or_insert_with(|| ProjectedToolCall {
                            name: call.name.clone(),
                            arguments: arguments.clone(),
                            activity: semantic_tool_activity(&call.name, &arguments, workspace, 1),
                            result: None,
                            turn_id: TurnId::new("turn-history")
                                .expect("static historical turn ID is valid"),
                        });
                items.push(committed_item(
                    item_id,
                    run_id.clone(),
                    durable_id.clone(),
                    ItemPayload::ToolCall(projected.activity.clone()),
                ));
            }
        }
        EntryValue::Compaction { summary, .. } => {
            items.push(committed_item(
                item_id_for_entry(entry, 0)?,
                run_id,
                durable_id,
                ItemPayload::Compaction {
                    reason: bounded_text(summary, 4 * 1024),
                },
            ));
        }
        EntryValue::Config { .. } => {
            if let Some(outcome) = entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.run_outcome.as_ref())
            {
                let outcome = match outcome.status {
                    SessionRunOutcomeStatus::Completed => {
                        octet_serve_backend::RunOutcome::Completed
                    }
                    SessionRunOutcomeStatus::Stopped => octet_serve_backend::RunOutcome::Stopped,
                    SessionRunOutcomeStatus::Failed => octet_serve_backend::RunOutcome::Failed,
                };
                let message = entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.run_outcome.as_ref())
                    .and_then(|outcome| outcome.message.as_deref())
                    .map(|message| bounded_text(message, 8 * 1024));
                items.push(committed_item(
                    item_id_for_entry(entry, 0)?,
                    run_id,
                    durable_id,
                    ItemPayload::RunOutcome {
                        outcome,
                        review: completion_review.cloned().unwrap_or_else(|| {
                            default_completion_review(outcome, 0, message.as_deref())
                        }),
                        message,
                    },
                ));
            }
        }
        EntryValue::ResponsesTurn { .. }
        | EntryValue::ResponsesCompaction { .. }
        | EntryValue::PromptTemplateSelected { .. }
        | EntryValue::SkillActivated { .. }
        | EntryValue::SkillResourceRead { .. }
        | EntryValue::ResponsesSteering { .. }
        | EntryValue::ResponsesReasoning { .. }
        | EntryValue::SkillDeactivated { .. } => {}
    }
    Ok(items)
}

fn committed_item(
    id: ItemId,
    run_id: Option<RunId>,
    durable_entry_id: DurableEntryId,
    payload: ItemPayload,
) -> SessionItem {
    SessionItem {
        id,
        run_id,
        turn_id: None,
        provider_attempt: None,
        lifecycle: ItemLifecycle::Committed,
        durable_entry_id: Some(durable_entry_id),
        payload,
    }
}

fn item_id_for_entry(entry: &Entry, part: usize) -> Result<ItemId, ServiceError> {
    ItemId::new(format!("item-entry-{}-part-{part}", entry.id.0))
        .map_err(|_| ServiceError::InvalidSeed)
}

fn rehydrate_stored_evidence(
    resources: &octet_serve_backend::ResourceStore,
    session: &Session,
    session_id: &SessionId,
    result_entry: &Entry,
    active_entry_ids: &std::collections::BTreeSet<&str>,
    tool_items: &HashMap<String, ItemId>,
) -> Option<EvidenceProjection> {
    let durable_result_id = DurableEntryId::new(result_entry.id.0.clone()).ok()?;
    let bytes = resources.record(session_id, &durable_result_id).ok()?;
    let record = serde_json::from_slice::<StoredToolEvidence>(&bytes).ok()?;
    if !matches!(record.version, 1 | STORED_EVIDENCE_VERSION)
        || record.session_id != session_id.as_str()
        || record.result_entry_id != result_entry.id.0
        || !active_entry_ids.contains(record.call_entry_id.as_str())
        || !result_entry_has_successful_tool_result(result_entry, &record.tool_call_id)
    {
        return None;
    }
    let call_entry = session.entry(&EntryId(record.call_entry_id.clone()))?;
    if !entry_has_tool_call(call_entry, &record.tool_call_id) {
        return None;
    }
    project_stored_evidence(
        resources,
        session_id,
        &record,
        None,
        None,
        tool_items.get(&record.tool_call_id).cloned(),
    )
    .ok()
}

fn result_entry_has_successful_tool_result(entry: &Entry, tool_call_id: &str) -> bool {
    matches!(
        &entry.value,
        EntryValue::Message(Message::User(message))
            if message.content.iter().any(|part| {
                matches!(
                    part,
                    UserPart::ToolResult(result)
                        if result.tool_call_id.0 == tool_call_id && !result.is_error
                )
            })
    )
}

fn entry_has_tool_call(entry: &Entry, tool_call_id: &str) -> bool {
    matches!(
        &entry.value,
        EntryValue::Message(Message::Assistant(message))
            if message.content.iter().any(|part| {
                matches!(
                    part,
                    AssistantPart::ToolCall(call) if call.id.0 == tool_call_id
                )
            })
    )
}

fn graphical_input_pricing(pricing: Option<&octet_ai::Pricing>) -> Option<ModelInputPricing> {
    pricing.map(|pricing| ModelInputPricing {
        base_microdollars_per_million_tokens: pricing.input.0,
        tiers: pricing
            .tiers
            .iter()
            .filter_map(|tier| {
                tier.input.map(|rate| ModelInputPricingTier {
                    min_input_tokens: tier.min_input_tokens,
                    microdollars_per_million_tokens: rate.0,
                })
            })
            .take(MAX_MODEL_INPUT_PRICING_TIERS)
            .collect(),
    })
}

fn graphical_model_catalog(catalog: &ModelCatalog, config: &Config) -> Vec<ModelSummary> {
    let subagents_available = subagents_extension_activation_configured(config);
    let models = catalog
        .models()
        .filter_map(|spec| catalog.resolve(&spec.id).ok())
        .map(|model| {
            let reasoning = supported_levels_with_subagents(&model, subagents_available)
                .into_iter()
                .map(thinking_label)
                .collect::<Vec<_>>();
            let preference = config
                .reasoning
                .clone()
                .unwrap_or_else(|| crate::app::default_reasoning_for_model(&model));
            let requested_default = selection_for_model(&model, &preference, config).reasoning;
            let default_reasoning = reasoning
                .iter()
                .find(|choice| choice.as_str() == requested_default.as_str())
                .cloned()
                .or_else(|| reasoning.first().cloned());
            let mut input_modalities = vec![InputModality::Text];
            if model
                .spec
                .capabilities
                .input_modalities
                .contains(Modality::Image)
            {
                input_modalities.push(InputModality::Image);
            }
            if model
                .spec
                .capabilities
                .input_modalities
                .contains(Modality::Audio)
            {
                input_modalities.push(InputModality::Audio);
            }
            ModelSummary {
                id: model.spec.id.0.clone(),
                name: model
                    .spec
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.spec.id.0.clone()),
                provider: model.endpoint.id.0.clone(),
                local: model
                    .endpoint
                    .base_url
                    .host_str()
                    .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1")),
                available: true,
                reasoning,
                default_reasoning,
                input_pricing: graphical_input_pricing(model.spec.pricing.as_ref()),
                input_modalities,
            }
        })
        .collect();
    bound_graphical_models(models, config.model.as_ref())
}

fn bound_graphical_models(
    mut models: Vec<ModelSummary>,
    configured_model: Option<&ModelId>,
) -> Vec<ModelSummary> {
    let compare = |left: &ModelSummary, right: &ModelSummary| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    };
    models.sort_by(compare);
    if models.len() <= MAX_GRAPHICAL_MODELS {
        return models;
    }

    let configured = configured_model.and_then(|configured| {
        models
            .iter()
            .position(|summary| summary.id == configured.0)
            .filter(|index| *index >= MAX_GRAPHICAL_MODELS)
            .map(|index| models.remove(index))
    });
    models.truncate(MAX_GRAPHICAL_MODELS - usize::from(configured.is_some()));
    if let Some(configured) = configured {
        models.push(configured);
        models.sort_by(compare);
    }
    models
}

fn selection_from_summary(summary: &ModelSummary) -> ModelSelection {
    ModelSelection {
        provider: summary.provider.clone(),
        model: summary.id.clone(),
        reasoning: summary
            .default_reasoning
            .clone()
            .or_else(|| summary.reasoning.first().cloned())
            .unwrap_or_else(|| "off".into()),
    }
}

fn selection_for_model(
    model: &Model,
    reasoning: &ReasoningConfig,
    config: &Config,
) -> ModelSelection {
    let normalized = crate::app::normalize_reasoning_selection_for_model_with_subagents(
        reasoning,
        octet_ai::ReasoningMode::Standard,
        model,
        subagents_extension_activation_configured(config),
    )
    .map(|(reasoning, _, _)| reasoning)
    .unwrap_or(ReasoningConfig::Off);
    let portable = crate::app::level_from_reasoning(&normalized, model)
        .map(thinking_label)
        .unwrap_or_else(|_| reasoning_label(&normalized));
    let choices =
        supported_levels_with_subagents(model, subagents_extension_activation_configured(config))
            .into_iter()
            .map(thinking_label)
            .collect::<Vec<_>>();
    let portable = choices
        .iter()
        .find(|choice| choice.as_str() == portable.as_str())
        .cloned()
        .or_else(|| choices.first().cloned())
        .unwrap_or_else(|| "off".into());
    let _ = config;
    ModelSelection {
        provider: model.endpoint.id.0.clone(),
        model: model.spec.id.0.clone(),
        reasoning: portable,
    }
}

fn selection_from_session(
    session: &Session,
    catalog: &ModelCatalog,
    config: &Config,
) -> Result<ModelSelection, ServiceError> {
    let mut model = None;
    let mut reasoning = None;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id).ok_or(ServiceError::InvalidSeed)?;
        if let EntryValue::Config {
            model: persisted_model,
            reasoning: persisted_reasoning,
            ..
        } = &entry.value
        {
            if model.is_none() {
                model = persisted_model.clone();
            }
            if reasoning.is_none() {
                reasoning = persisted_reasoning.clone();
            }
            if model.is_some() && reasoning.is_some() {
                break;
            }
        }
        cursor = entry.parent.as_ref();
    }
    selection_from_persisted_config(model, reasoning, catalog, config)
}

fn selection_from_catalog_entry(
    entry: &SessionCatalogEntry,
    catalog: &ModelCatalog,
    config: &Config,
) -> Result<ModelSelection, ServiceError> {
    selection_from_persisted_config(
        entry.configured_model.clone(),
        entry.configured_reasoning.clone(),
        catalog,
        config,
    )
}

fn selection_from_persisted_config(
    model: Option<String>,
    reasoning: Option<String>,
    catalog: &ModelCatalog,
    config: &Config,
) -> Result<ModelSelection, ServiceError> {
    let model_id = model
        .map(ModelId)
        .or_else(|| config.model.clone())
        .ok_or(ServiceError::InvalidSeed)?;
    let model = catalog
        .resolve(&model_id)
        .map_err(|_| ServiceError::InvalidSeed)?;
    let reasoning = reasoning
        .as_deref()
        .map(config::parse_reasoning)
        .transpose()
        .map_err(|_| ServiceError::InvalidSeed)?
        .or_else(|| config.reasoning.clone())
        .unwrap_or_else(|| crate::app::default_reasoning_for_model(&model));
    Ok(selection_for_model(&model, &reasoning, config))
}

fn advertised_selection_from_session(
    session: &Session,
    catalog: &ModelCatalog,
    config: &Config,
    models: &[ModelSummary],
) -> Option<ModelSelection> {
    selection_from_session(session, catalog, config)
        .ok()
        .filter(|selection| {
            models
                .iter()
                .any(|model| model.provider == selection.provider && model.id == selection.model)
        })
}

fn advertised_selection_from_catalog_entry(
    entry: &SessionCatalogEntry,
    catalog: &ModelCatalog,
    config: &Config,
    models: &[ModelSummary],
) -> Option<ModelSelection> {
    selection_from_catalog_entry(entry, catalog, config)
        .ok()
        .filter(|selection| {
            models
                .iter()
                .any(|model| model.provider == selection.provider && model.id == selection.model)
        })
}

#[cfg(test)]
fn current_selection(plan: &WorkerPlan) -> ModelSelection {
    let summary = plan
        .available_models
        .iter()
        .find(|summary| summary.id == plan.launch.model.0);
    match summary {
        Some(summary) => {
            let projected = reasoning_label(&plan.launch.reasoning);
            ModelSelection {
                provider: summary.provider.clone(),
                model: summary.id.clone(),
                reasoning: summary
                    .reasoning
                    .iter()
                    .find(|choice| choice.as_str() == projected.as_str())
                    .cloned()
                    .or_else(|| summary.default_reasoning.clone())
                    .or_else(|| summary.reasoning.first().cloned())
                    .unwrap_or_else(|| "off".into()),
            }
        }
        None => ModelSelection {
            provider: "unknown".into(),
            model: plan.launch.model.0.clone(),
            reasoning: "off".into(),
        },
    }
}

fn thinking_label(level: crate::config::ThinkingLevel) -> String {
    match level {
        crate::config::ThinkingLevel::Off => "off",
        crate::config::ThinkingLevel::On => "on",
        crate::config::ThinkingLevel::Minimal => "minimal",
        crate::config::ThinkingLevel::Low => "low",
        crate::config::ThinkingLevel::Medium => "medium",
        crate::config::ThinkingLevel::High => "high",
        crate::config::ThinkingLevel::Xhigh => "xhigh",
        crate::config::ThinkingLevel::Max => "max",
        crate::config::ThinkingLevel::Ultra => "ultra",
    }
    .into()
}

fn graphical_themes(config: &Config) -> anyhow::Result<(Vec<ThemeOption>, ThemeId)> {
    const MAX_GRAPHICAL_THEMES: usize = 64;

    let selected_name = crate::tui::theme::DEFAULT_THEME_NAME.to_owned();
    let mut names = crate::tui::theme::available_themes(config);
    names.retain(|name| name != &selected_name);
    names.insert(0, selected_name.clone());

    let mut themes = Vec::new();
    for name in names.into_iter().take(MAX_GRAPHICAL_THEMES) {
        let Ok(theme) = crate::tui::theme::load_named_theme(&name, config) else {
            continue;
        };
        themes.push(graphical_theme_option(&name, &theme, config)?);
    }
    if themes.is_empty() {
        let theme = crate::tui::theme::load_theme(config);
        themes.push(graphical_theme_option(&selected_name, &theme, config)?);
    }
    let selected_theme_id = graphical_theme_id(&selected_name)?;
    if !themes.iter().any(|theme| theme.id == selected_theme_id) {
        anyhow::bail!("selected graphical theme was not projected");
    }
    Ok((themes, selected_theme_id))
}

fn graphical_theme_id(name: &str) -> anyhow::Result<ThemeId> {
    ThemeId::new(format!("theme-{}", &stable_hash(name.as_bytes())[..24]))
        .map_err(anyhow::Error::msg)
}

fn graphical_theme_option(
    name: &str,
    theme: &crate::tui::theme::OctetTheme,
    config: &Config,
) -> anyhow::Result<ThemeOption> {
    const BUILT_IN_ROLES: &[&str] = &[
        "text",
        "muted",
        "subtle",
        "accent",
        "success",
        "warning",
        "error",
        "heading",
        "emphasis",
        "strong",
        "inline_code",
        "code",
        "quote",
        "border",
        "link",
        "list_marker",
        "diff_add",
        "diff_remove",
        "diff_context",
        "diff_hunk",
        "diff_header",
        "syntax_comment",
        "syntax_keyword",
        "syntax_function",
        "syntax_variable",
        "syntax_string",
        "syntax_number",
        "syntax_type",
        "syntax_operator",
        "syntax_punctuation",
    ];

    let mut role_names = BUILT_IN_ROLES
        .iter()
        .map(|role| (*role).to_owned())
        .collect::<Vec<_>>();
    role_names.extend(theme.semantic_role_names().map(str::to_owned));
    role_names.sort();
    role_names.dedup();
    // Each role may contribute one foreground and one background token.
    role_names.truncate(128);

    let mut colors = BTreeMap::new();
    let mut roles = BTreeMap::new();
    for (index, role_name) in role_names.into_iter().enumerate() {
        let Ok(role) = SemanticRole::new(role_name.clone()) else {
            continue;
        };
        let style = theme.semantic_style(&role_name);
        let foreground = graphical_color_token(&mut colors, index, "foreground", style.foreground);
        let background = graphical_color_token(&mut colors, index, "background", style.background);
        roles.insert(role, graphical_role_style(style, foreground, background));
    }

    let source = match theme.source() {
        crate::tui::theme::ThemeSource::CompiledDefault
        | crate::tui::theme::ThemeSource::CompiledCards
        | crate::tui::theme::ThemeSource::CompiledStill => ThemeSourceClass::Bundled,
        crate::tui::theme::ThemeSource::File(path) if path.starts_with(&config.workspace) => {
            ThemeSourceClass::Project
        }
        crate::tui::theme::ThemeSource::File(_) => ThemeSourceClass::Global,
    };
    let scheme = match theme.background() {
        crate::tui::theme::TerminalBackground::Dark => ColorScheme::Dark,
        crate::tui::theme::TerminalBackground::Light => ColorScheme::Light,
        crate::tui::theme::TerminalBackground::Unknown => ColorScheme::Unknown,
    };
    let density = match theme.layout().density {
        crate::tui::theme::ThemeDensity::Compact => ThemeDensity::Compact,
        crate::tui::theme::ThemeDensity::Comfortable => ThemeDensity::Comfortable,
        crate::tui::theme::ThemeDensity::Airy => ThemeDensity::Airy,
    };
    let display_name = if theme.metadata().name.trim().is_empty() {
        name
    } else {
        &theme.metadata().name
    };
    let option = ThemeOption {
        id: graphical_theme_id(name)?,
        theme: ThemeDto {
            name: bounded_text(display_name, 128),
            source,
            revision: 1,
            scheme,
            density,
            motion: ThemeMotion::Full,
            typography: ThemeTypography {
                body_family: "system-ui".into(),
                mono_family: "ui-monospace".into(),
                body_size: 17,
                display_ratio_milli: 1235,
            },
            colors,
            roles,
        },
    };
    option.validate().map_err(anyhow::Error::msg)?;
    Ok(option)
}

fn graphical_color_token(
    colors: &mut BTreeMap<String, ThemeColor>,
    index: usize,
    channel: &str,
    color: TuiColor,
) -> Option<String> {
    let projected = match color {
        TuiColor::Default => return Some("default".into()),
        TuiColor::Ansi16(index) | TuiColor::Indexed(index) => ThemeColor::Ansi { index },
        TuiColor::Rgb(red, green, blue) => ThemeColor::Rgb { red, green, blue },
    };
    let token = format!("role.{index}.{channel}");
    colors.insert(token.clone(), projected);
    Some(token)
}

fn graphical_role_style(
    style: TuiTextStyle,
    mut foreground: Option<String>,
    mut background: Option<String>,
) -> ThemeRoleStyle {
    if style.attributes.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    ThemeRoleStyle {
        foreground,
        background,
        bold: style.attributes.bold,
        dim: style.attributes.dim,
        italic: style.attributes.italic,
        underline: style.attributes.underline,
        strikethrough: style.attributes.strikethrough,
    }
}

#[cfg(test)]
fn session_meta_for_id(store: &SessionStore, session_id: &SessionId) -> Option<SessionMeta> {
    store.get_by_id(session_id.as_str()).ok().flatten()
}

fn session_meta_for_open_session(
    store: &SessionStore,
    session_id: &SessionId,
    session: &Session,
) -> Option<SessionMeta> {
    store
        .meta_for_open_session(session_id.as_str(), session)
        .ok()
        .flatten()
}

#[cfg(test)]
fn changed_session_title(
    store: &SessionStore,
    session_id: &SessionId,
    previous: Option<&str>,
) -> Option<String> {
    let title = session_meta_for_id(store, session_id)?.title;
    (title != "(empty session)" && !title.trim().is_empty() && previous != Some(title.as_str()))
        .then_some(title)
}

fn summary_from_meta(
    meta: &SessionMeta,
    project_id: Option<ProjectId>,
    model: ModelSelection,
) -> Result<SessionSummary, ServiceError> {
    let id = SessionId::new(meta.id.clone()).map_err(|_| ServiceError::InvalidSeed)?;
    let modified_at_ms = system_time_ms(meta.modified);
    let (lifecycle, retention, forked_from) = session_catalog_metadata(meta, &id)?;
    Ok(SessionSummary {
        id,
        project_id,
        title: bounded_text(&meta.title, 512),
        tags: meta.tags.iter().map(|tag| bounded_text(tag, 64)).collect(),
        created_at_ms: modified_at_ms,
        modified_at_ms,
        pinned: meta.pinned,
        archived: meta.archived,
        lifecycle,
        retention,
        forked_from,
        provisional: false,
        live_state: SessionLiveState::Idle,
        attention: AttentionState::None,
        pull_request: None,
        owner: ActorOwnerState::Inactive,
        model,
    })
}

fn session_catalog_metadata(
    meta: &SessionMeta,
    session_id: &SessionId,
) -> Result<
    (
        SessionCatalogState,
        Option<SessionRetention>,
        Option<ConversationBranchProvenance>,
    ),
    ServiceError,
> {
    let (lifecycle, retention) = match (meta.trashed_at_ms, meta.purge_after_ms) {
        (Some(trashed_at_ms), Some(purge_after_ms)) => (
            SessionCatalogState::Trash,
            Some(SessionRetention {
                trashed_at_ms,
                purge_after_ms,
                permanent_delete_requires_confirmation: true,
            }),
        ),
        (None, None) if meta.archived => (SessionCatalogState::Archived, None),
        (None, None) => (SessionCatalogState::Active, None),
        _ => return Err(ServiceError::InvalidSeed),
    };
    let forked_from = match (
        meta.forked_from_session_id.as_deref(),
        meta.forked_from_entry_id.as_deref(),
    ) {
        (None, None) => None,
        (Some(source_session_id), Some(source_entry_id)) => Some(ConversationBranchProvenance {
            operation: ConversationBranchOperation::ForkSession,
            source_session_id: SessionId::new(source_session_id.to_owned())
                .map_err(|_| ServiceError::InvalidSeed)?,
            source_entry_id: DurableEntryId::new(source_entry_id.to_owned())
                .map_err(|_| ServiceError::InvalidSeed)?,
            originating_user_entry_id: None,
            model_override: None,
            external_effects_preserved: true,
            warning: EXTERNAL_EFFECTS_WARNING.to_owned(),
        }),
        _ => return Err(ServiceError::InvalidSeed),
    };
    let _ = session_id;
    Ok((lifecycle, retention, forked_from))
}

fn session_id_from_path(path: &Path) -> Result<SessionId, ServiceError> {
    SessionId::new(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or(ServiceError::InvalidSeed)?
            .to_owned(),
    )
    .map_err(|_| ServiceError::InvalidSeed)
}

fn event(payload: EventPayload) -> TimestampedEvent {
    TimestampedEvent::new(now_ms(), payload)
}

fn bounded_text(text: &str, max: usize) -> String {
    octet_serve_backend::sanitize_public_text(text, max, true)
}

fn bounded_single_line_text(text: &str, max: usize) -> String {
    octet_serve_backend::sanitize_public_text(text, max, false)
}

fn next_actor_generation() -> u64 {
    now_ms()
        .saturating_mul(1_000)
        .saturating_add(NEXT_ACTOR_GENERATION.fetch_add(1, Ordering::Relaxed))
        .max(1)
}

fn stable_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn load_or_create_host_id(config: &Config) -> anyhow::Result<HostId> {
    use std::io::{Read as _, Write as _};

    // Keep serve-owned state below the configured session root. The session
    // root may have a broad or user-managed parent (for example `/tmp`), so
    // never tighten permissions on its parent directory.
    let state_dir = secure_serve_state_dir(&config.session_dir)?;
    let path = state_dir.join("serve-host-id");
    let read_existing = || -> anyhow::Result<Option<HostId>> {
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() || metadata.len() > 256 {
            anyhow::bail!("invalid octet serve host identity file");
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        let mut value = String::new();
        file.take(256).read_to_string(&mut value)?;
        let id = HostId::new(value.trim().to_owned()).map_err(anyhow::Error::msg)?;
        Ok(Some(id))
    };
    if let Some(id) = read_existing()? {
        return Ok(id);
    }

    let mut random = [0u8; 32];
    getrandom::fill(&mut random)?;
    let id = HostId::new(format!("host-{}", stable_hash(&random))).map_err(anyhow::Error::msg)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(id.as_str().as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_existing()?
            .ok_or_else(|| anyhow::anyhow!("octet serve host identity creation raced")),
        Err(error) => Err(error.into()),
    }
}

fn secure_serve_state_dir(session_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(session_dir)?;
    let state_dir = session_dir.join(".serve");
    loop {
        match state_dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
                break;
            }
            Ok(_) => anyhow::bail!("octet serve state path must be a real directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&state_dir) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
        let directory = options.open(&state_dir)?;
        if !directory.metadata()?.is_dir() {
            anyhow::bail!("octet serve state path changed during validation");
        }
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(state_dir)
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now())
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "serve/tests/support.rs"]
mod test_support;

#[cfg(test)]
#[path = "serve/tests/authority_tests.rs"]
mod authority_tests;

#[cfg(test)]
#[path = "serve/tests/usage_accounting_tests.rs"]
mod usage_accounting_tests;

#[cfg(test)]
#[path = "serve/tests/driver_robustness_tests.rs"]
mod driver_robustness_tests;

#[cfg(test)]
#[path = "serve/tests/catalog_resume_tests.rs"]
mod catalog_resume_tests;

#[cfg(test)]
#[path = "serve/tests/transcript_resume_tests.rs"]
mod transcript_resume_tests;

#[cfg(test)]
#[path = "serve/tests/pull_request_store_tests.rs"]
mod pull_request_store_tests;

#[cfg(test)]
#[path = "serve/tests/github_cli_tests.rs"]
mod github_cli_tests;

#[cfg(test)]
#[path = "serve/tests/deletion_recovery_tests.rs"]
mod deletion_recovery_tests;

#[cfg(test)]
#[path = "serve/tests/command_discovery_tests.rs"]
mod command_discovery_tests;

#[cfg(test)]
#[path = "serve/tests/project_trust_tests.rs"]
mod project_trust_tests;

#[cfg(test)]
#[path = "serve/tests/checkout_rollback_tests.rs"]
mod checkout_rollback_tests;

#[cfg(test)]
#[path = "serve/tests/resource_evidence_tests.rs"]
mod resource_evidence_tests;

#[cfg(test)]
#[path = "serve/tests/session_state_tests.rs"]
mod session_state_tests;

#[cfg(test)]
#[path = "serve/tests/run_projection_tests.rs"]
mod run_projection_tests;

#[cfg(test)]
#[path = "serve/tests/context_projection_tests.rs"]
mod context_projection_tests;

#[cfg(test)]
#[path = "serve/tests/model_catalog_tests.rs"]
mod model_catalog_tests;
