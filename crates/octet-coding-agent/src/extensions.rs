#![allow(missing_docs)]

//! Product integration for language-neutral executable extensions.
//!
//! `octet-agent` owns the typed JSON-RPC process protocol. This module owns the
//! coding product boundary: shared-resource discovery, explicit activation and
//! policy-derived trust, startup diagnostics, host-state refresh, slash commands,
//! context composition, semantic status collection, and reload.

#[cfg(feature = "serve")]
pub mod serve;

mod mutation_resources;
pub(crate) mod remote_ui;
pub(crate) mod resource_paths;

use octet_agent::extension_remote_ui::{ExtensionRemoteUiOperation, EXTENSION_FEATURE_REMOTE_UI};

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use crossterm::event::Event;
use octet_agent::extension_process::{
    ConfirmationRequest, ConfirmationResponse, ContextContribution, ContextPendingMessagesResult,
    ContextPlacement, ContextSessionManagerResult, ContextSkillSummary, ContextSystemPromptResult,
    DiscoveredExtension, ExtensionAutocompleteRequest, ExtensionComposerOperation,
    ExtensionContextOperation, ExtensionEditorRequest, ExtensionEditorResponse, ExtensionEvent,
    ExtensionEventBus, ExtensionFlag, ExtensionHealthSnapshot, ExtensionHealthState, ExtensionHook,
    ExtensionHookDisposition, ExtensionHostState, ExtensionInputRequest, ExtensionInputResponse,
    ExtensionLifecycleEvent, ExtensionLifecycleOutcome, ExtensionManifest,
    ExtensionMessageInjection, ExtensionPolicy, ExtensionPolicyEvaluationResponse,
    ExtensionProcess, ExtensionRequestFailure, ExtensionRequestId, ExtensionRequestOutcome,
    ExtensionResourceOwner, ExtensionRuntimeConfig, ExtensionRuntimeSharing,
    ExtensionSessionEntryOperation, ExtensionSessionLifecycleReceiver,
    ExtensionSessionLifecycleRequest, ExtensionSessionLifecycleService, ExtensionSource,
    ExtensionStartDecision, ExtensionStatusContribution, ExtensionTerminalInput,
    ExtensionTerminalOperation, ExtensionTerminalResize, ExtensionTrust, ExtensionUiContribution,
    ExtensionUiSurface, ExtensionWidgetPlacement, ShortcutDefinition, TerminalAcquireResult,
    ToolRenderRequest, ToolRenderSegment, DELEGATION_TELEMETRY_SCHEMA, EXTENSION_API_VERSION_0_1,
    EXTENSION_API_VERSION_0_3, EXTENSION_FEATURE_ACTIVE_TOOLS, EXTENSION_FEATURE_AGENT_SESSIONS,
    EXTENSION_FEATURE_COMPOSER, EXTENSION_FEATURE_DELEGATION_TELEMETRY,
    EXTENSION_FEATURE_DYNAMIC_TOOLS, EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
    EXTENSION_FEATURE_MESSAGE_INJECTION, EXTENSION_FEATURE_SESSION_CONTEXT,
    EXTENSION_FEATURE_SESSION_ENTRIES, EXTENSION_FEATURE_SHORTCUTS,
    EXTENSION_FEATURE_SYSTEM_PROMPT_READ, EXTENSION_FEATURE_TERMINAL_HANDOFF,
    EXTENSION_MANIFEST_FILENAME, MAX_EXTENSION_BASH_COMMAND_BYTES,
    MAX_EXTENSION_COMPOSER_TEXT_BYTES, MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS,
    MAX_EXTENSION_INJECTED_MESSAGE_BYTES, MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES,
    MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES, MAX_EXTENSION_SESSION_LABEL_BYTES,
    MAX_EXTENSION_SESSION_NAME_BYTES, MAX_EXTENSION_SHORTCUTS,
    MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES, MAX_EXTENSION_SHORTCUT_ID_BYTES,
    MAX_EXTENSION_SHORTCUT_KEY_BYTES, MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES,
    MAX_EXTENSION_UI_ENTRIES, MAX_EXTENSION_UI_KEY_BYTES, MAX_EXTENSION_UI_LINES,
};
use octet_agent::extension_runtime::{
    ExtensionRuntimeActivationOutcome, ExtensionRuntimeCatalog, ExtensionRuntimeDomain,
    ExtensionRuntimeManager, ExtensionSessionBinding,
};
use octet_agent::{
    Agent, CancellationToken, EntryId, ExtensionHost, ExtensionPolicyDecision,
    ExtensionPresentationSnapshot, ExtensionProviderAuthorizationPolicy,
    ExtensionProviderAuthorizationStatus, ExtensionProviderCatalogEntry, ExtensionProviderOwner,
    ExtensionProviderRegistry, PostMutationContext, PostMutationKind, PostMutationRescan,
    PostMutationState, Session, SessionError, ToolProgress, ToolProgressSink,
    MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES,
};
use octet_ai::{
    AiClient, AssistantMessage, AssistantPart, Auth, CacheCompatibility, Capabilities, Endpoint,
    EndpointId, EndpointTransport, Message, Model, ModelCatalog, ModelId, ModelLimits, ModelSpec,
    OpenAiChatReasoningMode, Protocol, ReasoningCapability, ReasoningConfig, ReasoningControl,
    ReasoningEffort, RequestRuntime, ToolCallId,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use crate::config::{Config, Mode};
use crate::resource_resolver::{
    ResolvedResource, ResourceDiagnosticLevel, ResourceKind, ResourceResolver, ResourceScope,
};
use crate::session_store::SessionStore;
use crate::tui::composer::ComposedInput;
use crate::tui::keymap::{
    extension_shortcut_key, is_reserved_extension_shortcut, parse_extension_shortcut,
    ExtensionShortcutKey,
};
use crate::tui::view::{
    InteractiveShell, ShellAutocompleteItem, ShellEditorSnapshot, ShellExtensionUi,
    ShellExtensionUiLine, ShellExtensionWorking,
};

mod admission;
mod commands;
mod composition;
mod confirmation;
mod event_drain;
mod headless;
mod host_requests;
mod lifecycle;
mod notifications;
mod post_mutation;
mod provider_runtime;
mod runtime_state;
mod status;
mod summaries;
mod trust_policy;
mod turns;
mod ui_projection;

use self::admission::*;
pub use self::composition::assistant_text;
pub use self::composition::latest_assistant_text;
pub use self::composition::ExtensionPromptComposition;
use self::composition::*;
pub use self::confirmation::ExtensionConfirmationHandler;
use self::confirmation::*;
pub(crate) use self::headless::ExtensionShortcutInvocation;
use self::headless::*;
pub(crate) use self::provider_runtime::ExtensionProviderRuntime;
pub(crate) use self::provider_runtime::ProviderCatalogReport;
use self::provider_runtime::*;
pub(crate) use self::runtime_state::ExtensionAutocompleteUpdate;
pub use self::runtime_state::ExtensionBackgroundUpdates;
pub use self::runtime_state::ExtensionToolRenderUpdate;
pub use self::runtime_state::ExtensionTurnLifecycle;
use self::runtime_state::*;
pub use self::summaries::ExtensionLifecycleSnapshot;
pub use self::summaries::ExtensionOptions;
pub use self::summaries::ExtensionPresentationView;
pub use self::summaries::ExtensionProviderSummary;
pub(crate) use self::summaries::ExtensionReloadReport;
pub(crate) use self::summaries::ExtensionRescanReport;
pub use self::summaries::ExtensionSummary;
use self::summaries::*;
pub(crate) use self::trust_policy::persistent_host_authority_grant;
pub(crate) use self::trust_policy::provider_preflight_config;
pub(crate) use self::trust_policy::selected_extension_flag_declarations;
use self::trust_policy::*;

/// The first-party extension that owns every in-harness child-session surface.
pub const SUBAGENTS_EXTENSION_NAME: &str = "octet-subagents";
/// The first-party MCP bridge whose remote transport is separately gated.
pub const MCP_EXTENSION_NAME: &str = "octet-mcp";
const EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT: &str = "--experimental-streamable-http-mcp";
const MAX_CONTEXT_CONTRIBUTION_BYTES: usize = 64 * 1024;
/// Returns whether configuration is eligible to launch the trusted observer.
///
/// The live process/handshake check remains authoritative; this conservative
/// preflight is used only to avoid advertising Ultra in a frontend before its
/// worker app has been built.
#[cfg(feature = "serve")]
pub fn subagents_extension_activation_configured(config: &Config) -> bool {
    if !config.start_extension_processes
        || !config.sandbox.process_execution_allowed()
        || !config
            .enabled_extensions
            .iter()
            .any(|name| name == SUBAGENTS_EXTENSION_NAME)
    {
        return false;
    }
    // Full access has implicit authority. Under controlled policies an exact
    // selected source must have a grant; a bare global name must not authorize
    // a project shadow when offering Ultra before the process is launched.
    if config.effect_policy == octet_agent::EffectPolicy::UnsafeHost {
        return true;
    }
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
    let mut diagnostics = Vec::new();
    let (policy, _) = extension_policy(config, &mut diagnostics);
    snapshot.resources().iter().any(|resource| {
        resource.name == SUBAGENTS_EXTENSION_NAME
            && load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
                .is_some_and(|descriptor| {
                    descriptor
                        .activation
                        .start_decision(descriptor.source, config.workspace_trusted)
                        == ExtensionStartDecision::Allowed
                })
    })
}

const MAX_EXTENSION_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_CONTEXT_LABEL_BYTES: usize = 1024;
const MAX_PENDING_CONTEXT_ITEMS: usize = 256;
const MAX_DIAGNOSTIC_ENTRY_BYTES: usize = 8 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTIC_ENTRIES: usize = 256;
const PROMPT_RPC_DEADLINE: Duration = Duration::from_secs(5);
const AFTER_RESPONSE_RPC_DEADLINE: Duration = Duration::from_secs(2);
const RENDERER_RPC_DEADLINE: Duration = Duration::from_millis(500);
const LIFECYCLE_NOTIFY_DEADLINE: Duration = Duration::from_millis(250);
/// API 0.3 reverse provider registration starts only after initialize is
/// acknowledged. The provider completion notification settles the full initial
/// batch (including zero providers); this remains a bounded fail-closed wait
/// for older extensions that do not negotiate the additive notification.
const PROVIDER_REGISTRATION_BARRIER: Duration = Duration::from_millis(500);
/// Upper bound on live-registration notices emitted by one catalog
/// synchronization, so a late bulk registration cannot flood the transcript.
const MAX_LIVE_REGISTRATION_NOTICES: usize = 8;
/// Total per-extension deadline for the typed post-mutation hook. A timeout
/// drops only that extension's rescan request after the host mutation settled.
const POST_MUTATION_RPC_DEADLINE: Duration = Duration::from_millis(250);
const MAX_SEEN_POST_MUTATION_IDS: usize = 256;
const MAX_PENDING_POST_MUTATION_RESCANS: usize = 256;
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(3);
/// An options-menu action runs while a person watches its live progress and
/// can cancel it, so installs and downloads may take this long.
const MENU_ACTION_DEADLINE: Duration = Duration::from_secs(30 * 60);
/// Building an options menu must stay interactive; a slower extension gets
/// generated entries instead.
const MENU_COLLECT_DEADLINE: Duration = Duration::from_secs(5);
const BACKGROUND_UPDATE_CAPACITY: usize = 64;
const SHORTCUT_TASK_CONCURRENCY: usize = 8;
const EVENT_DRAIN_BUDGET: usize = 64;
const EVENT_DRAIN_PER_RECEIVER_BUDGET: usize = 8;
const CONFIRMATION_DENIAL_QUEUE_CAPACITY: usize = 64;
const CONFIRMATION_DENIAL_CONCURRENCY: usize = 8;
const INPUT_CANCELLATION_QUEUE_CAPACITY: usize = 64;
const INPUT_CANCELLATION_CONCURRENCY: usize = 8;
const EXTENSION_UI_EDITOR_RESPONSE_DEADLINE: Duration = Duration::from_millis(500);
const EXTENSION_AUTOCOMPLETE_DEADLINE: Duration = Duration::from_millis(500);
const MAX_EXTENSION_AUTOCOMPLETE_TASKS: usize = 1;
const MAX_PROJECTED_EXTENSION_UI_LINES: usize = if MAX_EXTENSION_UI_ENTRIES > MAX_EXTENSION_UI_LINES
{
    MAX_EXTENSION_UI_ENTRIES
} else {
    MAX_EXTENSION_UI_LINES
};
const SESSION_LIFECYCLE_QUEUE_CAPACITY: usize = 8;
/// Upper bound for one `tools/set_active` name list. Individual names mirror
/// the agent-side key cap; the list bound is host-local.
const MAX_HOST_REQUEST_TOOL_NAMES: usize = 64;
/// Upper bound for host requests queued between two shell drains.
const HOST_REQUEST_QUEUE_CAPACITY: usize = 64;
const NATIVE_HOST_EXTENSION_START_DIAGNOSTIC: &str = "executable extensions were not started: the native-host protocol reports extension discovery only and never starts extension processes";
const CONTROLLED_EXTENSION_START_DIAGNOSTIC: &str = "enabled extensions without host authority were not started: grant host authority per source in /extensions or trusted_extensions; safe mode is not a sandbox—granted extensions run with your OS permissions outside the tool-effect broker";
static NEXT_EXTENSION_RUN_ID: AtomicU64 = AtomicU64::new(1);
// The process owns one interactive terminal. Retained workspace-service
// generations must keep waking the same consumer across App/session rebuilds.
static INTERACTIVE_REMOTE_UI_WAKE: std::sync::OnceLock<Arc<tokio::sync::Notify>> =
    std::sync::OnceLock::new();

/// The active-session driver is dispatched only by the interactive idle loop.
/// Other frontends must not advertise an operation they cannot settle safely.
fn active_session_lifecycle_enabled(config: &Config) -> bool {
    matches!(&config.mode, Mode::Interactive)
}

/// A lifecycle driver belongs to exactly one interactive session, so it may
/// only be injected into an isolated API 0.3/0.4 process. Shared and legacy
/// processes must never retain a binding-specific reverse service.
fn extension_session_lifecycle_eligible(descriptor: &DiscoveredExtension) -> bool {
    matches!(
        descriptor.manifest.api_version.as_str(),
        EXTENSION_API_VERSION_0_3 | octet_agent::extension_process::EXTENSION_API_VERSION_0_4
    ) && descriptor.manifest.runtime.sharing == ExtensionRuntimeSharing::Isolated
}

#[derive(serde::Serialize)]
struct BeforePromptHookPayload<'a> {
    prompt: &'a str,
}

