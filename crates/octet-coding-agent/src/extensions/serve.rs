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

mod checkout;
mod completion;
mod context_projection;
mod delegated_sessions;
mod event_projection;
mod evidence;
mod goals;
mod host_api;
mod idle_commands;
mod model_catalog;
mod projection_state;
mod prompt_input;
mod pull_requests;
mod run_driver;
mod session_driver;
mod session_meta;
mod themes;
mod tool_activity;
mod worker;

use self::checkout::*;
use self::completion::*;
use self::context_projection::*;
use self::delegated_sessions::*;
use self::event_projection::*;
use self::evidence::*;
use self::goals::*;
use self::host_api::*;
use self::idle_commands::*;
use self::model_catalog::*;
use self::projection_state::*;
use self::prompt_input::*;
use self::pull_requests::*;
use self::run_driver::*;
use self::session_driver::*;
use self::session_meta::*;
use self::themes::*;
use self::tool_activity::*;
use self::worker::*;

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

const SESSION_DELETION_VERSION: u16 = 1;
const SESSION_DELETION_DIRECTORY: &str = "session-deletions-v1";
const MAX_SESSION_DELETION_RECORD_BYTES: u64 = 4 * 1024;

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