#[derive(serde::Serialize)]
struct AfterResponseHookPayload<'a> {
    response: &'a str,
}

fn before_prompt_hook_payload(prompt: &str) -> serde_json::Value {
    serde_json::json!(BeforePromptHookPayload { prompt })
}

fn after_response_hook_payload(response: &str) -> serde_json::Value {
    serde_json::json!(AfterResponseHookPayload { response })
}

/// The MCP bridge owns server configuration; the host owns permission for the
/// exact call it dispatched. Full access permits external mutations too, while
/// controlled modes never gain external authority from an annotation or hint.
fn mcp_policy_response(
    effect_policy: octet_agent::EffectPolicy,
    process: &ExtensionProcess,
    generation: u64,
    parent_request_id: u64,
    intent: &octet_agent::ExtensionActionIntent,
) -> ExtensionPolicyEvaluationResponse {
    let allowed = effect_policy == octet_agent::EffectPolicy::UnsafeHost
        && process.descriptor().manifest.name == MCP_EXTENSION_NAME
        && intent.kind == "external_side_effect"
        && intent.operation == "mcp.tool.call"
        && mcp_policy_target(&intent.target).is_some_and(|(tool, arguments)| {
            process.policy_matches_tool_call(generation, parent_request_id, tool, arguments)
        });
    ExtensionPolicyEvaluationResponse {
        decision: if allowed {
            ExtensionPolicyDecision::Allow
        } else {
            ExtensionPolicyDecision::Deny
        },
        approval_token: None,
    }
}

fn mcp_policy_target(target: &Value) -> Option<(&str, &Value)> {
    let server = target.get("server")?.as_str()?;
    if server.is_empty()
        || server.len() > 32
        || !server.as_bytes()[0].is_ascii_lowercase()
        || !server
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return None;
    }
    let tool = target.get("tool")?.as_str()?;
    // This is only a namespace sanity check: server IDs may have overlapping
    // prefixes. Authority is the exact host-dispatched published tool (whose
    // bridge-generated identity includes the server), never this display field.
    if !tool.starts_with(&format!("mcp_{}_", server.replace('-', "_"))) {
        return None;
    }
    let arguments = target.get("arguments")?;
    arguments.is_object().then_some((tool, arguments))
}

pub struct ExecutableExtensions {
    telemetry: Option<octet_agent::TelemetryObserver>,
    telemetry_rejected: u64,
    telemetry_error: Option<std::io::ErrorKind>,
    processes: Vec<ExtensionProcess>,
    resource_paths_epoch: Arc<std::sync::atomic::AtomicU64>,
    resource_paths_live: Arc<std::sync::atomic::AtomicBool>,
    provider_runtime: ExtensionProviderRuntime,
    runtime_manager: Option<ExtensionRuntimeManager>,
    runtime_binding: Option<ExtensionSessionBinding>,
    receivers: Vec<broadcast::Receiver<ExtensionEvent>>,
    shortcuts: Vec<RegisteredExtensionShortcut>,
    summaries: Vec<ExtensionSummary>,
    diagnostics: BoundedDiagnostics,
    pending_context: PendingContext,
    presentations: BTreeMap<String, ExtensionPresentationView>,
    semantic_ui: BTreeMap<String, SemanticUiView>,
    autocomplete_registrations: BTreeMap<String, RegisteredAutocomplete>,
    displayed_autocomplete: Option<AutocompleteFence>,
    last_editor_state: Option<EditorStateDelivery>,
    background_tx: mpsc::Sender<ExtensionBackgroundUpdate>,
    background_rx: mpsc::Receiver<ExtensionBackgroundUpdate>,
    renderer_tasks: Vec<JoinHandle<()>>,
    autocomplete_tasks: Vec<JoinHandle<()>>,
    pending_editor_requests: VecDeque<PendingEditorRequest>,
    pending_host_requests: VecDeque<PendingHostRequest>,
    pending_session_requests: VecDeque<PendingHostRequest>,
    /// The one foreground terminal grant the host can cede, or `None` while the
    /// host still owns its own raw terminal.
    terminal_arbiter: TerminalGrantArbiter,
    remote_ui: remote_ui::RemoteUi,
    remote_ui_wake: Option<Arc<tokio::sync::Notify>>,
    command_dialog_process: Option<(String, u64)>,
    dynamic_shortcuts: Vec<RegisteredDynamicShortcut>,
    shortcut_tasks: Vec<JoinHandle<()>>,
    event_drain_cursor: usize,
    confirmation_denials: VecDeque<PendingConfirmationDenial>,
    confirmation_tasks: Vec<JoinHandle<()>>,
    input_cancellations: VecDeque<PendingInputCancellation>,
    input_tasks: Vec<JoinHandle<()>>,
    policy_supervisors: Vec<JoinHandle<()>>,
    effect_policy: octet_agent::EffectPolicy,
    event_bus: Option<Arc<ExtensionEventBus>>,
    session_lifecycle_service: Option<ExtensionSessionLifecycleService>,
    session_lifecycle_receiver: Option<ExtensionSessionLifecycleReceiver>,
    session_id: Option<String>,
    /// The latest host-state snapshot, refreshed whenever the model, reasoning,
    /// or active session changes. Read-only context snapshots answer from here
    /// so a shared process never discloses a stale per-session projection.
    host_state: Mutex<ExtensionHostState>,
    /// Host working directory for the active session, captured at discovery.
    workspace: PathBuf,
    resource_owner: Option<String>,
    session_started_at: Instant,
    session_lifecycle_started: bool,
    // UI hooks must run while the foreground shell can answer reverse requests,
    // never inside the blocking application bootstrap.
    pending_session_hook_starts: Vec<(ExtensionProcess, String)>,
    session_hook_start_tasks: Vec<JoinHandle<()>>,
    last_lifecycle_outcome: Option<ExtensionLifecycleOutcome>,
    /// Stable host-created mutation identities already delivered to hooks.
    /// Keeping this outside process generations prevents reload/restart paths
    /// from redelivering the same completed mutation.
    seen_post_mutation_ids: VecDeque<String>,
    /// Bounded resolver work requested by admitted post-mutation hooks. The
    /// product resource owner drains this queue and decides how to rescan; an
    /// extension never receives authority to perform the host mutation.
    pending_post_mutation_rescans: VecDeque<PostMutationRescan>,
    mutation_family_generations: BTreeMap<String, u64>,
    rescan_global_config: Option<PathBuf>,
    /// Discovery configuration bound at product construction.
    ///
    /// A post-mutation rescan must re-resolve through the same workspace trust,
    /// explicit extension roots, and extension policy as initial discovery, so
    /// the product drain paths reuse the exact configuration the fleet was
    /// built from instead of guessing roots at drain time.
    rescan_config: Option<Config>,
    #[cfg(test)]
    lifecycle_delivery_test_control: Option<std::sync::Arc<LifecycleDeliveryTestControl>>,
}

impl Default for ExecutableExtensions {
    fn default() -> Self {
        let (background_tx, background_rx) = mpsc::channel(BACKGROUND_UPDATE_CAPACITY);
        Self {
            telemetry: None,
            telemetry_rejected: 0,
            telemetry_error: None,
            processes: Vec::new(),
            resource_paths_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            resource_paths_live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            provider_runtime: ExtensionProviderRuntime::default(),
            runtime_manager: None,
            runtime_binding: None,
            receivers: Vec::new(),
            shortcuts: Vec::new(),
            summaries: Vec::new(),
            diagnostics: BoundedDiagnostics::default(),
            pending_context: PendingContext::default(),
            presentations: BTreeMap::new(),
            semantic_ui: BTreeMap::new(),
            autocomplete_registrations: BTreeMap::new(),
            displayed_autocomplete: None,
            last_editor_state: None,
            background_tx,
            background_rx,
            renderer_tasks: Vec::new(),
            autocomplete_tasks: Vec::new(),
            pending_editor_requests: VecDeque::new(),
            pending_host_requests: VecDeque::new(),
            pending_session_requests: VecDeque::new(),
            terminal_arbiter: TerminalGrantArbiter::default(),
            remote_ui: remote_ui::RemoteUi::default(),
            remote_ui_wake: None,
            command_dialog_process: None,
            dynamic_shortcuts: Vec::new(),
            shortcut_tasks: Vec::new(),
            event_drain_cursor: 0,
            confirmation_denials: VecDeque::new(),
            confirmation_tasks: Vec::new(),
            input_cancellations: VecDeque::new(),
            input_tasks: Vec::new(),
            policy_supervisors: Vec::new(),
            effect_policy: octet_agent::EffectPolicy::Controlled,
            event_bus: None,
            session_lifecycle_service: None,
            session_lifecycle_receiver: None,
            session_id: None,
            host_state: Mutex::new(ExtensionHostState::default()),
            workspace: PathBuf::new(),
            resource_owner: None,
            session_started_at: Instant::now(),
            session_lifecycle_started: false,
            pending_session_hook_starts: Vec::new(),
            session_hook_start_tasks: Vec::new(),
            last_lifecycle_outcome: None,
            seen_post_mutation_ids: VecDeque::new(),
            pending_post_mutation_rescans: VecDeque::new(),
            mutation_family_generations: BTreeMap::new(),
            rescan_global_config: None,
            rescan_config: None,
            #[cfg(test)]
            lifecycle_delivery_test_control: None,
        }
    }
}

impl ExecutableExtensions {}

fn shutdown_telemetry_observer(observer: octet_agent::TelemetryObserver) {
    if let Err(error) = observer.shutdown(Duration::from_secs(2)) {
        crate::output::stderr!("warning: optional telemetry drain incomplete: {error}");
    }
    let status = observer.status();
    if status.drain_timed_out {
        crate::output::stderr!("warning: optional telemetry shutdown timed out with {} record(s) not confirmed written", status.pending_records);
    }
}

impl Drop for ExecutableExtensions {
    fn drop(&mut self) {
        self.resource_paths_live
            .store(false, std::sync::atomic::Ordering::Release);
        self.deactivate_session_lifecycle_driver();
        self.cancel_background_work();
        if !self.processes.is_empty() || self.telemetry.is_some() {
            // Mode error paths still pass through this boundary. In the normal
            // multi-thread runtime, request graceful shutdown before Arc
            // teardown falls back to the process-group kill guard.
            self.shutdown_blocking();
        }
        // A current-thread runtime cannot enter the blocking shutdown adapter.
        // Error-path fallback still moves the bounded writer drain off-owner.
        if let Some(observer) = self.telemetry.take() {
            if let Err(error) = std::thread::Builder::new()
                .name("octet-telemetry-shutdown".into())
                .spawn(move || shutdown_telemetry_observer(observer))
            {
                crate::output::stderr!(
                    "warning: could not start telemetry shutdown worker: {error}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "extensions/hook_tests.rs"]
mod hook_tests;

#[cfg(test)]
#[path = "extensions/bus_tests.rs"]
mod bus_tests;

#[cfg(test)]
#[path = "extensions/lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "extensions/ui_transport_tests.rs"]
mod ui_transport_tests;

#[cfg(all(test, unix))]
pub(crate) mod reload_lifecycle_test_support {
    use super::*;

    /// A real process/command owner with a deterministic worker projection.
    /// No provider or actual child inference is used by these lifecycle tests.
    pub(crate) async fn fixture(
        workspace: &Path,
        worker_state: Option<octet_agent::ExtensionPresentationState>,
    ) -> (ExecutableExtensions, ExtensionProcess, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let fixture = workspace.join("reload-lifecycle.sh");
        let wire_log = workspace.join("reload-lifecycle.jsonl");
        std::fs::write(
            &fixture,
            r#"#!/bin/sh
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.4","tools":[],"commands":[{"name":"worker-control","description":"Check retained control"}],"protocol":{"version":"0.4","features":["request_cancellation","content_parts","terminal_handoff"],"limits":{"max_concurrent_requests":1}}}}\n' "$id"
while IFS= read -r request; do
  printf '%s\n' "$request" >> "$OCTET_WORKSPACE/reload-lifecycle.jsonl"
  case "$request" in
    *'"method":"command/execute"'*)
      id=$(printf '%s' "$request" | request_id)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"control retained","notifications":[],"context":[]}}\n' "$id"
      ;;
    *'"method":"shutdown"'*)
      id=$(printf '%s' "$request" | request_id)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = ExtensionManifest::parse(
            r#"
name = "octet-subagents"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "reload-lifecycle.sh"
[contributes]
commands = ["worker-control"]
"#,
        )
        .unwrap();
        let mut config = ExtensionRuntimeConfig::new(workspace);
        config.supervise = false;
        let process = ExtensionProcess::start(
            DiscoveredExtension {
                manifest,
                manifest_path: workspace.join("extension.toml"),
                source: ExtensionSource::Explicit,
                activation: octet_agent::extension_process::ExtensionActivation {
                    enabled: true,
                    trust: ExtensionTrust::Trusted,
                },
            },
            config,
        )
        .await
        .unwrap();
        let mut extensions = ExecutableExtensions::default();
        extensions.receivers.push(process.subscribe());
        extensions.processes.push(process.clone());
        if let Some(state) = worker_state {
            let mut snapshot: ExtensionPresentationSnapshot =
                serde_json::from_str(include_str!("../fixtures/extension-presentation.json"))
                    .unwrap();
            let nodes = &mut snapshot.collection.as_mut().unwrap().nodes;
            nodes.truncate(1);
            nodes[0].state = state;
            nodes[0].references.clear();
            extensions.presentations.insert(
                SUBAGENTS_EXTENSION_NAME.into(),
                ExtensionPresentationView {
                    extension: SUBAGENTS_EXTENSION_NAME.into(),
                    generation: process.health_snapshot().generation,
                    extension_instance_id: process.extension_instance_id().to_owned(),
                    resource_owner: None,
                    snapshot,
                },
            );
        }
        (extensions, process, wire_log)
    }

    pub(crate) fn acquire_terminal(
        extensions: &mut ExecutableExtensions,
        shell: &mut InteractiveShell,
        process: &ExtensionProcess,
    ) {
        // Keep the headless TestTerminal attached: suspending it and resuming
        // would enter the developer's real tty. Exercise the same arbiter and
        // input ownership as acquire; physical mode switching is PTY coverage.
        extensions
            .terminal_arbiter
            .acquire(
                TerminalHolder {
                    owner: extensions.resource_owner.clone(),
                    instance_id: process.extension_instance_id().to_owned(),
                    generation: process.health_snapshot().generation,
                    name: process.descriptor().manifest.name.clone(),
                },
                120,
                40,
            )
            .unwrap();
        shell.cede_terminal_input();
        assert!(extensions.terminal_grant_is_active());
        assert!(shell.terminal_input_parking().load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn terminal_reconciliation_restores_input_for_dead_or_changed_owner() {
        for dead in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (mut extensions, process, wire) = fixture(directory.path(), None).await;
            let mut shell = InteractiveShell::test_shell();
            acquire_terminal(&mut extensions, &mut shell, &process);
            if dead {
                assert!(process.shutdown().await);
            } else {
                extensions.resource_owner = Some("replacement-owner".into());
            }
            extensions.reconcile_terminal_grant_for_shell(&mut shell);
            assert!(!extensions.terminal_grant_is_active());
            assert!(!shell.terminal_input_parking().load(Ordering::SeqCst));
            extensions.reconcile_terminal_grant_for_shell(&mut shell);
            extensions.shutdown().await;
            if dead {
                assert!(!std::fs::read_to_string(wire)
                    .unwrap()
                    .contains("terminal/grant-lost"));
            } else {
                assert_revoked_before_shutdown(&wire);
            }
        }
    }

    pub(crate) fn assert_revoked_before_shutdown(wire_log: &Path) {
        let wire = std::fs::read_to_string(wire_log).unwrap();
        let methods: Vec<String> = wire
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["method"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let revoked = methods
            .iter()
            .position(|method| method == "terminal/grant-lost")
            .unwrap();
        let shutdown = methods
            .iter()
            .position(|method| method == "shutdown")
            .unwrap();
        assert!(revoked < shutdown, "{methods:?}");
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "terminal/grant-lost")
                .count(),
            1
        );
    }
}
