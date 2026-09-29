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
    ExtensionRuntimeConfig, ExtensionRuntimeSharing, ExtensionSessionEntryOperation,
    ExtensionSessionLifecycleReceiver, ExtensionSessionLifecycleRequest,
    ExtensionSessionLifecycleService, ExtensionSource, ExtensionStatusContribution,
    ExtensionTerminalInput, ExtensionTerminalOperation, ExtensionTerminalResize, ExtensionTrust,
    ExtensionUiContribution, ExtensionUiSurface, ExtensionWidgetPlacement, ShortcutDefinition,
    TerminalAcquireResult, ToolRenderRequest, ToolRenderSegment, DELEGATION_TELEMETRY_SCHEMA,
    EXTENSION_API_VERSION_0_1, EXTENSION_API_VERSION_0_3, EXTENSION_FEATURE_ACTIVE_TOOLS,
    EXTENSION_FEATURE_AGENT_SESSIONS, EXTENSION_FEATURE_COMPOSER,
    EXTENSION_FEATURE_DELEGATION_TELEMETRY, EXTENSION_FEATURE_DYNAMIC_TOOLS,
    EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, EXTENSION_FEATURE_MESSAGE_INJECTION,
    EXTENSION_FEATURE_SESSION_CONTEXT, EXTENSION_FEATURE_SESSION_ENTRIES,
    EXTENSION_FEATURE_SHORTCUTS, EXTENSION_FEATURE_SYSTEM_PROMPT_READ,
    EXTENSION_FEATURE_TERMINAL_HANDOFF, EXTENSION_MANIFEST_FILENAME,
    MAX_EXTENSION_BASH_COMMAND_BYTES, MAX_EXTENSION_COMPOSER_TEXT_BYTES,
    MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS, MAX_EXTENSION_INJECTED_MESSAGE_BYTES,
    MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES, MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES,
    MAX_EXTENSION_SESSION_LABEL_BYTES, MAX_EXTENSION_SESSION_NAME_BYTES, MAX_EXTENSION_SHORTCUTS,
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
    config.effect_policy == octet_agent::EffectPolicy::UnsafeHost
        && config.sandbox.process_execution_allowed()
        && config
            .enabled_extensions
            .iter()
            .any(|name| name == SUBAGENTS_EXTENSION_NAME)
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
const CONTROLLED_EXTENSION_START_DIAGNOSTIC: &str = "executable extensions were not started: safe mode/controlled policies deny extension process startup even with explicit trust; full access (unsafe_host) is required and should be used only inside OS-level isolation";
static NEXT_EXTENSION_RUN_ID: AtomicU64 = AtomicU64::new(1);

/// The active-session driver is dispatched only by the interactive idle loop.
/// Other frontends must not advertise an operation they cannot settle safely.
fn active_session_lifecycle_enabled(config: &Config) -> bool {
    matches!(&config.mode, Mode::Interactive)
}

/// A lifecycle driver belongs to exactly one interactive session, so it may
/// only be injected into an isolated API 0.3 process. Shared and legacy
/// processes must never retain a binding-specific reverse service.
fn extension_session_lifecycle_eligible(descriptor: &DiscoveredExtension) -> bool {
    descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3
        && descriptor.manifest.runtime.sharing == ExtensionRuntimeSharing::Isolated
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConfiguredTrustGrant {
    Global { name: String },
    Exact { name: String, path: PathBuf },
}

impl ConfiguredTrustGrant {
    fn matches(&self, descriptor: &DiscoveredExtension) -> bool {
        match self {
            Self::Global { name } => {
                descriptor.source == ExtensionSource::Global && descriptor.manifest.name == *name
            }
            Self::Exact { name, path } => {
                descriptor.manifest.name == *name && descriptor.manifest_path == *path
            }
        }
    }

    fn display(&self) -> String {
        match self {
            Self::Global { name } => name.clone(),
            Self::Exact { name, path } => format!("{name}@{}", path.display()),
        }
    }
}

fn extension_policy(
    config: &Config,
    diagnostics: &mut Vec<String>,
) -> (ExtensionPolicy, Vec<ConfiguredTrustGrant>) {
    // Implicit full-access trust is derived anew, never copied into either
    // persistent or invocation-specific grants in the product configuration.
    let mut policy = ExtensionPolicy::for_effect_policy(config.effect_policy);
    for name in &config.enabled_extensions {
        policy.enable(name.clone());
    }

    let mut grants = Vec::new();
    for grant in &config.trusted_extensions {
        if let Some((name, path)) = grant.split_once('@') {
            match normalize_trusted_manifest_path(Path::new(path)) {
                Ok(path) => {
                    policy.trust_source(name.to_owned(), path.clone());
                    grants.push(ConfiguredTrustGrant::Exact {
                        name: name.to_owned(),
                        path,
                    });
                }
                Err(error) => diagnostics.push(format!(
                    "warning: invalid source-bound extension trust grant {grant:?}: {error}"
                )),
            }
        } else {
            policy.trust(grant.clone());
            grants.push(ConfiguredTrustGrant::Global {
                name: grant.clone(),
            });
        }
    }
    for name in &config.invocation_trusted_extensions {
        policy.trust_for_invocation(name.clone());
    }
    (policy, grants)
}

/// Returns a copy of the product configuration narrowed to extensions that can
/// statically negotiate API 0.3 provider catalogs.
///
/// Provider discovery is the only reason bootstrap may activate an extension
/// before the final session/model exists. Keep that exceptional process set
/// narrow: ordinary tools, hooks, and UI extensions must first observe the
/// selected real launch state.
pub(crate) fn provider_preflight_config(config: &Config) -> Config {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
    let mut diagnostics = Vec::new();
    let (policy, _) = extension_policy(config, &mut diagnostics);
    let mut descriptors = BTreeMap::<String, DiscoveredExtension>::new();
    for resource in snapshot.resources() {
        let Some(descriptor) =
            load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
        else {
            continue;
        };
        descriptors
            .entry(descriptor.manifest.name.clone())
            .or_insert(descriptor);
    }
    let provider_names = descriptors
        .into_values()
        .filter(|descriptor| {
            descriptor.activation.enabled
                && descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3
                && descriptor.manifest.contributes.providers
        })
        .map(|descriptor| descriptor.manifest.name)
        .collect::<BTreeSet<_>>();
    let mut preflight = config.clone();
    preflight
        .enabled_extensions
        .retain(|name| provider_names.contains(name));
    preflight
}

fn normalize_trusted_manifest_path(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("the manifest path must be absolute");
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("the manifest path has no file name"))?;
    if file_name != EXTENSION_MANIFEST_FILENAME {
        anyhow::bail!("the manifest path must end in {EXTENSION_MANIFEST_FILENAME}");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("the manifest path has no parent directory"))?
        .canonicalize()
        .with_context(|| format!("cannot normalize manifest parent for {}", path.display()))?;
    Ok(parent.join(file_name))
}

fn persistent_trust_grant(descriptor: &DiscoveredExtension) -> String {
    if descriptor.source == ExtensionSource::Global {
        descriptor.manifest.name.clone()
    } else {
        format!(
            "{}@{}",
            descriptor.manifest.name,
            descriptor.manifest_path.display()
        )
    }
}

fn sha256_manifest(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!("{:x}", Sha256::digest(&bytes)),
        Err(_) => "unavailable".to_owned(),
    }
}

fn installed_bundle_digest(manifest_path: &Path) -> Option<String> {
    let install_path = manifest_path.parent()?.join("install.json");
    let bytes = std::fs::read(&install_path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let digest = value.get("archive_sha256")?.as_str()?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

fn extension_compatibility(
    name: &str,
    running: bool,
    features: &[String],
    health: Option<&ExtensionHealthSnapshot>,
) -> (Option<String>, String) {
    if name != SUBAGENTS_EXTENSION_NAME {
        return (None, "not_applicable".to_owned());
    }
    if features
        .iter()
        .any(|feature| feature == EXTENSION_FEATURE_DELEGATION_TELEMETRY)
    {
        return (
            Some(DELEGATION_TELEMETRY_SCHEMA.to_owned()),
            "compatible".to_owned(),
        );
    }
    if running {
        return (
            None,
            "incompatible: delegation telemetry was not negotiated; rebuild/reinstall the current workspace bundle"
                .to_owned(),
        );
    }
    let error = health
        .and_then(|health| health.last_error.as_deref())
        .unwrap_or_default();
    if error.contains(EXTENSION_FEATURE_DELEGATION_TELEMETRY)
        || error.contains("required API 0.2 features")
    {
        (
            None,
            "incompatible: rebuild/reinstall the current workspace bundle".to_owned(),
        )
    } else {
        (None, "unavailable".to_owned())
    }
}

/// Read only selected, trusted manifest metadata for CLI construction.
///
/// This deliberately shares the runtime resolver and policy calculation but
/// never starts or imports an extension process.
pub(crate) fn selected_extension_flag_declarations(
    config: &Config,
) -> Vec<(String, ExtensionFlag)> {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
    let mut diagnostics = Vec::new();
    let (policy, _) = extension_policy(config, &mut diagnostics);
    let mut by_name = BTreeMap::<String, DiscoveredExtension>::new();
    for resource in snapshot.resources() {
        let Some(descriptor) =
            load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
        else {
            continue;
        };
        by_name
            .entry(descriptor.manifest.name.clone())
            .or_insert(descriptor);
    }
    by_name
        .into_values()
        .filter(|descriptor| {
            descriptor.activation.enabled && descriptor.activation.trust == ExtensionTrust::Trusted
        })
        .flat_map(|descriptor| {
            let name = descriptor.manifest.name;
            descriptor
                .manifest
                .contributes
                .flags
                .into_iter()
                .map(move |flag| (name.clone(), flag))
        })
        .collect()
}

fn load_extension_descriptor(
    resolver: &ResourceResolver,
    resource: &ResolvedResource,
    policy: &ExtensionPolicy,
    diagnostics: &mut Vec<String>,
) -> Option<DiscoveredExtension> {
    let manifest = match resolver
        .read_text(resource)
        .and_then(|source| ExtensionManifest::parse(&source).map_err(anyhow::Error::from))
    {
        Ok(manifest) => manifest,
        Err(error) => {
            diagnostics.push(format!("error: {}: {error}", resource.path.display()));
            return None;
        }
    };
    if resource.name != manifest.name {
        diagnostics.push(format!(
            "warning: {}: extension directory name {:?} must match manifest name {:?}; ignored",
            resource.path.display(),
            resource.name,
            manifest.name
        ));
        return None;
    }
    let source = match resource.scope {
        ResourceScope::Global => ExtensionSource::Global,
        ResourceScope::Project => ExtensionSource::Project,
        ResourceScope::Explicit => ExtensionSource::Explicit,
    };
    Some(DiscoveredExtension {
        activation: policy.activation(&manifest.name, &resource.path, source),
        manifest,
        manifest_path: resource.path.clone(),
        source,
    })
}

/// One char-safe bounded slice of a user shell command for a fixed-shape
/// extension notification. The bound is the same one the host applies, so a
/// long escape never turns into a protocol failure diagnostic.
fn bounded_notification_command(command: &str) -> &str {
    let mut end = command.len().min(MAX_EXTENSION_BASH_COMMAND_BYTES);
    while end > 0 && !command.is_char_boundary(end) {
        end -= 1;
    }
    &command[..end]
}

pub trait ExtensionConfirmationHandler {
    /// Wait until the frontend asks to cancel the in-flight command. Dropping
    /// this future must leave the input source usable by `confirm`.
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        Box::pin(std::future::pending())
    }

    /// Receive one bounded, request-scoped extension command progress event.
    ///
    /// Implementations must treat this as transient presentation only; it is
    /// never a command result or durable session content.
    fn progress(&mut self, _extension: &str, _progress: &ToolProgress) {}

    /// Clear transient command progress after its request settles.
    fn finish_progress(&mut self, _extension: &str) {}

    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>>;

    fn input<'a>(
        &'a mut self,
        _extension: &'a str,
        _request: &'a ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

struct PreapprovedExtensionConfirmation<'a, H: ?Sized> {
    // One action-level approval may satisfy only the first confirmation emitted
    // by that same manifest-scoped command; later prompts still reach the UI.
    inner: &'a mut H,
    remaining: usize,
}

impl<H> ExtensionConfirmationHandler for PreapprovedExtensionConfirmation<'_, H>
where
    H: ExtensionConfirmationHandler + ?Sized,
{
    fn wait_for_cancel<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
        self.inner.wait_for_cancel()
    }

    fn progress(&mut self, extension: &str, progress: &ToolProgress) {
        self.inner.progress(extension, progress);
    }

    fn finish_progress(&mut self, extension: &str) {
        self.inner.finish_progress(extension);
    }

    fn confirm<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ConfirmationRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + 'a>> {
        if self.remaining > 0 {
            self.remaining -= 1;
            Box::pin(std::future::ready(Ok(true)))
        } else {
            self.inner.confirm(extension, request)
        }
    }

    fn input<'a>(
        &'a mut self,
        extension: &'a str,
        request: &'a ExtensionInputRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + 'a>> {
        self.inner.input(extension, request)
    }
}

/// One frontend-owned snapshot of the live extension processes, taken before a
/// host dialog borrows [`ExecutableExtensions`] mutably.
///
/// The confirmation and input handlers run under the same mutable borrow that
/// drives the extension runtime, so they cannot call the `*_all` fan-out
/// helpers. The snapshot holds the same process handles, and each host emitter
/// is a no-op unless `lifecycle_events_v2` was negotiated and the process is
/// live, so presenting a dialog never needs its own feature check.
#[derive(Default)]
pub struct ExtensionLifecycleSnapshot {
    processes: Vec<ExtensionProcess>,
}

impl ExecutableExtensions {
    /// Capture the current process handles for one frontend-owned broadcast.
    pub fn lifecycle_snapshot(&self) -> ExtensionLifecycleSnapshot {
        ExtensionLifecycleSnapshot {
            processes: self.processes.clone(),
        }
    }
}

impl ExtensionLifecycleSnapshot {
    /// Open one host-owned dialog boundary on every captured process.
    pub fn dialog_started(&self, dialog: &str) {
        for process in &self.processes {
            let _ = process.notify_dialog_started(dialog);
        }
    }

    /// Close one host-owned dialog boundary on every captured process.
    pub fn dialog_settled(&self, dialog: &str) {
        for process in &self.processes {
            let _ = process.notify_dialog_settled(dialog);
        }
    }
}

/// One live secret-free API 0.3 provider declaration owned by an extension.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ExtensionProviderSummary {
    /// Extension-declared provider identifier.
    pub id: String,
    /// Extension-declared display label.
    pub label: String,
    /// Host-owned availability wire status (`ready`, `pending`, `denied`,
    /// `unavailable`, or `revoked`).
    pub authorization: String,
    /// Routable catalog model ids (`provider/model`).
    pub models: Vec<String>,
    /// True only after the owning generation completed its initial catalog; a
    /// declaration may be recorded while that batch is still incomplete, and
    /// then it is never callable.
    pub live: bool,
}

/// Results stay typed until the caller chooses automatic or explicit feedback.
#[derive(Debug, Default)]
pub(crate) struct ExtensionReloadReport {
    pub processes: Vec<(String, Result<String, String>)>,
    pub shortcuts: Vec<String>,
    pub rescans: ExtensionRescanReport,
    pub details: Vec<String>,
    /// Occurrences (including discarded requests), never persistent problems.
    pub events: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ExtensionRescanReport {
    pub checked: Vec<(String, Vec<String>)>,
    pub details: Vec<String>,
    pub events: Vec<String>,
}

impl ExtensionRescanReport {
    fn into_notices(self) -> Vec<String> {
        self.checked
            .into_iter()
            .flat_map(|(_, problems)| problems)
            .chain(self.details)
            .chain(self.events)
            .collect()
    }
}

#[cfg(all(test, unix))]
impl ExtensionReloadReport {
    fn into_notices(self) -> Vec<String> {
        self.processes
            .into_iter()
            .map(|(_, result)| match result {
                Ok(detail) | Err(detail) => detail,
            })
            .chain(self.details)
            .chain(self.shortcuts)
            .chain(self.events)
            .chain(self.rescans.into_notices())
            .collect()
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExtensionSummary {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub manifest_digest: String,
    pub bundle_digest: Option<String>,
    pub source: ExtensionSource,
    pub enabled: bool,
    pub trusted: bool,
    pub running: bool,
    pub api_version: String,
    pub negotiated_features: Vec<String>,
    pub telemetry_schema: Option<String>,
    pub compatibility: String,
    pub health: Option<ExtensionHealthSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<octet_agent::extension_runtime::ExtensionRuntimeStatus>,
    pub tools: Vec<String>,
    pub commands: Vec<String>,
    pub hooks: Vec<ExtensionHook>,
    pub ui: Vec<ExtensionUiSurface>,
    /// Live secret-free API 0.3 provider declarations owned by this extension.
    pub providers: Vec<ExtensionProviderSummary>,
}

/// The options menu shown for one extension under `/extensions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionOptions {
    /// The extension's own menu, or entries generated from its commands.
    pub menu: octet_agent::ExtensionMenu,
    /// Generated entries ask for command arguments before running.
    pub generated: bool,
}

fn generated_options(process: &ExtensionProcess) -> ExtensionOptions {
    let items = process
        .contributions()
        .commands
        .iter()
        .map(|command| octet_agent::ExtensionMenuItem {
            id: format!("command:{}", command.name),
            label: command.name.clone(),
            description: Some(match &command.usage {
                Some(usage) => format!("{} · {usage}", command.description),
                None => command.description.clone(),
            }),
            command: Some(command.name.clone()),
            arguments: Vec::new(),
            destructive: false,
            recommended: false,
            items: None,
            detail: None,
        })
        .collect();
    ExtensionOptions {
        menu: octet_agent::ExtensionMenu {
            items,
            ..octet_agent::ExtensionMenu::default()
        },
        generated: true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ExtensionPresentationView {
    /// Manifest-bound extension that owns this state.
    pub extension: String,
    /// Active process generation; stale generations are discarded.
    pub generation: u64,
    /// Host-created process-instance fence. This prevents a replacement
    /// process whose generation counter restarted from accepting stale actions.
    pub extension_instance_id: String,
    /// Host-derived durable session owner for frontend isolation.
    pub resource_owner: Option<String>,
    /// Complete monotonic semantic snapshot.
    pub snapshot: ExtensionPresentationSnapshot,
}

#[derive(Default)]
struct BoundedDiagnostics {
    entries: VecDeque<String>,
    retained_bytes: usize,
    dropped: u64,
}

impl BoundedDiagnostics {
    fn push(&mut self, message: impl Into<String>) {
        let message = truncate_diagnostic(message.into());
        while !self.entries.is_empty()
            && (self.entries.len() >= MAX_DIAGNOSTIC_ENTRIES
                || self.retained_bytes.saturating_add(message.len()) > MAX_DIAGNOSTIC_BYTES)
        {
            if let Some(removed) = self.entries.pop_front() {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.len());
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        self.retained_bytes = self.retained_bytes.saturating_add(message.len());
        self.entries.push_back(message);
    }

    fn extend(&mut self, messages: impl IntoIterator<Item = String>) {
        for message in messages {
            self.push(message);
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.dropped == 0
    }

    fn iter(&self) -> impl Iterator<Item = &String> {
        self.entries.iter()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

fn truncate_diagnostic(message: String) -> String {
    const MARKER: &str = "\n[… diagnostic truncated …]";
    if message.len() <= MAX_DIAGNOSTIC_ENTRY_BYTES {
        return message.into_boxed_str().into_string();
    }
    let mut keep = MAX_DIAGNOSTIC_ENTRY_BYTES.saturating_sub(MARKER.len());
    while !message.is_char_boundary(keep) {
        keep = keep.saturating_sub(1);
    }
    let mut bounded = String::with_capacity(MAX_DIAGNOSTIC_ENTRY_BYTES);
    bounded.push_str(&message[..keep]);
    bounded.push_str(MARKER);
    bounded
}

#[derive(Default)]
struct PendingContext {
    entries: VecDeque<ContextContribution>,
    retained_bytes: usize,
}

impl PendingContext {
    fn try_push(&mut self, mut contribution: ContextContribution) -> Result<(), String> {
        let contribution_bytes = context_contribution_bytes(&contribution)?;
        if self.entries.len() >= MAX_PENDING_CONTEXT_ITEMS {
            return Err(format!(
                "pending context exceeds the {MAX_PENDING_CONTEXT_ITEMS} contribution limit"
            ));
        }
        if self.retained_bytes.saturating_add(contribution_bytes) > MAX_EXTENSION_CONTEXT_BYTES {
            return Err(format!(
                "pending context exceeds the {MAX_EXTENSION_CONTEXT_BYTES} byte aggregate limit"
            ));
        }
        contribution.label = contribution.label.into_boxed_str().into_string();
        contribution.content = contribution.content.into_boxed_str().into_string();
        self.retained_bytes = self.retained_bytes.saturating_add(contribution_bytes);
        self.entries.push_back(contribution);
        Ok(())
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &ContextContribution> {
        self.entries.iter()
    }

    #[cfg(test)]
    fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    fn commit(&mut self, count: usize) {
        for _ in 0..count.min(self.entries.len()) {
            if let Some(contribution) = self.entries.pop_front() {
                let bytes = context_contribution_bytes(&contribution).unwrap_or_default();
                self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
            }
        }
    }

    fn into_vec(self) -> Vec<ContextContribution> {
        self.entries.into_iter().collect()
    }
}

fn admit_context(
    pending_context: &mut PendingContext,
    diagnostics: &mut BoundedDiagnostics,
    source: &str,
    contribution: ContextContribution,
) -> bool {
    let label = contribution.label.clone();
    match pending_context.try_push(contribution) {
        Ok(()) => true,
        Err(error) => {
            diagnostics.push(format!(
                "warning: {source}: dropped extension context {label:?}: {error}"
            ));
            false
        }
    }
}

fn admit_presentation_owner(
    published: Option<octet_agent::extension_process::ExtensionResourceOwner>,
    active_owner: Option<&str>,
) -> Result<Option<String>, String> {
    let published = published.map(|owner| owner.session_id);
    if published.is_some() && published.as_deref() != active_owner {
        return Err("discarded semantic presentation for another resource owner".into());
    }
    Ok(published)
}

/// Bounded wire name used in typed refusal messages.
fn host_request_operation_name(operation: &HostRequestOperation) -> &'static str {
    match operation {
        HostRequestOperation::Composer(_) => "composer",
        HostRequestOperation::SessionEntry(_) => "session_entries",
        HostRequestOperation::MessageInjection(_) => "message_injection",
        HostRequestOperation::Shortcut { .. } => "shortcuts",
        HostRequestOperation::ActiveTools { .. } => "active_tools",
        HostRequestOperation::Terminal(_) => "terminal_handoff",
        HostRequestOperation::ContextSnapshot(operation) => match operation {
            ExtensionContextOperation::SessionManager => "session_manager",
            ExtensionContextOperation::PendingMessages => "pending_messages",
            ExtensionContextOperation::SystemPrompt => "system_prompt",
        },
    }
}

/// The negotiated feature that gates one host-mediated operation.
fn host_request_feature(operation: &HostRequestOperation) -> &'static str {
    match operation {
        HostRequestOperation::Composer(_) => EXTENSION_FEATURE_COMPOSER,
        HostRequestOperation::SessionEntry(_) => EXTENSION_FEATURE_SESSION_ENTRIES,
        HostRequestOperation::MessageInjection(_) => EXTENSION_FEATURE_MESSAGE_INJECTION,
        HostRequestOperation::Shortcut { .. } => EXTENSION_FEATURE_SHORTCUTS,
        HostRequestOperation::ActiveTools { .. } => EXTENSION_FEATURE_ACTIVE_TOOLS,
        HostRequestOperation::Terminal(_) => EXTENSION_FEATURE_TERMINAL_HANDOFF,
        HostRequestOperation::ContextSnapshot(ExtensionContextOperation::SystemPrompt) => {
            EXTENSION_FEATURE_SYSTEM_PROMPT_READ
        }
        HostRequestOperation::ContextSnapshot(_) => EXTENSION_FEATURE_SESSION_CONTEXT,
    }
}

/// One bounded, char-safe slice of a failure reason for a fixed-shape
/// extension notification.
fn bounded_notification_reason(reason: &str) -> &str {
    const MAX_NOTIFICATION_REASON_BYTES: usize = 512;
    if reason.len() <= MAX_NOTIFICATION_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_NOTIFICATION_REASON_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}

/// Bound one header/footer surface text to the semantic-UI disclosure cap,
/// truncating on a UTF-8 boundary rather than refusing the whole contribution.
fn bounded_surface_text(text: &str) -> String {
    const MAX_SURFACE_TEXT_BYTES: usize = 8 * 1024;
    if text.len() <= MAX_SURFACE_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_SURFACE_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The host request fence: an owner must exist and it must be the foreground
/// resource owner. A missing owner is a typed refusal, never a silent
/// coercion into the foreground session.
fn host_request_owner_is_foreground(
    owner: Option<&octet_agent::extension_process::ExtensionResourceOwner>,
    foreground: Option<&str>,
) -> bool {
    owner.is_some_and(|owner| foreground == Some(owner.session_id.as_str()))
}

/// Resolve one runtime shortcut binding. Host-reserved bindings are refused so
/// the product keymap always wins, and the refusal stays bounded and visible.
fn dynamic_shortcut_binding(
    key: &str,
) -> Result<ExtensionShortcutKey, (ExtensionRequestOutcome, String)> {
    let parsed = parse_extension_shortcut(key).map_err(|error| {
        (
            ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is not a supported binding: {error}"),
            ),
            format!("shortcut {key:?} was refused: {error}"),
        )
    })?;
    if is_reserved_extension_shortcut(&parsed) {
        return Err((
            ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is reserved by the host keymap"),
            ),
            format!("shortcut {key:?} was refused: the host keymap reserves this binding"),
        ));
    }
    Ok(parsed)
}

fn bounded_host_request_text(
    field: &str,
    text: &str,
    cap: usize,
) -> Result<(), (ExtensionRequestFailure, String)> {
    if text.len() > cap {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            format!("{field} exceeds {cap} bytes"),
        ));
    }
    Ok(())
}

fn bounded_host_request_name(
    field: &str,
    value: &str,
    cap: usize,
) -> Result<(), (ExtensionRequestFailure, String)> {
    if value.is_empty() {
        return Err((
            ExtensionRequestFailure::InvalidRequest,
            format!("{field} must not be empty"),
        ));
    }
    if value.len() > cap {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            format!("{field} exceeds {cap} bytes"),
        ));
    }
    Ok(())
}

/// Validate one extension request against the negotiated agent-side caps. No
/// payload reaches host state until this passes.
fn validate_host_request(
    operation: &HostRequestOperation,
) -> Result<(), (ExtensionRequestFailure, String)> {
    match operation {
        HostRequestOperation::Composer(operation) => match operation {
            ExtensionComposerOperation::Get => Ok(()),
            ExtensionComposerOperation::Set { text }
            | ExtensionComposerOperation::Insert { text } => {
                bounded_host_request_text("composer text", text, MAX_EXTENSION_COMPOSER_TEXT_BYTES)
            }
        },
        HostRequestOperation::SessionEntry(operation) => match operation {
            ExtensionSessionEntryOperation::Append { entry_type, data } => {
                bounded_host_request_name(
                    "entry_type",
                    entry_type,
                    MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES,
                )?;
                let encoded = serde_json::to_string(data).map_err(|error| {
                    (
                        ExtensionRequestFailure::InvalidRequest,
                        format!("entry data is not serializable: {error}"),
                    )
                })?;
                if encoded.len() > MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES {
                    return Err((
                        ExtensionRequestFailure::BoundsExceeded,
                        format!(
                            "entry data exceeds {MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES} bytes"
                        ),
                    ));
                }
                Ok(())
            }
            ExtensionSessionEntryOperation::SetName { name } => {
                bounded_host_request_name("session name", name, MAX_EXTENSION_SESSION_NAME_BYTES)
            }
            ExtensionSessionEntryOperation::SetLabel { entry_id, label } => {
                bounded_host_request_name("entry id", entry_id, MAX_EXTENSION_UI_KEY_BYTES)?;
                bounded_host_request_text("entry label", label, MAX_EXTENSION_SESSION_LABEL_BYTES)
            }
        },
        HostRequestOperation::MessageInjection(injection) => match injection {
            ExtensionMessageInjection::Assistant { text }
            | ExtensionMessageInjection::System { text }
            | ExtensionMessageInjection::User { text } => bounded_host_request_text(
                "injected message text",
                text,
                MAX_EXTENSION_INJECTED_MESSAGE_BYTES,
            ),
        },
        HostRequestOperation::Shortcut {
            shortcut_id,
            key,
            description,
        } => {
            bounded_host_request_name("shortcut id", shortcut_id, MAX_EXTENSION_SHORTCUT_ID_BYTES)?;
            bounded_host_request_name("shortcut key", key, MAX_EXTENSION_SHORTCUT_KEY_BYTES)?;
            bounded_host_request_text(
                "shortcut description",
                description,
                MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES,
            )
        }
        HostRequestOperation::ActiveTools { names } => {
            if names.len() > MAX_HOST_REQUEST_TOOL_NAMES {
                return Err((
                    ExtensionRequestFailure::BoundsExceeded,
                    format!("tool name list exceeds {MAX_HOST_REQUEST_TOOL_NAMES} entries"),
                ));
            }
            for name in names {
                bounded_host_request_name("tool name", name, MAX_EXTENSION_UI_KEY_BYTES)?;
            }
            Ok(())
        }
        // The handoff carries no payload: the host mints the grant and reports
        // the size it left the terminal in, so there is nothing to bound here.
        HostRequestOperation::Terminal(_) => Ok(()),
        // Read-only snapshots take no caller payload; the reply is bounded at
        // the point the host composes it.
        HostRequestOperation::ContextSnapshot(_) => Ok(()),
    }
}

fn reduce_presentation_update(
    presentations: &mut BTreeMap<String, ExtensionPresentationView>,
    extension: String,
    active_generation: Option<u64>,
    extension_instance_id: String,
    resource_owner: Option<String>,
    generation: u64,
    snapshot: ExtensionPresentationSnapshot,
) -> Result<String, String> {
    if active_generation != Some(generation) {
        return Err(format!(
            "discarded semantic presentation from stale generation {generation}"
        ));
    }
    if presentations
        .get(&extension)
        .is_some_and(|view| view.extension_instance_id != extension_instance_id)
    {
        presentations.remove(&extension);
    }
    if presentations.get(&extension).is_some_and(|view| {
        view.generation == generation && view.resource_owner.is_some() && resource_owner.is_none()
    }) {
        return Err(
            "discarded process-scoped semantic presentation while owner-scoped state is active"
                .into(),
        );
    }
    if presentations
        .get(&extension)
        .is_some_and(|view| view.resource_owner != resource_owner)
    {
        presentations.remove(&extension);
    }
    if presentations.get(&extension).is_some_and(|view| {
        view.generation > generation
            || (view.generation == generation && view.snapshot.revision >= snapshot.revision)
    }) {
        return Err(format!(
            "discarded stale semantic presentation revision {} for generation {generation}",
            snapshot.revision
        ));
    }
    let compact = snapshot
        .status
        .as_ref()
        .map(|status| status.label.clone())
        .or_else(|| {
            snapshot
                .activities
                .last()
                .map(|activity| activity.summary.clone())
        })
        .unwrap_or_else(|| "presentation updated".to_owned());
    presentations.insert(
        extension.clone(),
        ExtensionPresentationView {
            extension,
            generation,
            extension_instance_id,
            resource_owner,
            snapshot,
        },
    );
    Ok(compact)
}

/// A host-validated terminal shortcut target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtensionShortcutInvocation {
    pub(crate) extension: String,
    pub(crate) name: String,
    pub(crate) description: String,
}

#[derive(Clone, Debug)]
struct RegisteredExtensionShortcut {
    key: ExtensionShortcutKey,
    invocation: ExtensionShortcutInvocation,
}

fn register_extension_shortcut(
    registered: &mut Vec<RegisteredExtensionShortcut>,
    diagnostics: &mut Vec<String>,
    extension: &str,
    shortcut: &ShortcutDefinition,
) {
    let key = match parse_extension_shortcut(&shortcut.key) {
        Ok(key) => key,
        Err(error) => {
            diagnostics.push(format!(
                "warning: extension {extension:?} shortcut {:?} was not registered: {error}",
                shortcut.key
            ));
            return;
        }
    };
    if is_reserved_extension_shortcut(&key) {
        diagnostics.push(format!(
            "warning: extension {extension:?} shortcut {:?} was not registered: binding is reserved by octet",
            shortcut.key
        ));
        return;
    }
    if let Some(existing) = registered
        .iter()
        .find(|existing: &&RegisteredExtensionShortcut| existing.key == key)
    {
        diagnostics.push(format!(
            "warning: extension {extension:?} shortcut {:?} conflicts with {}:{}; the first binding remains active",
            shortcut.key, existing.invocation.extension, existing.invocation.name
        ));
        return;
    }
    registered.push(RegisteredExtensionShortcut {
        key,
        invocation: ExtensionShortcutInvocation {
            extension: extension.to_owned(),
            name: shortcut.name.clone(),
            description: shortcut.description.clone(),
        },
    });
}

fn register_extension_shortcuts(
    processes: &[ExtensionProcess],
) -> (Vec<RegisteredExtensionShortcut>, Vec<String>) {
    let mut registered = Vec::new();
    let mut diagnostics = Vec::new();
    for process in processes {
        let extension = &process.descriptor().manifest.name;
        for shortcut in &process.contributions().shortcuts {
            register_extension_shortcut(&mut registered, &mut diagnostics, extension, shortcut);
        }
    }
    (registered, diagnostics)
}

async fn execute_headless_command(
    process: &ExtensionProcess,
    name: &str,
    arguments: Vec<String>,
    execution_context: octet_agent::extension_process::ExtensionExecutionContext,
    mut approval_budget: usize,
    diagnostics: &mut BoundedDiagnostics,
) -> anyhow::Result<octet_agent::extension_process::CommandOutput> {
    let extension_name = &process.descriptor().manifest.name;
    let mut events = process.subscribe();
    let (output, confirmation_denied) = {
        let mut confirmation_denied = false;
        let output = {
            let legacy_uncorrelated = process.api_version() == EXTENSION_API_VERSION_0_1;
            let (request_started, started) = tokio::sync::oneshot::channel();
            let mut started = Box::pin(started);
            let mut operation = None;
            let cancellation_token = CancellationToken::default();
            let (progress_sink, _progress_rx) = ToolProgressSink::bounded_channel();
            let mut execution = Box::pin(process.execute_command_controlled_with_progress(
                name.to_owned(),
                arguments,
                execution_context,
                cancellation_token,
                progress_sink,
                request_started,
            ));
            let mut events_open = true;
            loop {
                tokio::select! {
                    result = &mut execution => break result?,
                    started = &mut started, if operation.is_none() => match started {
                        Ok(started) => operation = Some(started),
                        Err(_) => break execution.await?,
                    },
                    event = events.recv(), if events_open && operation.is_some() => match event {
                        Ok(ExtensionEvent::ConfirmationRequested {
                            request_id,
                            generation,
                            parent_request_id,
                            ..
                        }) if parent_request_id.is_some_and(|parent| {
                            operation.is_some_and(|operation| operation.owns(generation, parent))
                        }) || (legacy_uncorrelated
                            && parent_request_id.is_none()
                            && operation.is_some_and(|operation| operation.generation == generation)) => {
                            if !process.confirmation_answered(&request_id, generation) {
                                let confirmed = approval_budget > 0;
                                if confirmed {
                                    approval_budget -= 1;
                                } else {
                                    confirmation_denied = true;
                                }
                                process
                                    .respond_to_confirmation(
                                        request_id,
                                        generation,
                                        ConfirmationResponse { confirmed },
                                    )
                                    .await?;
                            }
                        }
                        Ok(ExtensionEvent::PolicyEvaluationRequested { .. }) => {}
                        Ok(ExtensionEvent::InputRequested {
                            request_id,
                            generation,
                            parent_request_id,
                            ..
                        }) if operation.is_some_and(|operation| {
                            operation.owns(generation, parent_request_id)
                        }) => {
                            process
                                .respond_to_input(
                                    request_id,
                                    generation,
                                    ExtensionInputResponse { value: None },
                                )
                                .await?;
                        }
                        Ok(_) => {
                            // The persistent receiver owns ordinary notifications,
                            // status, context, and diagnostics.
                        }
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            diagnostics.push(format!(
                                "warning: {extension_name}: confirmation listener lagged by {count} events"
                            ));
                        }
                        Err(broadcast::error::RecvError::Closed) => events_open = false,
                    },
                }
            }
        };
        (output, confirmation_denied)
    };
    if confirmation_denied {
        anyhow::bail!("extension command {name:?} requires an interactive confirmation surface");
    }
    Ok(output)
}

async fn execute_shortcut_headless(
    process: ExtensionProcess,
    name: String,
    execution_context: octet_agent::extension_process::ExtensionExecutionContext,
) -> (String, Vec<ContextContribution>, Vec<String>) {
    let extension = process.descriptor().manifest.name.clone();
    let mut events = process.subscribe();
    let (request_started, started) = tokio::sync::oneshot::channel();
    let mut started = Box::pin(started);
    let mut operation = None;
    let mut execution = Box::pin(process.execute_shortcut_controlled(
        name.clone(),
        execution_context,
        request_started,
    ));
    let mut events_open = true;
    let mut confirmation_denied = false;
    let mut confirmation_state_uncertain = false;
    let output = loop {
        tokio::select! {
            result = &mut execution => break result,
            started = &mut started, if operation.is_none() => match started {
                Ok(started) => operation = Some(started),
                Err(_) => break execution.await,
            },
            event = events.recv(), if events_open && operation.is_some() => match event {
                Ok(ExtensionEvent::ConfirmationRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    ..
                }) if parent_request_id.is_some_and(|parent| {
                    operation.is_some_and(|operation| operation.owns(generation, parent))
                }) => {
                    // The persistent event drain may have denied this first;
                    // either way, a background shortcut never owns an approval UI.
                    confirmation_denied = true;
                    if !process.confirmation_answered(&request_id, generation) {
                        let _ = process.respond_to_confirmation(
                            request_id,
                            generation,
                            ConfirmationResponse { confirmed: false },
                        ).await;
                    }
                }
                Ok(ExtensionEvent::InputRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    ..
                }) if operation.is_some_and(|operation| operation.owns(generation, parent_request_id)) => {
                    if !process.input_answered(&request_id, generation) {
                        let _ = process.respond_to_input(
                            request_id,
                            generation,
                            ExtensionInputResponse { value: None },
                        ).await;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    confirmation_state_uncertain = true;
                }
                Err(broadcast::error::RecvError::Closed) => events_open = false,
            },
        }
    };
    if operation.is_none() {
        if let Ok(started) = started.as_mut().get_mut().try_recv() {
            operation = Some(started);
        }
    }
    if let Some(operation) = operation {
        loop {
            match events.try_recv() {
                Ok(ExtensionEvent::ConfirmationRequested {
                    request_id,
                    generation,
                    parent_request_id,
                    ..
                }) if parent_request_id
                    .is_some_and(|parent| operation.owns(generation, parent)) =>
                {
                    // A response may already have been sent by the persistent
                    // drain. It was necessarily a denial for this background
                    // operation, so never admit its output or context.
                    confirmation_denied = true;
                    if !process.confirmation_answered(&request_id, generation) {
                        let _ = process
                            .respond_to_confirmation(
                                request_id,
                                generation,
                                ConfirmationResponse { confirmed: false },
                            )
                            .await;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty)
                | Err(broadcast::error::TryRecvError::Closed) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    confirmation_state_uncertain = true;
                }
            }
        }
    } else if output.is_ok() {
        // A successful admitted request always reports its operation token. If
        // it did not, do not accept output that cannot be scoped safely.
        confirmation_state_uncertain = true;
    }
    match output {
        Ok(_) if confirmation_denied => {
            let message = format!(
                "extension shortcut {extension:?}/{name:?} requires an interactive confirmation and was denied"
            );
            (extension, Vec::new(), vec![message])
        }
        Ok(_) if confirmation_state_uncertain => (
            extension,
            Vec::new(),
            vec![format!(
                "extension shortcut {name:?} output was discarded because its confirmation state could not be verified"
            )],
        ),
        Ok(output) => {
            let mut messages = Vec::new();
            if !output.text.trim().is_empty() {
                messages.push(output.text);
            }
            messages.extend(
                output
                    .notifications
                    .iter()
                    .map(|notification| format_notification(&name, notification)),
            );
            (extension, output.context, messages)
        }
        Err(error) => (
            extension,
            Vec::new(),
            vec![format!("extension shortcut {name:?} failed: {error}")],
        ),
    }
}

/// Product-owned authorization boundary for API 0.3 extension providers.
///
/// The coding agent does not currently expose a credential or OAuth setup
/// surface for extension providers. Explicitly park those routes instead of
/// allowing an extension declaration to imply credential authority. The
/// registry handles unauthenticated providers without consulting this policy.
#[derive(Default)]
struct CodingAgentProviderAuthorizationPolicy;

impl ExtensionProviderAuthorizationPolicy for CodingAgentProviderAuthorizationPolicy {
    fn authorize(
        &self,
        _owner: &ExtensionProviderOwner,
        _provider: &octet_agent::extension_api_v03::ProviderDefinition,
        request: &octet_agent::extension_api_v03::ProviderAuthorizationRequest,
    ) -> octet_agent::extension_api_v03::ProviderAuthorizationResult {
        octet_agent::extension_api_v03::ProviderAuthorizationResult {
            status: if request.action == "revoke" {
                "revoked"
            } else {
                "unavailable"
            }
            .to_owned(),
            lease: None,
        }
    }
}

#[derive(Default)]
struct ProviderCatalogProjection {
    revision: Option<usize>,
    /// Model ids the last synchronization attempted to project. A desired
    /// model the catalog refused (for example a conflicting identifier) stays
    /// here so an unchanged registry is never re-projected on every boundary.
    desired: BTreeSet<String>,
    routes: BTreeMap<String, EndpointId>,
    problems: Vec<String>,
}

#[derive(Default)]
pub(crate) struct ProviderCatalogReport {
    pub checked: bool,
    pub problems: Vec<String>,
    pub details: Vec<String>,
}

impl ProviderCatalogReport {
    fn into_notices(self) -> Vec<String> {
        if self.checked {
            self.problems.into_iter().chain(self.details).collect()
        } else {
            Vec::new()
        }
    }
}

/// Shared owner for extension-provider declarations and their local catalog
/// projection. The registry itself stores only secret-free declarations; this
/// product layer synthesizes opaque local endpoints and host stream routes.
#[derive(Clone)]
pub(crate) struct ExtensionProviderRuntime {
    registry: Arc<ExtensionProviderRegistry>,
    projection: Arc<Mutex<ProviderCatalogProjection>>,
}

impl Default for ExtensionProviderRuntime {
    fn default() -> Self {
        Self {
            registry: Arc::new(ExtensionProviderRegistry::with_authorization_policy(
                Arc::new(CodingAgentProviderAuthorizationPolicy),
            )),
            projection: Arc::new(Mutex::new(ProviderCatalogProjection::default())),
        }
    }
}

impl ExtensionProviderRuntime {
    fn registry(&self) -> Arc<ExtensionProviderRegistry> {
        Arc::clone(&self.registry)
    }

    /// Returns every recorded declaration owned by one extension instance,
    /// paired with whether the owning generation's initial batch completed.
    fn recorded_providers_for(
        &self,
        instance_id: &str,
    ) -> Vec<(ExtensionProviderCatalogEntry, bool)> {
        self.registry
            .recorded_providers()
            .into_iter()
            .filter(|(entry, _)| entry.owner.extension_instance_id == instance_id)
            .collect()
    }

    fn initial_provider_owners(processes: &[ExtensionProcess]) -> Vec<ExtensionProviderOwner> {
        processes
            .iter()
            .filter(|process| process.contributions().providers)
            .map(|process| ExtensionProviderOwner {
                extension_instance_id: process.extension_instance_id().to_owned(),
                generation: process.health_snapshot().generation,
            })
            .collect()
    }

    fn await_initial_registrations(&self, processes: &[ExtensionProcess]) {
        let owners = Self::initial_provider_owners(processes);
        // A completion notification follows every bridge-owned initial batch,
        // including an empty one. Extensions predating that additive API surface
        // time out here and their incomplete declarations remain unprojected.
        let _ = self
            .registry
            .wait_for_owners(&owners, PROVIDER_REGISTRATION_BARRIER);
    }

    async fn await_initial_registrations_async(&self, processes: &[ExtensionProcess]) {
        let owners = Self::initial_provider_owners(processes);
        if owners.is_empty() {
            return;
        }
        let registry = Arc::clone(&self.registry);
        // Reload runs from the Tokio runtime. Keep its completion wait off a
        // core worker so post-initialize reverse registration can make progress
        // even when the runtime has only one configured worker thread.
        let _ = tokio::task::spawn_blocking(move || {
            registry.wait_for_owners(&owners, PROVIDER_REGISTRATION_BARRIER)
        })
        .await;
    }

    /// Reconciles ready provider declarations into the local model catalog.
    ///
    /// Each synthesized endpoint is fenced by the owner instance and process
    /// generation. A replacement or authorization revocation therefore removes
    /// the old model and host stream transport before a newer route is made
    /// selectable. Extension-declared URLs, headers, credentials, and leases
    /// never enter this projection.
    fn synchronize(
        &self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
        processes: &[ExtensionProcess],
    ) -> Vec<String> {
        self.synchronize_report(catalog, client, processes)
            .into_notices()
    }

    fn synchronize_report(
        &self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
        processes: &[ExtensionProcess],
    ) -> ProviderCatalogReport {
        let (revision, entries) = self.registry.snapshot();
        let desired = entries
            .iter()
            .filter(|entry| entry.authorization == ExtensionProviderAuthorizationStatus::Ready)
            .flat_map(|entry| {
                entry.models.iter().filter_map(|model| {
                    extension_provider_protocol(&model.protocol)
                        .map(|_| extension_provider_model_id(&entry.provider.id, &model.id).0)
                })
            })
            .collect::<BTreeSet<_>>();
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = projection.revision == Some(revision)
            && projection.desired == desired
            && projection.routes.iter().all(|(model, endpoint)| {
                catalog
                    .resolve(&ModelId(model.clone()))
                    .is_ok_and(|registered| registered.endpoint.id == *endpoint)
            });
        if current {
            return ProviderCatalogReport {
                problems: projection.problems.clone(),
                ..Default::default()
            };
        }

        // A synchronization after the first projection is a live change to a
        // running session. Models that were not projected before are reported
        // through the existing notice surface; the first projection stays quiet.
        let live_change = projection.revision.is_some();
        let previously_projected = projection.routes.keys().cloned().collect::<BTreeSet<_>>();
        let mut newly_live = Vec::new();
        for (model, endpoint) in std::mem::take(&mut projection.routes) {
            let model = ModelId(model);
            catalog.remove_model_if_endpoint(&model, &endpoint);
            client.remove_host_stream_transport(&endpoint);
            catalog.remove_endpoint_if_unused(&endpoint);
        }

        let mut diagnostics = Vec::new();
        for entry in entries {
            if entry.authorization != ExtensionProviderAuthorizationStatus::Ready {
                continue;
            }
            let Some(process) = processes.iter().find(|process| {
                process.extension_instance_id() == entry.owner.extension_instance_id
                    && process.health_snapshot().generation == entry.owner.generation
                    && process.is_running()
            }) else {
                diagnostics.push(format!(
                    "warning: extension provider {:?} has no live owning process",
                    entry.provider.id
                ));
                continue;
            };

            for provider_model in entry.models {
                let Some(protocol) = extension_provider_protocol(&provider_model.protocol) else {
                    diagnostics.push(format!(
                        "warning: extension provider {:?} model {:?} declares an unsupported protocol",
                        entry.provider.id, provider_model.id
                    ));
                    continue;
                };
                let Some(route) = self
                    .registry
                    .resolve(&entry.provider.id, &provider_model.id)
                else {
                    continue;
                };
                if route.owner != entry.owner || route.model != provider_model {
                    continue;
                }

                let model_id = extension_provider_model_id(&entry.provider.id, &provider_model.id);
                if catalog.resolve(&model_id).is_ok() {
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} conflicts with an existing catalog model",
                        model_id.0
                    ));
                    continue;
                }
                let endpoint_id = extension_provider_endpoint_id(
                    &entry.owner,
                    &entry.provider.id,
                    &provider_model.id,
                );
                if catalog.has_endpoint(&endpoint_id) {
                    diagnostics.push(
                        "warning: extension provider endpoint identity conflicts with an existing catalog endpoint"
                            .to_owned(),
                    );
                    continue;
                }
                let (Ok(context_window), Ok(max_output_tokens)) = (
                    u64::try_from(provider_model.context_window),
                    u64::try_from(provider_model.max_output_tokens),
                ) else {
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} has token limits outside the local catalog range",
                        model_id.0
                    ));
                    continue;
                };
                let endpoint = Endpoint {
                    id: endpoint_id.clone(),
                    // This inert local URL is required by ModelCatalog's
                    // endpoint invariant. AiClient dispatches the registered
                    // host transport before any HTTP codec can inspect it.
                    base_url: url::Url::parse("http://127.0.0.1:9/")
                        .expect("fixed extension-provider endpoint URL is valid"),
                    auth: Auth::None,
                    default_headers: http::HeaderMap::new(),
                    transport: EndpointTransport::Http,
                    runtime: RequestRuntime::default(),
                    timeout: Duration::from_secs(30),
                };
                if catalog.register_endpoint(endpoint).is_err() {
                    diagnostics.push(
                        "warning: extension provider endpoint could not be registered".to_owned(),
                    );
                    continue;
                }
                let _ =
                    catalog.set_endpoint_label(endpoint_id.clone(), entry.provider.label.clone());
                let specification = ModelSpec {
                    id: model_id.clone(),
                    endpoint: endpoint_id.clone(),
                    api_name: provider_model.api_name.clone(),
                    display_name: provider_model.display_name.clone(),
                    protocol,
                    capabilities: extension_provider_capabilities(&provider_model.capabilities),
                    limits: ModelLimits {
                        context_window,
                        max_output_tokens,
                    },
                    pricing: None,
                    // API 0.3 provider declarations carry no model presets or
                    // HTTP headers; never infer unnegotiated transport authority.
                    preset: Default::default(),
                    cache: CacheCompatibility {
                        supports_long_retention: false,
                        supports_explicit_prompt_cache_mode: false,
                        send_session_id_header: false,
                        send_session_affinity_headers: false,
                        session_affinity_format: None,
                        cache_control_format: None,
                        supports_cache_control_on_tools: false,
                    },
                };
                if catalog.register_model(specification).is_err() {
                    catalog.remove_endpoint_if_unused(&endpoint_id);
                    diagnostics.push(format!(
                        "warning: extension provider model {:?} could not be registered",
                        model_id.0
                    ));
                    continue;
                }
                client.register_host_stream_transport(
                    endpoint_id.clone(),
                    process.provider_stream_transport(entry.provider.id.clone(), provider_model.id),
                );
                if live_change && !previously_projected.contains(&model_id.0) {
                    newly_live.push(model_id.0.clone());
                }
                projection.routes.insert(model_id.0, endpoint_id);
            }
        }
        projection.revision = Some(revision);
        projection.desired = desired;
        projection.problems = diagnostics.clone();
        let mut details = Vec::new();
        for (index, model) in newly_live.iter().enumerate() {
            if index == MAX_LIVE_REGISTRATION_NOTICES {
                details.push(format!(
                    "extension provider: {} more model(s) registered while this session was running",
                    newly_live.len().saturating_sub(MAX_LIVE_REGISTRATION_NOTICES)
                ));
                break;
            }
            details.push(format!(
                "extension provider model {model:?} is now live; it is available for the next request"
            ));
        }
        ProviderCatalogReport {
            checked: true,
            problems: diagnostics,
            details,
        }
    }

    /// Removes only routes this runtime previously projected, including their
    /// host-stream transport registrations.
    fn clear(&self, catalog: &mut ModelCatalog, client: &AiClient) {
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (model, endpoint) in std::mem::take(&mut projection.routes) {
            catalog.remove_model_if_endpoint(&ModelId(model), &endpoint);
            client.remove_host_stream_transport(&endpoint);
            catalog.remove_endpoint_if_unused(&endpoint);
        }
        projection.revision = None;
        projection.desired.clear();
    }
}

fn extension_provider_model_id(provider_id: &str, model_id: &str) -> ModelId {
    ModelId(format!("{provider_id}/{model_id}"))
}

fn extension_provider_endpoint_id(
    owner: &ExtensionProviderOwner,
    provider_id: &str,
    model_id: &str,
) -> EndpointId {
    let mut digest = Sha256::new();
    digest.update(b"octet-coding-agent-extension-provider-endpoint-v1\0");
    digest.update(owner.extension_instance_id.as_bytes());
    digest.update([0]);
    digest.update(owner.generation.to_le_bytes());
    digest.update(provider_id.as_bytes());
    digest.update([0]);
    digest.update(model_id.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    EndpointId(format!("extension-provider-{}", &digest[..32]))
}

fn extension_provider_protocol(protocol: &str) -> Option<Protocol> {
    match protocol {
        "openai_chat" => Some(Protocol::OpenAiChat),
        "openai_responses" => Some(Protocol::OpenAiResponses),
        "anthropic_messages" => Some(Protocol::AnthropicMessages),
        _ => None,
    }
}

fn extension_provider_capabilities(
    capabilities: &octet_agent::extension_api_v03::ProviderModelCapabilities,
) -> Capabilities {
    Capabilities {
        responses_features: Default::default(),
        input_modalities: Default::default(),
        output_modalities: Default::default(),
        tools: capabilities.tools,
        parallel_tool_calls: capabilities.parallel_tool_calls,
        reasoning: capabilities.reasoning.then_some(ReasoningCapability {
            options: None,
            control: ReasoningControl::Effort,
            exposes_text: true,
            preserves_state: false,
            effort_budgets: None,
            openai_chat_mode: OpenAiChatReasoningMode::Standard,
            min_effort: ReasoningEffort::Minimal,
            max_effort: ReasoningEffort::High,
        }),
        responses_lite: false,
        agent_delegation: None,
        structured_output: capabilities.structured_output,
        deferred_tool_loading: false,
    }
}

pub struct ExecutableExtensions {
    telemetry: Option<octet_agent::TelemetryObserver>,
    telemetry_rejected: u64,
    telemetry_error: Option<std::io::ErrorKind>,
    processes: Vec<ExtensionProcess>,
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

pub struct ExtensionTurnLifecycle {
    processes: Vec<ExtensionProcess>,
    resource_owner: String,
    session_id: String,
    run_id: String,
    turn_id: String,
    started_at: Instant,
    started_delivery: watch::Receiver<bool>,
    settled: bool,
    #[cfg(test)]
    lifecycle_delivery_test_control: Option<std::sync::Arc<LifecycleDeliveryTestControl>>,
}

/// Per-instance cancellation barrier used only by lifecycle ownership tests.
/// Keeping this on the owning `ExecutableExtensions` avoids global hooks and
/// lets the rest of the test suite continue to run in parallel.
#[cfg(test)]
#[derive(Default)]
struct LifecycleDeliveryTestControl {
    gate_turn_started: std::sync::atomic::AtomicBool,
    turn_started_entered: tokio::sync::Notify,
    turn_started_release: tokio::sync::Notify,
    gate_turn_settled: std::sync::atomic::AtomicBool,
    turn_settled_entered: tokio::sync::Notify,
    turn_settled_release: tokio::sync::Notify,
}

#[cfg(test)]
impl LifecycleDeliveryTestControl {
    /// Only the lifecycle-delivery suite drives these barriers, and that suite
    /// runs real extension processes, so it exists on unix builds only. The
    /// hook itself stays everywhere because the product code that calls
    /// `wait_before_delivery` is not platform-specific.
    #[cfg(unix)]
    fn gate_turn_started(&self) {
        self.gate_turn_started.store(true, Ordering::Release);
    }

    #[cfg(unix)]
    fn release_turn_started(&self) {
        self.turn_started_release.notify_one();
    }

    #[cfg(unix)]
    async fn turn_started_entered(&self) {
        self.turn_started_entered.notified().await;
    }

    #[cfg(unix)]
    fn gate_turn_settled(&self) {
        self.gate_turn_settled.store(true, Ordering::Release);
    }

    #[cfg(unix)]
    fn release_turn_settled(&self) {
        self.turn_settled_release.notify_one();
    }

    #[cfg(unix)]
    async fn turn_settled_entered(&self) {
        self.turn_settled_entered.notified().await;
    }

    async fn wait_before_delivery(&self, event: &ExtensionLifecycleEvent) {
        match event {
            ExtensionLifecycleEvent::TurnStarted { .. }
                if self.gate_turn_started.load(Ordering::Acquire) =>
            {
                self.turn_started_entered.notify_one();
                self.turn_started_release.notified().await;
            }
            ExtensionLifecycleEvent::TurnSettled { .. }
                if self.gate_turn_settled.load(Ordering::Acquire) =>
            {
                self.turn_settled_entered.notify_one();
                self.turn_settled_release.notified().await;
            }
            _ => {}
        }
    }
}

impl ExtensionTurnLifecycle {
    async fn settle(
        mut self,
        outcome: ExtensionLifecycleOutcome,
        reason: Option<String>,
    ) -> Vec<String> {
        let processes = self.processes.clone();
        let resource_owner = self.resource_owner.clone();
        let turn_id = self.turn_id.clone();
        let event = ExtensionLifecycleEvent::TurnSettled {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            turn_id: self.turn_id.clone(),
            outcome,
            duration_ms: duration_millis(self.started_at.elapsed()),
            reason,
        };
        #[cfg(test)]
        let lifecycle_delivery_test_control = self.lifecycle_delivery_test_control.clone();
        // Transfer terminal delivery to an owned task before this future can
        // be cancelled. Dropping the JoinHandle detaches rather than aborts
        // the task, so every admitted turn retains one terminal owner.
        let delivery = tokio::spawn(async move {
            #[cfg(test)]
            if let Some(control) = lifecycle_delivery_test_control {
                control.wait_before_delivery(&event).await;
            }
            let diagnostics = notify_lifecycle_all(&processes, event).await;
            for process in &processes {
                process.clear_active_lifecycle_turn(&resource_owner, &turn_id);
            }
            diagnostics
        });
        self.settled = true;
        match delivery.await {
            Ok(diagnostics) => diagnostics,
            Err(error) => vec![format!(
                "warning: extension turn lifecycle task failed: {error}"
            )],
        }
    }
}

impl Drop for ExtensionTurnLifecycle {
    fn drop(&mut self) {
        if self.settled || self.processes.is_empty() {
            return;
        }
        let Ok(handle) = Handle::try_current() else {
            for process in &self.processes {
                process.clear_active_lifecycle_turn(&self.resource_owner, &self.turn_id);
            }
            return;
        };
        let processes = self.processes.clone();
        let resource_owner = self.resource_owner.clone();
        let turn_id = self.turn_id.clone();
        let mut started_delivery = self.started_delivery.clone();
        let event = ExtensionLifecycleEvent::TurnSettled {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            turn_id: self.turn_id.clone(),
            outcome: ExtensionLifecycleOutcome::FrontendDisconnected,
            duration_ms: duration_millis(self.started_at.elapsed()),
            reason: Some("turn owner dropped before explicit settlement".into()),
        };
        #[cfg(test)]
        let lifecycle_delivery_test_control = self.lifecycle_delivery_test_control.clone();
        handle.spawn(async move {
            let _ = started_delivery.wait_for(|started| *started).await;
            #[cfg(test)]
            if let Some(control) = lifecycle_delivery_test_control {
                control.wait_before_delivery(&event).await;
            }
            let _ = notify_lifecycle_all(&processes, event).await;
            for process in &processes {
                process.clear_active_lifecycle_turn(&resource_owner, &turn_id);
            }
        });
    }
}

struct PendingConfirmationDenial {
    process: ExtensionProcess,
    request_id: ExtensionRequestId,
    generation: u64,
}

struct PendingInputCancellation {
    process: ExtensionProcess,
    request_id: ExtensionRequestId,
    generation: u64,
}

struct PendingEditorRequest {
    process: ExtensionProcess,
    request_id: ExtensionRequestId,
    generation: u64,
    request: ExtensionEditorRequest,
}

/// One admitted extension request that a host surface still has to answer.
/// Every entry is answered exactly once, or dropped with a bounded diagnostic
/// when its process generation is gone.
struct PendingHostRequest {
    process: ExtensionProcess,
    request_id: ExtensionRequestId,
    generation: u64,
    operation: HostRequestOperation,
}

impl PendingHostRequest {
    fn discard_notice(&self) -> Option<String> {
        (!self.process.is_running() || self.process.health_snapshot().generation != self.generation)
            .then(|| {
                format!(
                    "warning: {}: discarded host-owned extension request from stale generation {}",
                    self.process.descriptor().manifest.name,
                    self.generation
                )
            })
    }
}

/// Host-mediated operations an extension may request. Each one is gated on a
/// negotiated additive feature and on foreground resource ownership.
enum HostRequestOperation {
    Composer(ExtensionComposerOperation),
    SessionEntry(ExtensionSessionEntryOperation),
    MessageInjection(ExtensionMessageInjection),
    Shortcut {
        shortcut_id: String,
        key: String,
        description: String,
    },
    ActiveTools {
        names: Vec<String>,
    },
    Terminal(ExtensionTerminalOperation),
    /// A read-only foreground context snapshot. `SystemPrompt` is resolved by
    /// the product loop that owns the agent; the other operations resolve
    /// against the live shell and the cached host state.
    ContextSnapshot(ExtensionContextOperation),
}

/// Identity of the caller that owns, or wants, the foreground terminal grant.
///
/// The grant is fenced by foreground resource owner, extension instance and
/// process generation, so a reload, a restarted process or a switched session
/// can never inherit or release someone else's granted tty.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalHolder {
    /// Host-derived foreground resource owner, or process scope when absent.
    owner: Option<String>,
    /// Extension instance that owns the holder process.
    instance_id: String,
    /// Process generation admitted for this grant.
    generation: u64,
    /// Manifest name, used only for bounded diagnostics.
    name: String,
}

/// One live foreground terminal grant. The host keeps the record so it can
/// always recognise the holder, and always take the terminal back.
struct ActiveTerminalGrant {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain the exact issued grant ID with its owner for terminal grant diagnostics."
        )
    )]
    grant_id: String,
    holder: TerminalHolder,
}

/// Single-slot arbiter for the one foreground raw terminal the host can cede.
///
/// The terminal stays host-owned: a second acquire is refused while a grant is
/// live, only the recorded holder can release it, and every refusal is typed so
/// no request is ever silently dropped. Revocation never waits on the previous
/// holder.
#[derive(Default)]
struct TerminalGrantArbiter {
    active: Option<ActiveTerminalGrant>,
}

impl TerminalGrantArbiter {
    fn active(&self) -> Option<&ActiveTerminalGrant> {
        self.active.as_ref()
    }

    /// Admit one acquire. The host mints the grant id and reports the size it
    /// left the terminal in; it never trusts a child-supplied identity.
    fn acquire(
        &mut self,
        holder: TerminalHolder,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalAcquireResult, (ExtensionRequestFailure, String)> {
        if let Some(active) = self.active.as_ref() {
            return Err((
                ExtensionRequestFailure::InvalidRequest,
                format!(
                    "the foreground terminal is already granted to {}",
                    active.holder.name
                ),
            ));
        }
        let grant_id = mint_terminal_grant_id(&holder.instance_id);
        self.active = Some(ActiveTerminalGrant {
            grant_id: grant_id.clone(),
            holder,
        });
        Ok(TerminalAcquireResult {
            grant_id,
            columns,
            rows,
        })
    }

    /// Admit one release from the current holder only.
    fn release(
        &mut self,
        holder: &TerminalHolder,
    ) -> Result<(), (ExtensionRequestFailure, String)> {
        let Some(active) = self.active.as_ref() else {
            return Err((
                ExtensionRequestFailure::InvalidRequest,
                "no foreground terminal grant is active".to_owned(),
            ));
        };
        if &active.holder != holder {
            return Err((
                ExtensionRequestFailure::InvalidRequest,
                format!(
                    "{} does not hold the active foreground terminal grant",
                    holder.name
                ),
            ));
        }
        self.active = None;
        Ok(())
    }

    /// Take the live grant when its holder stopped being valid.
    fn revoke_if(
        &mut self,
        still_valid: impl FnOnce(&TerminalHolder) -> bool,
    ) -> Option<ActiveTerminalGrant> {
        let stale = {
            let active = self.active.as_ref()?;
            !still_valid(&active.holder)
        };
        if stale {
            self.active.take()
        } else {
            None
        }
    }
}

/// Mint a bounded, host-owned grant identifier. The monotonic sequence keeps
/// ids unique across grants handed to one long-lived extension instance.
fn mint_terminal_grant_id(instance_id: &str) -> String {
    static TERMINAL_GRANT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = TERMINAL_GRANT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let candidate = format!("terminal-grant-{sequence:016x}-{instance_id}");
    let mut bounded = String::new();
    for character in candidate.chars() {
        if bounded.len() + character.len_utf8() > MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

/// A keymap binding registered at runtime by a live extension generation.
#[derive(Clone)]
struct RegisteredDynamicShortcut {
    extension: String,
    shortcut_id: String,
    key: ExtensionShortcutKey,
    description: String,
    process: ExtensionProcess,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorStateDelivery {
    state: ExtensionEditorResponse,
    generations: BTreeMap<String, (u64, String)>,
}

#[derive(Clone)]
struct SemanticUiStatus {
    text: String,
    style_role: Option<String>,
    priority: i32,
}

#[derive(Clone)]
struct SemanticUiWidget {
    lines: Vec<String>,
    placement: ExtensionWidgetPlacement,
    style_role: Option<String>,
    priority: i32,
}

#[derive(Default)]
struct SemanticUiView {
    extension_instance_id: String,
    generation: u64,
    statuses: BTreeMap<String, SemanticUiStatus>,
    widgets: BTreeMap<String, SemanticUiWidget>,
    /// The extension-owned header surface, or `None` while it is cleared.
    header: Option<SemanticUiStatus>,
    /// The extension-owned footer surface, or `None` while it is cleared.
    footer: Option<SemanticUiStatus>,
    working: Option<ShellExtensionWorking>,
    hidden_thinking_label: Option<String>,
}

#[derive(Clone)]
struct RegisteredAutocomplete {
    process: ExtensionProcess,
    generation: u64,
    extension_instance_id: String,
}

pub(crate) struct ExtensionAutocompleteUpdate {
    pub(crate) snapshot: ShellEditorSnapshot,
    pub(crate) prefix: String,
    pub(crate) items: Vec<ShellAutocompleteItem>,
}

impl Default for ExecutableExtensions {
    fn default() -> Self {
        let (background_tx, background_rx) = mpsc::channel(BACKGROUND_UPDATE_CAPACITY);
        Self {
            telemetry: None,
            telemetry_rejected: 0,
            telemetry_error: None,
            processes: Vec::new(),
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
            last_editor_state: None,
            background_tx,
            background_rx,
            renderer_tasks: Vec::new(),
            autocomplete_tasks: Vec::new(),
            pending_editor_requests: VecDeque::new(),
            pending_host_requests: VecDeque::new(),
            pending_session_requests: VecDeque::new(),
            terminal_arbiter: TerminalGrantArbiter::default(),
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

pub struct ExtensionToolRenderUpdate {
    pub id: ToolCallId,
    pub segments: Vec<ToolRenderSegment>,
}

#[derive(Default)]
pub struct ExtensionBackgroundUpdates {
    pub rendered_tools: Vec<ExtensionToolRenderUpdate>,
    pub(crate) autocomplete: Vec<ExtensionAutocompleteUpdate>,
    /// Completed shortcut output, ready for the interactive transcript.
    pub shortcut_messages: Vec<String>,
}

enum ExtensionBackgroundUpdate {
    Renderer {
        update: Option<ExtensionToolRenderUpdate>,
        diagnostic: Option<String>,
    },
    Autocomplete {
        update: Option<ExtensionAutocompleteUpdate>,
        diagnostic: Option<String>,
    },
    Shortcut {
        extension: String,
        context: Vec<ContextContribution>,
        messages: Vec<String>,
    },
}

fn opaque_extension_resource_id(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    format!("extension:{digest:x}")
}

/// Keep the experimental transport switch at the process-owner boundary.
///
/// Manifest and MCP configuration are both data loaded before the extension
/// process starts. Strip any copy supplied through that data, then add the
/// argument only when this octet process received its one-shot CLI opt-in.
fn apply_experimental_streamable_http_mcp_gate(
    descriptor: &mut DiscoveredExtension,
    enabled: bool,
) {
    if descriptor.manifest.name != MCP_EXTENSION_NAME {
        return;
    }
    descriptor
        .manifest
        .entrypoint
        .args
        .retain(|argument| argument != EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT);
    if enabled {
        descriptor
            .manifest
            .entrypoint
            .args
            .push(EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT.to_owned());
    }
}

impl ExecutableExtensions {
    /// Discovers and starts extensions with a fresh ordinary-host runtime manager.
    ///
    /// Product bootstrap uses [`Self::discover_and_start_with_provider_runtime`]
    /// to retain compatible workspace services across an App rebuild. This
    /// wrapper preserves the direct construction seam used by focused tests,
    /// which all drive real extension processes.
    #[cfg(all(test, unix))]
    pub fn discover_and_start(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
    ) -> Self {
        Self::discover_and_start_with_runtime_manager(
            config, session, model, reasoning, sessions, host, None,
        )
    }

    /// Discovers a static catalog, binds the current session, and activates
    /// eager lifecycle profiles through the supplied durable manager.
    ///
    /// Reached from the `discover_and_start` test seam and from the
    /// process-startup suite, so it shares that suite's platform gate.
    #[cfg(all(test, unix))]
    pub fn discover_and_start_with_runtime_manager(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
        runtime_manager: Option<ExtensionRuntimeManager>,
    ) -> Self {
        Self::discover_and_start_with_provider_runtime(
            config,
            session,
            model,
            reasoning,
            sessions,
            host,
            runtime_manager,
            ExtensionProviderRuntime::default(),
        )
    }

    /// Discovers and starts extensions using one product-owned provider runtime.
    ///
    /// Bootstrap and rebuild pass the same value through this seam so API 0.3
    /// declarations keep their process-owner fences while the App is replaced.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn discover_and_start_with_provider_runtime(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
        runtime_manager: Option<ExtensionRuntimeManager>,
        provider_runtime: ExtensionProviderRuntime,
    ) -> Self {
        let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
        let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
        let mut diagnostics = snapshot
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}: {}: {}",
                    match diagnostic.level {
                        ResourceDiagnosticLevel::Info => "info",
                        ResourceDiagnosticLevel::Warning => "warning",
                    },
                    diagnostic.path.display(),
                    diagnostic.message
                )
            })
            .collect::<Vec<_>>();

        let (policy, trust_grants) = extension_policy(config, &mut diagnostics);

        // Read through the shared no-follow, bounded boundary, then construct
        // the protocol catalog from validated values. A second filename that
        // declares the same manifest name is retained only as a diagnostic.
        let mut by_name = BTreeMap::<String, DiscoveredExtension>::new();
        for resource in snapshot.resources() {
            let Some(descriptor) =
                load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
            else {
                continue;
            };
            if let Some(first) = by_name.get(&descriptor.manifest.name) {
                diagnostics.push(format!(
                    "warning: {}: extension {:?} duplicates {}; the first manifest wins",
                    descriptor.manifest_path.display(),
                    descriptor.manifest.name,
                    first.manifest_path.display()
                ));
            } else {
                by_name.insert(descriptor.manifest.name.clone(), descriptor);
            }
        }

        let discovered_names = by_name.keys().cloned().collect::<BTreeSet<_>>();
        for name in &config.enabled_extensions {
            if !discovered_names.contains(name) {
                diagnostics.push(format!(
                    "warning: enabled extension {name:?} was not discovered"
                ));
            }
        }
        for grant in &trust_grants {
            if !by_name.values().any(|descriptor| grant.matches(descriptor)) {
                diagnostics.push(format!(
                    "info: trust grant {:?} has no matching discovered extension source",
                    grant.display()
                ));
            }
        }
        for name in &config.invocation_trusted_extensions {
            if !discovered_names.contains(name) {
                diagnostics.push(format!(
                    "info: one-shot trust grant {name:?} has no discovered extension"
                ));
            }
        }

        let mut descriptors = by_name.into_values().collect::<Vec<_>>();
        for descriptor in &mut descriptors {
            apply_experimental_streamable_http_mcp_gate(
                descriptor,
                config.experimental_streamable_http_mcp,
            );
        }
        for descriptor in &descriptors {
            if descriptor.activation.enabled
                && descriptor.activation.trust == ExtensionTrust::Untrusted
            {
                diagnostics.push(format!(
                    "warning: {}: extension {:?} is enabled but untrusted; add trusted_extensions = [{:?}] to the user config or pass --trust-extension {} for this invocation",
                    descriptor.manifest_path.display(),
                    descriptor.manifest.name,
                    persistent_trust_grant(descriptor),
                    descriptor.manifest.name
                ));
            }
        }
        let host_state = host_state(session, model, reasoning, sessions);
        let has_enabled = descriptors
            .iter()
            .any(|descriptor| descriptor.activation.enabled);
        // Executable extensions are ambient-authority child processes, not
        // merely optional tools. Controlled must prevent startup itself; later
        // broker checks cannot contain an already-running process.
        if config.effect_policy != octet_agent::EffectPolicy::UnsafeHost && has_enabled {
            diagnostics.push(CONTROLLED_EXTENSION_START_DIAGNOSTIC.to_owned());
        }
        // Keep the independent product process gate as an additional
        // prerequisite. Discovery remains available for actionable diagnostics.
        if !config.sandbox.process_execution_allowed() && has_enabled {
            diagnostics.push(
                "executable extensions were not started: process execution is disabled by --no-process/--no-shell".to_owned(),
            );
        }
        let execution_allowed = config.effect_policy == octet_agent::EffectPolicy::UnsafeHost
            && config.sandbox.process_execution_allowed();
        let startable = descriptors
            .iter()
            .filter(|descriptor| {
                execution_allowed
                    && descriptor.activation.enabled
                    && descriptor.activation.trust == ExtensionTrust::Trusted
            })
            .cloned()
            .collect::<Vec<_>>();

        let event_bus = Arc::new(ExtensionEventBus::default());
        let (session_lifecycle_service, session_lifecycle_receiver) =
            if active_session_lifecycle_enabled(config)
                && startable.iter().any(extension_session_lifecycle_eligible)
            {
                let (service, receiver) =
                    ExtensionSessionLifecycleService::channel(SESSION_LIFECYCLE_QUEUE_CAPACITY)
                        .expect("fixed session lifecycle queue capacity is bounded");
                (Some(service), Some(receiver))
            } else {
                (None, None)
            };

        // Discovery remains static. Catalog construction reads bounded source
        // identity only; the durable manager is the sole owner allowed to
        // activate a process after policy/trust gates have admitted it.
        // Apply execution policy to retained runtimes as well as new starts.
        // Keep the discovered activation above intact for actionable status;
        // catalog ineligibility retires even still-trusted workspace services.
        crate::app::bootstrap::startup_phase("extensions.digest.begin");
        let catalog = ExtensionRuntimeCatalog::from_descriptors(descriptors.iter().cloned().map(
            |mut descriptor| {
                descriptor.activation.enabled &= execution_allowed;
                descriptor
            },
        ));
        crate::app::bootstrap::startup_phase("extensions.digest.ready");
        crate::app::bootstrap::startup_count(
            "extensions.digest.files",
            catalog.digest_work().files,
        );
        crate::app::bootstrap::startup_count(
            "extensions.digest.bytes",
            catalog.digest_work().bytes,
        );
        crate::app::bootstrap::startup_count(
            "extensions.digest.inactive",
            catalog.digest_work().inactive,
        );
        diagnostics.extend(catalog.diagnostics().iter().map(|diagnostic| {
            format!(
                "warning: extension {:?}: runtime catalog {}",
                diagnostic.extension, diagnostic.message
            )
        }));
        let mut managed_runtime = runtime_manager;
        let mut runtime_binding = None;
        let mut starts = Vec::<(String, Result<ExtensionProcess, String>)>::new();
        if !descriptors.is_empty() || managed_runtime.is_some() {
            if managed_runtime.is_none() {
                match ExtensionRuntimeDomain::ordinary(&config.workspace) {
                    Ok(domain) => managed_runtime = Some(ExtensionRuntimeManager::new(domain)),
                    Err(error) => diagnostics.push(format!(
                        "error: executable extension runtime domain could not be created: {error}"
                    )),
                }
            }
            if let Some(manager) = managed_runtime.clone() {
                let workspace = config.workspace.clone();
                let state = host_state.clone();
                let session_lifecycle_service = session_lifecycle_service.clone();
                let event_bus = event_bus.clone();
                let provider_registry = provider_runtime.registry();
                let subagents_tool_available =
                    model.spec.capabilities.tools && config.tool_available("subagent_spawn");
                let extension_flag_values = config.extension_flag_values.clone();
                let owner = session.resource_owner_key();
                let startable_names = startable
                    .iter()
                    .map(|descriptor| descriptor.manifest.name.clone())
                    .collect::<Vec<_>>();
                crate::app::bootstrap::startup_count(
                    "extensions.handshake.requested",
                    startable_names.len(),
                );
                crate::app::bootstrap::startup_phase("extensions.handshake.begin");
                let activation_result = block_on_runtime(async move {
                    manager.replace_catalog(catalog).await;
                    let binding = manager
                        .bind_session(owner)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    let starts = binding
                        .activate_eager(startable_names, |entry| {
                            let mut runtime = ExtensionRuntimeConfig::new(workspace.clone());
                            runtime.host_state = state.clone();
                            runtime.flag_values = extension_flag_values
                                .get(&entry.descriptor.manifest.name)
                                .cloned()
                                .unwrap_or_default();
                            runtime.agent_sessions = entry.descriptor.manifest.name
                                == SUBAGENTS_EXTENSION_NAME
                                && subagents_tool_available;
                            runtime.session_lifecycle =
                                if extension_session_lifecycle_eligible(&entry.descriptor) {
                                    session_lifecycle_service.clone()
                                } else {
                                    None
                                };
                            if extension_session_lifecycle_eligible(&entry.descriptor) {
                                runtime.event_bus = Some(event_bus.clone());
                            }
                            runtime.provider_registry = Some(provider_registry.clone());
                            runtime
                        })
                        .await;
                    Ok::<_, anyhow::Error>((binding, starts))
                });
                crate::app::bootstrap::startup_phase("extensions.handshake.ready");
                match activation_result {
                    Ok(Ok((binding, activations))) => {
                        runtime_binding = Some(binding);
                        for activation in activations {
                            let name = activation.extension;
                            match (activation.outcome, activation.process) {
                                (ExtensionRuntimeActivationOutcome::Ready, Some(process)) => {
                                    starts.push((name, Ok(process)));
                                }
                                (outcome, _) => starts.push((
                                    name,
                                    Err(match outcome {
                                        ExtensionRuntimeActivationOutcome::Inactive => {
                                            "extension is not eligible for activation".to_owned()
                                        }
                                        ExtensionRuntimeActivationOutcome::StaleSource => {
                                            "extension source changed before activation".to_owned()
                                        }
                                        ExtensionRuntimeActivationOutcome::ResourceExhausted(error) => {
                                            error.to_string()
                                        }
                                        ExtensionRuntimeActivationOutcome::Failed(failure) => {
                                            format!("runtime startup {failure:?}")
                                        }
                                        ExtensionRuntimeActivationOutcome::Ready => {
                                            "runtime activation returned no process".to_owned()
                                        }
                                    }),
                                )),
                            }
                        }
                    }
                    Ok(Err(error)) => diagnostics.push(format!(
                        "error: executable extensions could not bind to the runtime manager: {error}"
                    )),
                    Err(error) => diagnostics.push(format!(
                        "error: executable extensions could not start: {error}"
                    )),
                }
            }
        }

        let mut processes = Vec::new();
        let mut receivers = Vec::new();
        let mut running = BTreeSet::new();
        let mut start_failures = BTreeMap::new();
        for (name, start) in starts {
            match start {
                Ok(process) => {
                    receivers.push(process.subscribe());
                    running.insert(name);
                    processes.push(process);
                }
                Err(error) => {
                    let error = error.to_string();
                    diagnostics.push(format!("error: extension {name:?} launch failed: {error}"));
                    start_failures.insert(name, clip_lifecycle_reason(&error, 4 * 1024));
                }
            }
        }

        // API 0.3 provider declarations are reverse RPCs intentionally sent
        // after initialize. Observe their bounded startup barrier before the
        // caller projects models from the registry.
        provider_runtime.await_initial_registrations(&processes);

        // Register tool catalogs before observers/hooks so all live processes
        // have a host-owned dynamic group. A compatible shared process was
        // detached from the previous App binding before this point.
        for process in &processes {
            process.register_dynamic_tool_catalog(host);
        }
        for process in &processes {
            host.load(process);
        }

        let (shortcuts, shortcut_diagnostics) = register_extension_shortcuts(&processes);
        diagnostics.extend(shortcut_diagnostics);

        let runtime_statuses = managed_runtime
            .as_ref()
            .map(ExtensionRuntimeManager::statuses)
            .unwrap_or_default()
            .into_iter()
            .map(|status| (status.provenance.extension.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let processes_by_name = processes
            .iter()
            .map(|process| (process.descriptor().manifest.name.as_str(), process))
            .collect::<BTreeMap<_, _>>();
        let summaries = descriptors
            .into_iter()
            .map(|descriptor| {
                let process = processes_by_name
                    .get(descriptor.manifest.name.as_str())
                    .copied();
                let contributions = process.map(ExtensionProcess::contributions);
                let health = process.map(ExtensionProcess::health_snapshot).or_else(|| {
                    start_failures.get(&descriptor.manifest.name).map(|error| {
                        ExtensionHealthSnapshot {
                            state: ExtensionHealthState::Parked,
                            generation: 0,
                            pending_requests: 0,
                            last_error: Some(error.clone()),
                        }
                    })
                });
                let negotiated_features: Vec<String> = process
                    .map(|process| process.negotiated_features().iter().cloned().collect())
                    .unwrap_or_default();
                let (telemetry_schema, compatibility) = extension_compatibility(
                    &descriptor.manifest.name,
                    running.contains(&descriptor.manifest.name),
                    &negotiated_features,
                    health.as_ref(),
                );
                let manifest_path = descriptor.manifest_path;
                let manifest_digest = sha256_manifest(&manifest_path);
                let bundle_digest = installed_bundle_digest(&manifest_path);
                ExtensionSummary {
                    name: descriptor.manifest.name.clone(),
                    version: descriptor.manifest.version,
                    manifest_path,
                    manifest_digest,
                    bundle_digest,
                    source: descriptor.source,
                    enabled: descriptor.activation.enabled,
                    trusted: descriptor.activation.trust == ExtensionTrust::Trusted,
                    running: running.contains(&descriptor.manifest.name),
                    api_version: process
                        .map(|process| process.api_version().to_owned())
                        .unwrap_or_else(|| descriptor.manifest.api_version.clone()),
                    negotiated_features,
                    telemetry_schema,
                    compatibility,
                    health,
                    runtime: runtime_statuses.get(&descriptor.manifest.name).cloned(),
                    tools: contributions
                        .map(|value| value.tools.iter().map(|tool| tool.name.clone()).collect())
                        .unwrap_or_else(|| descriptor.manifest.contributes.tools.clone()),
                    commands: contributions
                        .map(|value| {
                            value
                                .commands
                                .iter()
                                .map(|command| command.name.clone())
                                .collect()
                        })
                        .unwrap_or_else(|| descriptor.manifest.contributes.commands.clone()),
                    hooks: descriptor.manifest.contributes.hooks,
                    ui: descriptor.manifest.contributes.ui,
                    // Live declarations are overlaid by `summaries()` from the
                    // shared registry; discovery alone has no provider state.
                    providers: Vec::new(),
                }
            })
            .collect();

        let mut extensions = Self::default();
        extensions.processes = processes;
        extensions.provider_runtime = provider_runtime;
        extensions.runtime_manager = managed_runtime;
        extensions.runtime_binding = runtime_binding;
        extensions.receivers = receivers;
        extensions.shortcuts = shortcuts;
        extensions.summaries = summaries;
        extensions.diagnostics.extend(diagnostics);
        extensions.event_bus = Some(event_bus);
        extensions.session_lifecycle_service = session_lifecycle_service;
        extensions.session_lifecycle_receiver = session_lifecycle_receiver;
        extensions.session_id = host_state.session_id.clone();
        extensions.host_state = Mutex::new(host_state);
        extensions.workspace = config.workspace.clone();
        extensions.resource_owner = Some(session.resource_owner_key());
        extensions.rescan_config = Some(config.clone());
        extensions.rescan_global_config = crate::cli::global_config_path();
        extensions.effect_policy = config.effect_policy;
        extensions.start_policy_supervisors();
        extensions.start_session_lifecycle();
        extensions
    }

    /// Returns the durable process-fleet owner, if discovery created one.
    ///
    /// App rebuilds retain this exact manager and only replace their session
    /// binding, which allows a compatible workspace service to survive without
    /// transferring process ownership to a session object.
    pub fn runtime_manager(&self) -> Option<ExtensionRuntimeManager> {
        self.runtime_manager.clone()
    }

    /// Returns the shared product provider runtime retained across App rebuilds.
    pub(crate) fn provider_runtime(&self) -> ExtensionProviderRuntime {
        self.provider_runtime.clone()
    }

    /// Projects the current registry snapshot into this App's local catalog.
    ///
    /// Callers own the catalog/client mutation boundary; executable extensions
    /// retain only declarations and process handles.
    pub(crate) fn synchronize_provider_catalog(
        &mut self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
    ) -> Vec<String> {
        let diagnostics = self
            .provider_runtime
            .synchronize(catalog, client, &self.processes);
        self.diagnostics.extend(diagnostics.clone());
        diagnostics
    }

    pub(crate) fn synchronize_provider_catalog_report(
        &mut self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
    ) -> ProviderCatalogReport {
        let report = self
            .provider_runtime
            .synchronize_report(catalog, client, &self.processes);
        if report.checked {
            self.diagnostics.extend(report.problems.iter().cloned());
        }
        report
    }

    /// Withdraws this host's provider projection before its processes stop.
    pub(crate) fn clear_provider_catalog(&mut self, catalog: &mut ModelCatalog, client: &AiClient) {
        self.provider_runtime.clear(catalog, client);
    }

    /// Enables requests only after an interactive application has a safely
    /// bound active session. Startup and rebuild leave the queue inactive.
    pub fn activate_session_lifecycle_driver(&self) {
        if let Some(service) = &self.session_lifecycle_service {
            service.activate();
        }
    }

    /// Fences queued work before shutdown or replacement of the owning app.
    pub fn deactivate_session_lifecycle_driver(&self) {
        if let Some(service) = &self.session_lifecycle_service {
            service.deactivate();
        }
    }

    /// Takes one current active-session request at an interactive idle boundary.
    pub fn next_session_lifecycle_request(&mut self) -> Option<ExtensionSessionLifecycleRequest> {
        self.session_lifecycle_receiver
            .as_mut()
            .and_then(ExtensionSessionLifecycleReceiver::try_next)
    }

    pub fn bind_agent_sessions(&self, agent: &Agent) -> anyhow::Result<usize> {
        let mut bound = 0;
        for process in &self.processes {
            if agent
                .bind_extension_agent_sessions(process)
                .with_context(|| {
                    format!(
                        "could not bind agent_sessions for extension {:?}",
                        process.descriptor().manifest.name
                    )
                })?
            {
                bound += 1;
            }
        }
        Ok(bound)
    }

    pub fn has_dynamic_tool_provider(&self) -> bool {
        self.processes
            .iter()
            .any(|process| process.supports_feature(EXTENSION_FEATURE_DYNAMIC_TOOLS))
    }

    /// Resolve a host-validated extension shortcut from one terminal event.
    /// Static manifest contributions win over runtime registrations.
    fn shortcut_for_event(&self, event: &Event) -> Option<ExtensionShortcutInvocation> {
        let key = extension_shortcut_key(event)?;
        if let Some(shortcut) = self.shortcuts.iter().find(|shortcut| shortcut.key == key) {
            return Some(shortcut.invocation.clone());
        }
        let dynamic = self
            .dynamic_shortcuts
            .iter()
            .find(|shortcut| shortcut.key == key)?;
        Some(ExtensionShortcutInvocation {
            extension: dynamic.extension.clone(),
            name: dynamic.shortcut_id.clone(),
            description: dynamic.description.clone(),
        })
    }

    /// Schedule a shortcut without blocking terminal input. Child confirmation
    /// and input requests are denied because a background invocation cannot
    /// safely take ownership of the frontend confirmation surface.
    pub(crate) fn dispatch_shortcut_for_event(
        &mut self,
        event: &Event,
    ) -> Option<ExtensionShortcutInvocation> {
        let invocation = self.shortcut_for_event(event)?;
        if let Some(index) = self.dynamic_shortcuts.iter().position(|dynamic| {
            dynamic.extension == invocation.extension && dynamic.shortcut_id == invocation.name
        }) {
            let dynamic = self.dynamic_shortcuts[index].clone();
            if !dynamic.process.is_running()
                || dynamic.process.health_snapshot().generation != dynamic.generation
            {
                self.dynamic_shortcuts.remove(index);
                self.diagnostics.push(format!(
                    "warning: extension shortcut {:?}/{} was dropped with its process generation",
                    dynamic.extension, dynamic.shortcut_id
                ));
                return None;
            }
            if let Err(error) = dynamic
                .process
                .notify_shortcut_trigger(&dynamic.shortcut_id)
            {
                self.diagnostics.push(format!(
                    "warning: extension shortcut {:?}/{} could not be delivered: {error}",
                    dynamic.extension, dynamic.shortcut_id
                ));
                return None;
            }
            return Some(invocation);
        }
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == invocation.extension
                    && process
                        .contributions()
                        .shortcuts
                        .iter()
                        .any(|shortcut| shortcut.name == invocation.name)
            })
            .cloned()
        else {
            self.diagnostics.push(format!(
                "warning: extension shortcut {:?}/{} is no longer available",
                invocation.extension, invocation.name
            ));
            return None;
        };
        if Handle::try_current().is_err() {
            self.diagnostics.push(format!(
                "warning: extension shortcut {:?}/{} could not start outside the octet Tokio runtime",
                invocation.extension, invocation.name
            ));
            return None;
        }
        self.shortcut_tasks.retain(|task| !task.is_finished());
        if self.shortcut_tasks.len() >= SHORTCUT_TASK_CONCURRENCY {
            self.diagnostics.push(format!(
                "warning: extension shortcut {:?}/{} was not started: {SHORTCUT_TASK_CONCURRENCY} invocations are already running",
                invocation.extension, invocation.name
            ));
            return None;
        }
        let sender = self.background_tx.clone();
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        let started = invocation.clone();
        self.shortcut_tasks.push(tokio::spawn(async move {
            let (extension, context, messages) =
                execute_shortcut_headless(process, invocation.name, context).await;
            let _ = sender
                .send(ExtensionBackgroundUpdate::Shortcut {
                    extension,
                    context,
                    messages,
                })
                .await;
        }));
        Some(started)
    }

    fn start_policy_supervisors(&mut self) {
        let Ok(handle) = Handle::try_current() else {
            if !self.processes.is_empty() {
                self.diagnostics
                    .push("warning: extension policy supervision requires the octet Tokio runtime");
            }
            return;
        };
        // A reload replaces the subscriptions, not the policy authority. Never
        // leave duplicate responders attached to a retained process.
        for task in self.policy_supervisors.drain(..) {
            task.abort();
        }
        let effect_policy = self.effect_policy;
        self.policy_supervisors
            .extend(self.processes.iter().cloned().map(|process| {
                let mut events = process.subscribe();
                handle.spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(ExtensionEvent::PolicyEvaluationRequested {
                                request_id,
                                generation,
                                parent_request_id,
                                intent,
                            }) => {
                                let response = mcp_policy_response(
                                    effect_policy,
                                    &process,
                                    generation,
                                    parent_request_id,
                                    &intent,
                                );
                                let _ = process
                                    .respond_to_policy_evaluation(request_id, generation, response)
                                    .await;
                            }
                            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                })
            }));
    }

    fn start_session_lifecycle(&mut self) {
        if self.processes.is_empty() || self.session_lifecycle_started {
            return;
        }
        let processes = self.processes.clone();
        if let Some(session_id) = self.session_id.clone() {
            let event = ExtensionLifecycleEvent::SessionStarted {
                session_id,
                run_id: None,
            };
            match block_on_runtime(async move { notify_lifecycle_all(&processes, event).await }) {
                Ok(messages) => self.diagnostics.extend(messages),
                Err(error) => self.diagnostics.push(format!(
                    "warning: extension session lifecycle could not start: {error}"
                )),
            }
        }
        if let Some(resource_owner) = self.resource_owner.clone() {
            let processes = self.processes.clone();
            match block_on_runtime(async move {
                start_session_hooks_all(&processes, &resource_owner).await
            }) {
                Ok(messages) => self.diagnostics.extend(messages),
                Err(error) => self.diagnostics.push(format!(
                    "warning: extension session hooks could not start: {error}"
                )),
            }
        }
        self.session_lifecycle_started = true;
        self.session_started_at = Instant::now();
    }

    pub async fn begin_turn(&self) -> ExtensionTurnLifecycle {
        let sequence = NEXT_EXTENSION_RUN_ID.fetch_add(1, Ordering::Relaxed);
        let session_id = self
            .processes
            .first()
            .and_then(|process| process.current_context().host.session_id)
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| "unknown-session".into());
        let run_id = format!("extension-run-{sequence}");
        let turn_id = format!("extension-turn-{sequence}");
        let resource_owner = self
            .resource_owner
            .clone()
            .unwrap_or_else(|| session_id.clone());
        let started_at = Instant::now();
        let processes = self.processes.clone();
        for process in &processes {
            process.set_active_lifecycle_turn(
                resource_owner.clone(),
                session_id.clone(),
                run_id.clone(),
                turn_id.clone(),
            );
        }
        let (started_tx, started_delivery) = watch::channel(false);
        let start_processes = processes.clone();
        let start_event = ExtensionLifecycleEvent::TurnStarted {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
        };
        #[cfg(test)]
        let lifecycle_delivery_test_control = self.lifecycle_delivery_test_control.clone();
        tokio::spawn(async move {
            #[cfg(test)]
            if let Some(control) = lifecycle_delivery_test_control {
                control.wait_before_delivery(&start_event).await;
            }
            let _ = notify_lifecycle_all(&start_processes, start_event).await;
            let _ = started_tx.send(true);
        });
        let mut turn = ExtensionTurnLifecycle {
            processes,
            resource_owner,
            session_id,
            run_id,
            turn_id,
            started_at,
            started_delivery,
            settled: false,
            #[cfg(test)]
            lifecycle_delivery_test_control: self.lifecycle_delivery_test_control.clone(),
        };
        let _ = turn.started_delivery.wait_for(|started| *started).await;
        turn
    }

    pub async fn settle_turn(
        &mut self,
        turn: ExtensionTurnLifecycle,
        outcome: &crate::modes::HostRunOutcome,
    ) {
        let lifecycle_outcome = outcome.extension_lifecycle_outcome();
        let reason = outcome
            .failure_message()
            .map(|reason| clip_lifecycle_reason(reason, 4 * 1024));
        let diagnostics = turn.settle(lifecycle_outcome, reason).await;
        self.diagnostics.extend(diagnostics);
        self.last_lifecycle_outcome = Some(lifecycle_outcome);
    }

    pub fn command_suggestions(&self) -> Vec<(String, String)> {
        self.command_suggestions_with_usage()
            .into_iter()
            .map(|(name, description, _)| (name, description))
            .collect()
    }

    /// Returns the executable command metadata needed by non-TUI discovery surfaces.
    pub fn command_suggestions_with_usage(&self) -> Vec<(String, String, Option<String>)> {
        self.processes
            .iter()
            .flat_map(|process| {
                process.contributions().commands.iter().map(|command| {
                    (
                        command.name.clone(),
                        command.description.clone(),
                        command.usage.clone(),
                    )
                })
            })
            .collect()
    }

    /// Returns the manifest identity that owns one registered slash command.
    pub fn command_owner(&self, command: &str) -> Option<String> {
        self.processes.iter().find_map(|process| {
            process
                .contributions()
                .commands
                .iter()
                .any(|definition| definition.name == command)
                .then(|| process.descriptor().manifest.name.clone())
        })
    }

    pub fn status_summary(&self) -> String {
        let ready = self
            .summaries()
            .into_iter()
            .filter(|extension| {
                extension
                    .health
                    .as_ref()
                    .is_some_and(|health| health.state == ExtensionHealthState::Ready)
            })
            .map(|extension| extension.name.clone())
            .collect::<Vec<_>>();
        if self.summaries.is_empty() {
            "0 ready / 0 discovered".to_owned()
        } else if ready.is_empty() {
            format!("0 ready / {} discovered", self.summaries.len())
        } else {
            format!(
                "{} ready / {} discovered ({})",
                ready.len(),
                self.summaries.len(),
                ready.join(", ")
            )
        }
    }

    /// Returns discovery metadata with live protocol-health snapshots overlaid.
    pub fn summaries(&self) -> Vec<ExtensionSummary> {
        let runtime_statuses = self
            .runtime_manager
            .as_ref()
            .map(ExtensionRuntimeManager::statuses)
            .unwrap_or_default()
            .into_iter()
            .map(|status| (status.provenance.extension.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let processes_by_name = self
            .processes
            .iter()
            .map(|process| (process.descriptor().manifest.name.as_str(), process))
            .collect::<BTreeMap<_, _>>();
        let mut summaries = self.summaries.clone();
        for summary in &mut summaries {
            summary.runtime = runtime_statuses.get(&summary.name).cloned();
            let Some(process) = processes_by_name.get(summary.name.as_str()).copied() else {
                continue;
            };
            let health = process.health_snapshot();
            summary.running = process.is_running();
            summary.health = Some(health);
            summary.api_version = process.api_version().to_owned();
            summary.negotiated_features = process.negotiated_features().iter().cloned().collect();
            let (telemetry_schema, compatibility) = extension_compatibility(
                &summary.name,
                summary.running,
                &summary.negotiated_features,
                summary.health.as_ref(),
            );
            summary.telemetry_schema = telemetry_schema;
            summary.compatibility = compatibility;
            summary.tools = process
                .tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect();
            summary.providers = self
                .provider_runtime
                .recorded_providers_for(process.extension_instance_id())
                .into_iter()
                .map(|(entry, complete)| ExtensionProviderSummary {
                    id: entry.provider.id.clone(),
                    label: entry.provider.label.clone(),
                    authorization: entry.authorization.as_wire().to_owned(),
                    models: entry
                        .models
                        .iter()
                        .map(|model| extension_provider_model_id(&entry.provider.id, &model.id).0)
                        .collect(),
                    live: complete,
                })
                .collect();
        }
        summaries
    }

    /// Returns whether the observing first-party subagents extension has a
    /// live, negotiated child-session service.
    pub fn has_agent_session_service(&self) -> bool {
        self.processes.iter().any(|process| {
            process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                && process.is_running()
                && process.supports_feature(EXTENSION_FEATURE_AGENT_SESSIONS)
                && process.supports_feature(EXTENSION_FEATURE_DELEGATION_TELEMETRY)
        })
    }

    /// Count nonterminal workers in the current owner-fenced subagents roster.
    /// This must be observed before releasing the App binding, not on its replacement.
    pub(crate) fn active_subagent_worker_count(&self) -> usize {
        self.presentation_views()
            .into_iter()
            .filter(|view| view.extension == SUBAGENTS_EXTENSION_NAME)
            .filter_map(|view| view.snapshot.collection)
            .flat_map(|collection| collection.nodes)
            .filter(|node| {
                matches!(
                    node.state,
                    octet_agent::ExtensionPresentationState::Pending
                        | octet_agent::ExtensionPresentationState::Active
                        | octet_agent::ExtensionPresentationState::Running
                        | octet_agent::ExtensionPresentationState::Degraded
                )
            })
            .count()
    }

    /// Returns the latest accepted semantic state for each running extension.
    pub fn presentation_views(&self) -> Vec<ExtensionPresentationView> {
        self.presentations
            .values()
            .filter(|view| {
                (view.resource_owner.is_none()
                    || view.resource_owner.as_deref() == self.resource_owner.as_deref())
                    && self.processes.iter().any(|process| {
                        process.descriptor().manifest.name == view.extension
                            && process.is_running()
                            && process.health_snapshot().generation == view.generation
                            && process.extension_instance_id() == view.extension_instance_id
                    })
            })
            .cloned()
            .collect()
    }

    /// Returns the exact path-free extension principal that issued a current
    /// owner-scoped delegated-session reference. Resolution remains separately
    /// parent- and principal-bound in `Agent`.
    pub fn presentation_session_reference_principal(&self, reference: &str) -> Option<String> {
        let matches = |references: &[octet_agent::ExtensionPresentationReference]| {
            references.iter().any(|candidate| {
                candidate.kind == octet_agent::ExtensionPresentationReferenceKind::Session
                    && candidate.id == reference
            })
        };
        let view = self.presentation_views().into_iter().find(|view| {
            view.snapshot
                .activities
                .iter()
                .any(|activity| matches(&activity.references))
                || view.snapshot.collection.as_ref().is_some_and(|collection| {
                    collection
                        .nodes
                        .iter()
                        .any(|node| matches(&node.references))
                        || collection
                            .detail
                            .as_ref()
                            .is_some_and(|detail| matches(&detail.references))
                })
        })?;
        self.processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == view.extension
                    && process.extension_instance_id() == view.extension_instance_id
                    && process.health_snapshot().generation == view.generation
            })
            .map(ExtensionProcess::agent_session_principal)
    }

    /// Drains pending extension events and renders the generic headless fallback.
    pub fn presentation_text(&mut self) -> String {
        let _ = self.drain_events();
        format_presentation_views(&self.presentation_views())
    }

    pub fn inspect_text(&mut self) -> String {
        self.drain_events();
        let mut lines = Vec::new();
        if self.summaries.is_empty() {
            lines.push("No executable extensions discovered.".to_owned());
        } else {
            lines.push("Executable extensions".to_owned());
            for extension in self.summaries() {
                let state = match (extension.enabled, extension.trusted, extension.running) {
                    (_, _, true) => "running",
                    (true, false, false) => "enabled, untrusted",
                    (false, true, false) => "trusted, disabled",
                    (true, true, false) => "launch failed",
                    (false, false, false) => "disabled, untrusted",
                };
                lines.push(format!(
                    "- {} {} · API {} · {} · {:?} · {}",
                    extension.name,
                    extension.version,
                    extension.api_version,
                    state,
                    extension.source,
                    extension.manifest_path.display()
                ));
                lines.push(format!(
                    "  manifest sha256: {} · bundle sha256: {}",
                    extension.manifest_digest,
                    extension.bundle_digest.as_deref().unwrap_or("unpackaged"),
                ));
                lines.push(format!(
                    "  compatibility: {} · telemetry schema: {}",
                    extension.compatibility,
                    extension
                        .telemetry_schema
                        .as_deref()
                        .unwrap_or("not negotiated"),
                ));
                if let Some(health) = &extension.health {
                    lines.push(format!(
                        "  health: {:?} · generation {} · {} pending{}",
                        health.state,
                        health.generation,
                        health.pending_requests,
                        health
                            .last_error
                            .as_ref()
                            .map(|error| format!(" · last error: {error}"))
                            .unwrap_or_default()
                    ));
                }
                if !extension.negotiated_features.is_empty() {
                    lines.push(format!(
                        "  features: {}",
                        extension.negotiated_features.join(", ")
                    ));
                }
                if !extension.tools.is_empty() {
                    lines.push(format!("  tools: {}", extension.tools.join(", ")));
                }
                if !extension.providers.is_empty() {
                    lines.push(format!(
                        "  providers: {}",
                        extension
                            .providers
                            .iter()
                            .map(|provider| {
                                format!(
                                    "{} [{}] {} · models: {}",
                                    provider.id,
                                    provider.authorization,
                                    if provider.live { "live" } else { "pending" },
                                    provider.models.join(", ")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
                if !extension.commands.is_empty() {
                    lines.push(format!("  commands: /{}", extension.commands.join(", /")));
                }
                if !extension.hooks.is_empty() {
                    lines.push(format!("  hooks: {:?}", extension.hooks));
                }
                if !extension.ui.is_empty() {
                    lines.push(format!("  ui: {:?}", extension.ui));
                }
            }
        }
        let presentation = self.presentation_text();
        if !presentation.is_empty() {
            lines.push(String::new());
            lines.push("Extension activity".to_owned());
            lines.push(presentation);
        }
        if !self.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Diagnostics".to_owned());
            if self.diagnostics.dropped > 0 {
                lines.push(format!(
                    "- warning: {} older extension diagnostic(s) were dropped to enforce the {} byte / {} entry history limit",
                    self.diagnostics.dropped, MAX_DIAGNOSTIC_BYTES, MAX_DIAGNOSTIC_ENTRIES
                ));
            }
            lines.extend(
                self.diagnostics
                    .iter()
                    .map(|diagnostic| format!("- {diagnostic}")),
            );
        }
        lines.join("\n")
    }

    /// Settles the old observational session boundary, updates every live
    /// process snapshot, starts observation for the replacement session, and
    /// fences queued active-session mutations from the previous snapshot. The
    /// active agent has already changed by the time this is called.
    pub fn transition_active_session(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        if self.session_lifecycle_started {
            let outcome = ExtensionLifecycleOutcome::Completed;
            if let Some(resource_owner) = self.resource_owner.clone() {
                let processes = self.processes.clone();
                match block_on_runtime(async move {
                    settle_session_hooks_all(&processes, &resource_owner, outcome).await
                }) {
                    Ok(messages) => self.diagnostics.extend(messages),
                    Err(error) => self.diagnostics.push(format!(
                        "warning: extension session hooks could not settle: {error}"
                    )),
                }
            }
            if let Some(session_id) = self.session_id.clone() {
                let processes = self.processes.clone();
                let event = ExtensionLifecycleEvent::SessionSettled {
                    session_id,
                    run_id: None,
                    outcome,
                    duration_ms: duration_millis(self.session_started_at.elapsed()),
                    reason: Some("active session changed".into()),
                };
                match block_on_runtime(async move { notify_lifecycle_all(&processes, event).await })
                {
                    Ok(messages) => self.diagnostics.extend(messages),
                    Err(error) => self.diagnostics.push(format!(
                        "warning: extension session lifecycle could not settle: {error}"
                    )),
                }
            }
            self.session_lifecycle_started = false;
        }
        self.last_lifecycle_outcome = None;
        // Contributions and owner-scoped presentation are observations of the
        // old active session and must not bleed into the replacement.
        self.pending_context = PendingContext::default();
        self.pending_post_mutation_rescans.clear();
        self.mutation_family_generations.clear();
        if let Some(bus) = &self.event_bus {
            bus.reset();
        }
        self.resource_owner = Some(session.resource_owner_key());
        let active_owner = self.resource_owner.as_deref();
        self.presentations.retain(|_, view| {
            view.resource_owner.is_none() || view.resource_owner.as_deref() == active_owner
        });
        self.refresh_host_state(session, model, reasoning, sessions);
        self.start_session_lifecycle();
        // The session changed in place, so requests admitted against the old
        // snapshot must not run against this replacement.
        self.activate_session_lifecycle_driver();
    }

    /// The launch already projected skills and session metadata for initialize.
    /// Only a changed final provider view or reasoning needs another projection.
    pub(crate) fn refresh_initial_host_state(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        let initial = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let unchanged = initial.model.as_deref() == Some(&model.spec.id.0)
            && initial.model_view == extension_model_view(model)
            && initial.reasoning == Some(serde_json::Value::String(format!("{reasoning:?}")));
        drop(initial);
        if !unchanged {
            self.refresh_host_state(session, model, reasoning, sessions);
        }
    }

    pub fn refresh_host_state(
        &mut self,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
    ) {
        let state = host_state(session, model, reasoning, sessions);
        self.session_id = state.session_id.clone();
        for process in &self.processes {
            if process.descriptor().manifest.runtime.sharing
                == octet_agent::extension_process::ExtensionRuntimeSharing::Isolated
            {
                process.set_host_state(state.clone());
            }
        }
        // Every session or model boundary refreshes this cache, so a read-only
        // context snapshot never answers from the process-startup projection.
        *self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    fn enqueue_context(&mut self, source: &str, contribution: ContextContribution) -> bool {
        admit_context(
            &mut self.pending_context,
            &mut self.diagnostics,
            source,
            contribution,
        )
    }

    fn enqueue_contexts(
        &mut self,
        source: &str,
        contributions: impl IntoIterator<Item = ContextContribution>,
    ) {
        let mut dropped = 0usize;
        let mut last_error = None;
        for contribution in contributions {
            if let Err(error) = self.pending_context.try_push(contribution) {
                dropped = dropped.saturating_add(1);
                last_error = Some(error);
            }
        }
        if dropped > 0 {
            self.diagnostics.push(format!(
                "warning: {source}: dropped {dropped} extension context contribution(s): {}",
                last_error.unwrap_or_else(|| "context admission failed".into())
            ));
        }
    }

    pub async fn compose_prompt(
        &mut self,
        base_system: &str,
        prompt: String,
    ) -> anyhow::Result<ExtensionPromptComposition> {
        let mut notifications = self.drain_events();
        // Composition is transactional. Context already queued by an
        // extension remains pending until the complete composed prompt has
        // passed validation and can be submitted durably.
        let pending_count = self.pending_context.len();
        let mut context = PendingContext::default();
        for contribution in self.pending_context.iter().take(pending_count).cloned() {
            context
                .try_push(contribution)
                .expect("admitted pending extension context must remain valid");
        }
        let mut rejected_context = Vec::new();

        for process in &self.processes {
            let execution = extension_execution_context(process, self.resource_owner.as_deref());
            if process
                .contributions()
                .hooks
                .contains(&ExtensionHook::BeforePrompt)
            {
                let output = tokio::time::timeout(
                    PROMPT_RPC_DEADLINE,
                    process.run_hook(
                        ExtensionHook::BeforePrompt,
                        before_prompt_hook_payload(&prompt),
                        execution.clone(),
                    ),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "extension {:?} before_prompt hook exceeded {:?}",
                        process.descriptor().manifest.name,
                        PROMPT_RPC_DEADLINE,
                    )
                })?
                .with_context(|| {
                    format!(
                        "extension {:?} before_prompt hook failed",
                        process.descriptor().manifest.name
                    )
                })?;
                if let ExtensionHookDisposition::Deny { reason } = output.disposition {
                    anyhow::bail!(
                        "extension {:?} denied the prompt: {reason}",
                        process.descriptor().manifest.name
                    );
                }
                let mut dropped = 0usize;
                let mut last_error = None;
                for contribution in output.context {
                    if let Err(error) = context.try_push(contribution) {
                        dropped = dropped.saturating_add(1);
                        last_error = Some(error);
                    }
                }
                if dropped > 0 {
                    rejected_context.push(format!(
                        "warning: extension {:?} dropped {dropped} before_prompt context contribution(s): {}",
                        process.descriptor().manifest.name,
                        last_error.unwrap_or_else(|| "context admission failed".into())
                    ));
                }
                notifications.extend(output.notifications.into_iter().map(|notification| {
                    format_notification(&process.descriptor().manifest.name, &notification)
                }));
            }
            if process.contributions().context {
                let collected = tokio::time::timeout(
                    PROMPT_RPC_DEADLINE,
                    process.collect_context(Some(prompt.clone()), execution),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "extension {:?} context collection exceeded {:?}",
                        process.descriptor().manifest.name,
                        PROMPT_RPC_DEADLINE,
                    )
                })?
                .with_context(|| {
                    format!(
                        "extension {:?} context collection failed",
                        process.descriptor().manifest.name
                    )
                })?;
                let mut dropped = 0usize;
                let mut last_error = None;
                for contribution in collected {
                    if let Err(error) = context.try_push(contribution) {
                        dropped = dropped.saturating_add(1);
                        last_error = Some(error);
                    }
                }
                if dropped > 0 {
                    rejected_context.push(format!(
                        "warning: extension {:?} dropped {dropped} collected context contribution(s): {}",
                        process.descriptor().manifest.name,
                        last_error.unwrap_or_else(|| "context admission failed".into())
                    ));
                }
            }
        }

        notifications.extend(rejected_context.iter().cloned());
        self.diagnostics.extend(rejected_context);
        let (system, prompt) = compose_context(base_system, prompt, context.into_vec())?;
        notifications.extend(self.drain_events());
        Ok(ExtensionPromptComposition {
            system,
            prompt,
            notifications,
            pending_context_count: pending_count,
        })
    }

    /// Commit the one-shot context captured by a successful prompt
    /// composition. Frontends call this only after the user message has been
    /// appended durably; preflight/append failures leave the context available
    /// for the restored draft's retry.
    pub fn commit_prompt_context(&mut self, pending_context_count: usize) {
        self.pending_context.commit(pending_context_count);
    }

    pub async fn after_response(&mut self, response: &str) -> Vec<String> {
        let mut messages = Vec::new();
        let mut queued_context = Vec::new();
        for process in &self.processes {
            if !process
                .contributions()
                .hooks
                .contains(&ExtensionHook::AfterResponse)
            {
                continue;
            }
            let execution = extension_execution_context(process, self.resource_owner.as_deref());
            match tokio::time::timeout(
                AFTER_RESPONSE_RPC_DEADLINE,
                process.run_hook(
                    ExtensionHook::AfterResponse,
                    after_response_hook_payload(response),
                    execution,
                ),
            )
            .await
            {
                Err(_) => messages.push(format!(
                    "extension {:?} after_response hook exceeded {:?}",
                    process.descriptor().manifest.name,
                    AFTER_RESPONSE_RPC_DEADLINE,
                )),
                Ok(Ok(output)) => {
                    let extension_name = process.descriptor().manifest.name.clone();
                    messages.extend(
                        output.notifications.into_iter().map(|notification| {
                            format_notification(&extension_name, &notification)
                        }),
                    );
                    queued_context.extend(
                        output
                            .context
                            .into_iter()
                            .map(|contribution| (extension_name.clone(), contribution)),
                    );
                }
                Ok(Err(error)) => messages.push(format!(
                    "extension {:?} after_response hook failed: {error}",
                    process.descriptor().manifest.name
                )),
            }
        }
        for (extension_name, contribution) in queued_context {
            self.enqueue_context(&extension_name, contribution);
        }
        messages.extend(self.drain_events());
        messages
    }

    pub async fn execute_presentation_action_with_confirmation<H>(
        &mut self,
        extension: &str,
        action_id: &str,
        confirmations: &mut H,
    ) -> anyhow::Result<String>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let action = self
            .presentation_views()
            .into_iter()
            .find(|view| view.extension == extension)
            .and_then(|view| {
                view.snapshot
                    .actions
                    .into_iter()
                    .find(|action| action.id == action_id)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                "extension presentation action {extension:?}/{action_id:?} is unavailable or stale"
            )
            })?;
        let mut approval_budget = 0;
        if action.destructive {
            let request = ConfirmationRequest {
                parent_request_id: None,
                prompt: format!("Run {}?", action.label),
                detail: Some(format!("Declared by extension {extension:?}")),
                destructive: true,
                default: false,
            };
            if !confirmations.confirm(extension, &request).await? {
                anyhow::bail!("extension presentation action was denied");
            }
            approval_budget = 1;
        }
        let mut command_confirmations = PreapprovedExtensionConfirmation {
            inner: confirmations,
            remaining: approval_budget,
        };
        self.execute_command_with_confirmation_scoped(
            Some(extension),
            &action.command,
            action.arguments,
            &mut command_confirmations,
            None,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("extension presentation action routed to an unavailable command")
        })
    }

    /// Executes an authenticated Serve action with one command-scoped approval.
    #[cfg(feature = "serve")]
    pub async fn execute_presentation_action_for_serve(
        &mut self,
        extension: &str,
        expected_extension_instance_id: &str,
        expected_generation: u64,
        expected_revision: u64,
        action_id: &str,
        confirmed: bool,
    ) -> anyhow::Result<String> {
        let action = self
            .presentation_views()
            .into_iter()
            .find(|view| {
                view.extension == extension
                    && view.extension_instance_id == expected_extension_instance_id
                    && view.generation == expected_generation
                    && view.snapshot.revision == expected_revision
            })
            .and_then(|view| {
                view.snapshot
                    .actions
                    .into_iter()
                    .find(|action| action.id == action_id)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                "extension presentation action {extension:?}/{action_id:?} is unavailable or stale"
            )
            })?;
        if confirmed && !action.destructive {
            anyhow::bail!("non-destructive extension action cannot carry approval");
        }
        if action.destructive && !confirmed {
            anyhow::bail!("extension presentation action requires explicit confirmation");
        }
        self.execute_command_headless_scoped(
            Some(extension),
            &action.command,
            action.arguments,
            usize::from(action.destructive && confirmed),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("extension presentation action routed to an unavailable command")
        })
    }

    /// The options menu `/extensions` shows for one running extension: its
    /// own `menu/collect` answer, or entries generated from its declared
    /// commands when it offers no menu. `None` when it is not running.
    pub async fn options_menu(
        &mut self,
        extension: &str,
    ) -> anyhow::Result<Option<ExtensionOptions>> {
        let _ = self.drain_events();
        let Some(process) = self
            .processes
            .iter()
            .find(|process| process.descriptor().manifest.name == extension && process.is_running())
            .cloned()
        else {
            return Ok(None);
        };
        if !process.contributions().menu {
            return Ok(Some(generated_options(&process)));
        }
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        let menu = tokio::time::timeout(MENU_COLLECT_DEADLINE, process.collect_menu(context))
            .await
            .map_err(|_| anyhow::anyhow!("timed out after {MENU_COLLECT_DEADLINE:?}"))
            .and_then(|menu| menu.map_err(anyhow::Error::from))
            .with_context(|| format!("{extension} could not build its options menu"))?;
        Ok(Some(ExtensionOptions {
            menu,
            generated: false,
        }))
    }

    /// Entries generated from a running extension's declared commands, used
    /// when it offers no menu or its menu could not be built.
    pub fn generated_options_menu(&self, extension: &str) -> Option<ExtensionOptions> {
        self.processes
            .iter()
            .find(|process| process.descriptor().manifest.name == extension && process.is_running())
            .map(generated_options)
    }

    /// Runs one options-menu action: a declared command of `extension`, behind
    /// a host confirmation when the extension marked it destructive. `place`
    /// names the menu the action was chosen from.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_menu_action_with_confirmation<H>(
        &mut self,
        extension: &str,
        label: &str,
        place: &str,
        command: &str,
        arguments: Vec<String>,
        destructive: bool,
        confirmations: &mut H,
    ) -> anyhow::Result<String>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let mut approval_budget = 0;
        if destructive {
            let request = ConfirmationRequest {
                parent_request_id: None,
                prompt: format!("{label}?"),
                detail: Some(format!("{place} · offered by {extension}")),
                destructive: true,
                default: false,
            };
            if !confirmations.confirm(extension, &request).await? {
                anyhow::bail!("{label} was cancelled");
            }
            approval_budget = 1;
        }
        let mut command_confirmations = PreapprovedExtensionConfirmation {
            inner: confirmations,
            remaining: approval_budget,
        };
        // The person started this action and watches its progress, and can
        // cancel it, so it may outlast the ordinary request deadline.
        self.execute_command_with_confirmation_scoped(
            Some(extension),
            command,
            arguments,
            &mut command_confirmations,
            Some(MENU_ACTION_DEADLINE),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("{extension} no longer offers {label:?}"))
    }

    // Every caller is a Unix process-fixture test.
    #[cfg(all(test, unix))]
    pub async fn execute_command_with_confirmation<H>(
        &mut self,
        name: &str,
        arguments: Vec<String>,
        confirmations: &mut H,
    ) -> anyhow::Result<Option<String>>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        self.execute_command_with_confirmation_scoped(None, name, arguments, confirmations, None)
            .await
    }

    async fn execute_command_with_confirmation_scoped<H>(
        &mut self,
        extension: Option<&str>,
        name: &str,
        arguments: Vec<String>,
        confirmations: &mut H,
        attended_deadline: Option<Duration>,
    ) -> anyhow::Result<Option<String>>
    where
        H: ExtensionConfirmationHandler + ?Sized,
    {
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                extension.is_none_or(|extension| process.descriptor().manifest.name == extension)
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == name)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let extension_name = process.descriptor().manifest.name.clone();
        let execution_context =
            extension_execution_context(&process, self.resource_owner.as_deref());
        let mut events = process.subscribe();
        let output: anyhow::Result<_> = async {
            let legacy_uncorrelated = process.api_version() == EXTENSION_API_VERSION_0_1;
            let (request_started, started) = tokio::sync::oneshot::channel();
            let mut started = Box::pin(started);
            let mut operation = None;
            let cancellation_token = CancellationToken::default();
            let (progress_sink, mut progress_rx) = ToolProgressSink::bounded_channel();
            let mut execution: Pin<Box<dyn Future<Output = _> + Send + '_>> =
                match attended_deadline {
                    Some(deadline) => Box::pin(process.execute_attended_command_with_progress(
                        name.to_owned(),
                        arguments,
                        execution_context,
                        cancellation_token.clone(),
                        progress_sink,
                        request_started,
                        deadline,
                    )),
                    None => Box::pin(process.execute_command_controlled_with_progress(
                        name.to_owned(),
                        arguments,
                        execution_context,
                        cancellation_token.clone(),
                        progress_sink,
                        request_started,
                    )),
                };
            let mut events_open = true;
            let result = loop {
                // The cancellation future and confirmation UI borrow the same
                // frontend. Keep the select in its own scope so cancellation
                // is dropped before a confirmation prompt borrows it again.
                let mut command_progress = None;
                let event = {
                    let cancellation = confirmations.wait_for_cancel();
                    tokio::pin!(cancellation);
                    tokio::select! {
                        result = &mut execution => break result?,
                        started = &mut started, if operation.is_none() => match started {
                            Ok(started) => {
                                operation = Some(started);
                                None
                            }
                            Err(_) => break execution.await?,
                        },
                        progress = progress_rx.recv() => {
                            command_progress = progress;
                            None
                        },
                        event = events.recv(), if events_open && operation.is_some() => Some(event),
                        cancelled = &mut cancellation => {
                            cancelled.with_context(|| format!(
                                "cancellation UI failed for extension {extension_name:?}"
                            ))?;
                            cancellation_token.cancel();
                            anyhow::bail!("extension command {name:?} cancelled");
                        }
                    }
                };
                if let Some(progress) = command_progress {
                    confirmations.progress(&extension_name, &progress);
                    continue;
                }
                let Some(event) = event else {
                    continue;
                };
                match event {
                    Ok(ExtensionEvent::ConfirmationRequested {
                        request_id,
                        generation,
                        parent_request_id,
                        request,
                    }) if parent_request_id.is_some_and(|parent| {
                        operation.is_some_and(|operation| operation.owns(generation, parent))
                    }) || (legacy_uncorrelated
                        && parent_request_id.is_none()
                        && operation
                            .is_some_and(|operation| operation.generation == generation)) =>
                    {
                        if process.confirmation_answered(&request_id, generation) {
                            continue;
                        }
                        let confirmed = confirmations
                            .confirm(&extension_name, &request)
                            .await
                            .with_context(|| {
                                format!("confirmation UI failed for extension {extension_name:?}")
                            })?;
                        process
                            .respond_to_confirmation(
                                request_id,
                                generation,
                                ConfirmationResponse { confirmed },
                            )
                            .await?;
                    }
                    Ok(ExtensionEvent::PolicyEvaluationRequested { .. }) => {}
                    Ok(ExtensionEvent::InputRequested {
                        request_id,
                        generation,
                        parent_request_id,
                        request,
                    }) if operation
                        .is_some_and(|operation| operation.owns(generation, parent_request_id)) =>
                    {
                        if process.input_answered(&request_id, generation) {
                            continue;
                        }
                        let value = confirmations
                            .input(&extension_name, &request)
                            .await
                            .with_context(|| {
                                format!("input UI failed for extension {extension_name:?}")
                            })?;
                        process
                            .respond_to_input(
                                request_id,
                                generation,
                                ExtensionInputResponse { value },
                            )
                            .await?;
                    }
                    Ok(_) => {
                        // The product's persistent receiver owns ordinary
                        // notifications, status, context, and diagnostics.
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        self.diagnostics.push(format!(
                                "warning: {extension_name}: confirmation listener lagged by {count} events"
                            ));
                    }
                    Err(broadcast::error::RecvError::Closed) => events_open = false,
                }
            };
            Ok::<_, anyhow::Error>(result)
        }
        .await;
        confirmations.finish_progress(&extension_name);
        let output = output?;
        self.enqueue_contexts(&extension_name, output.context);
        let mut blocks = Vec::new();
        if !output.text.trim().is_empty() {
            blocks.push(output.text);
        }
        blocks.extend(
            output
                .notifications
                .iter()
                .map(|notification| format_notification(name, notification)),
        );
        blocks.extend(self.drain_events());
        Ok(Some(blocks.join("\n")))
    }

    /// Own the first-party stop request independently of the active Agent borrow.
    /// No confirmation can be silently approved and no extension context may be
    /// injected. The extension and host remain responsible for owner validation
    /// and for reporting terminal settlement after the stop acknowledgement.
    pub(crate) fn subagent_stop_control(
        &self,
        target: String,
        expected_owner: &str,
    ) -> anyhow::Result<Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>> {
        anyhow::ensure!(
            !expected_owner.is_empty() && self.resource_owner.as_deref() == Some(expected_owner),
            "subagent stop requires the active session owner"
        );
        anyhow::ensure!(
            self.command_owner("subagents").as_deref() == Some(SUBAGENTS_EXTENSION_NAME),
            "first-party subagent command is unavailable"
        );
        let process = self
            .processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                    && process.is_running()
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == "subagents")
            })
            .ok_or_else(|| anyhow::anyhow!("first-party subagent command is not running"))?
            .clone();
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        Ok(Box::pin(async move {
            let mut diagnostics = BoundedDiagnostics::default();
            // Use the extension runtime's normal command deadline, just like
            // idle dispatch. A shorter UI timeout can cancel stop-all halfway
            // through its owner-checked sequence of interrupts.
            let output = execute_headless_command(
                &process,
                "subagents",
                vec!["stop".into(), target],
                context,
                0,
                &mut diagnostics,
            )
            .await?;
            anyhow::ensure!(
                output.context.is_empty(),
                "subagent stop attempted context injection"
            );
            anyhow::ensure!(!output.text.contains("failed closed"), "{}", output.text);
            Ok(output.text)
        }))
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn test_subagent_stop_fixture(
        workspace: &Path,
        owner: Option<&str>,
        name: &str,
        delay_seconds: u64,
    ) -> (Self, ExtensionProcess, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let script = workspace.join("subagent-stop-fixture.sh");
        let log = workspace.join("subagent-stop-fixture.jsonl");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
request_id() {{ sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }}
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"api_version":"0.4","tools":[],"commands":[{{"name":"subagents","description":"Test owner-bound stop"}}],"protocol":{{"version":"0.4","features":["request_cancellation","content_parts","terminal_handoff"],"limits":{{"max_concurrent_requests":1}}}}}}}}\n' "$id"
while IFS= read -r request; do
  case "$request" in
    *'"method":"command/execute"'*)
      printf '%s\n' "$request" >> "$OCTET_WORKSPACE/subagent-stop-fixture.jsonl"
      id=$(printf '%s' "$request" | request_id)
      case "$request" in *'"stop"'*) sleep {} ;; esac
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"text":"interrupt requested; state stopping (not settled)","notifications":[],"context":[]}}}}\n' "$id"
      ;;
    *'"method":"shutdown"'*)
      id=$(printf '%s' "$request" | request_id)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
                delay_seconds
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = ExtensionManifest::parse(&format!(
            r#"name = {name:?}
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "subagent-stop-fixture.sh"
[contributes]
commands = ["subagents"]
"#
        ))
        .unwrap();
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
            ExtensionRuntimeConfig::new(workspace),
        )
        .await
        .unwrap();
        let mut extensions = Self::default();
        extensions.resource_owner = owner.map(str::to_owned);
        extensions.receivers.push(process.subscribe());
        extensions.processes.push(process.clone());
        (extensions, process, log)
    }

    /// Publishes a worker roster for `process` as its current semantic state.
    /// Each worker is `(node id, label, stop target)`; one without a target
    /// carries no stop action, like a settled worker.
    #[cfg(all(test, unix))]
    pub(crate) fn test_publish_worker_roster(
        &mut self,
        process: &ExtensionProcess,
        workers: &[(&str, &str, Option<&str>)],
    ) {
        let mut nodes = Vec::new();
        let mut actions = Vec::new();
        for (node_id, label, stop) in workers {
            let mut action_ids = Vec::new();
            if let Some(target) = stop {
                let id = format!("stop:{node_id}");
                actions.push(octet_agent::ExtensionPresentationAction {
                    id: id.clone(),
                    label: format!("Stop {label}"),
                    command: "subagents".into(),
                    arguments: vec!["stop".into(), (*target).to_owned()],
                    destructive: true,
                });
                action_ids.push(id);
            }
            nodes.push(octet_agent::ExtensionPresentationNode {
                id: (*node_id).to_owned(),
                parent_id: None,
                state: if stop.is_some() {
                    octet_agent::ExtensionPresentationState::Running
                } else {
                    octet_agent::ExtensionPresentationState::Succeeded
                },
                label: (*label).to_owned(),
                secondary: None,
                action_ids,
                references: Vec::new(),
            });
        }
        let name = process.descriptor().manifest.name.clone();
        self.presentations.insert(
            name.clone(),
            ExtensionPresentationView {
                extension: name,
                generation: process.health_snapshot().generation,
                extension_instance_id: process.extension_instance_id().to_owned(),
                resource_owner: None,
                snapshot: ExtensionPresentationSnapshot {
                    revision: 1,
                    status: None,
                    activities: Vec::new(),
                    collection: Some(octet_agent::ExtensionPresentationCollection {
                        kind: octet_agent::ExtensionPresentationCollectionKind::List,
                        title: "Subagents".into(),
                        nodes,
                        selected_node_id: None,
                        detail: None,
                    }),
                    actions,
                },
            },
        );
    }

    /// An owned, single-flight observation request for the active modal loop.
    /// Only the first-party status command is admitted, without consent or
    /// context-injection authority. Persistent receivers still own snapshots.
    pub(crate) fn subagent_status_check(
        &self,
    ) -> Option<Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>> {
        let process = self
            .processes
            .iter()
            .find(|process| {
                process.descriptor().manifest.name == SUBAGENTS_EXTENSION_NAME
                    && process.is_running()
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == "subagents")
            })?
            .clone();
        let context = extension_execution_context(&process, self.resource_owner.as_deref());
        Some(Box::pin(async move {
            let mut diagnostics = BoundedDiagnostics::default();
            let result = tokio::time::timeout(
                Duration::from_millis(750),
                execute_headless_command(
                    &process,
                    "subagents",
                    vec!["status".into()],
                    context,
                    0,
                    &mut diagnostics,
                ),
            )
            .await;
            for message in diagnostics.entries {
                crate::output::stderr_line(message);
            }
            if diagnostics.dropped > 0 {
                crate::output::stderr!(
                    "warning: {} extension diagnostics omitted",
                    diagnostics.dropped
                );
            }
            let output =
                result.map_err(|_| anyhow::anyhow!("subagent status refresh timed out"))??;
            anyhow::ensure!(
                output.context.is_empty(),
                "subagent status refresh attempted context injection"
            );
            if output.text.contains("failed closed") {
                anyhow::bail!("subagent status refresh failed closed");
            }
            Ok(output.text)
        }))
    }

    /// Executes an extension command at a non-interactive boundary.
    ///
    /// Extension confirmation requests are explicitly denied and reported as a
    /// failed invocation because no trusted confirmation surface is available to
    /// the caller. Commands that do not request confirmation retain ordinary
    /// output and queued-context handling.
    pub async fn execute_command_without_confirmation(
        &mut self,
        name: &str,
        arguments: Vec<String>,
    ) -> anyhow::Result<Option<String>> {
        self.execute_command_headless_scoped(None, name, arguments, 0)
            .await
    }

    async fn execute_command_headless_scoped(
        &mut self,
        extension: Option<&str>,
        name: &str,
        arguments: Vec<String>,
        approval_budget: usize,
    ) -> anyhow::Result<Option<String>> {
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                extension.is_none_or(|extension| process.descriptor().manifest.name == extension)
                    && process
                        .contributions()
                        .commands
                        .iter()
                        .any(|command| command.name == name)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let extension_name = process.descriptor().manifest.name.clone();
        let execution_context =
            extension_execution_context(&process, self.resource_owner.as_deref());
        let output = execute_headless_command(
            &process,
            name,
            arguments,
            execution_context,
            approval_budget,
            &mut self.diagnostics,
        )
        .await?;
        self.enqueue_contexts(&extension_name, output.context);
        let mut blocks = Vec::new();
        if !output.text.trim().is_empty() {
            blocks.push(output.text);
        }
        blocks.extend(
            output
                .notifications
                .iter()
                .map(|notification| format_notification(name, notification)),
        );
        blocks.extend(self.drain_events());
        Ok(Some(blocks.join("\n")))
    }

    /// Start a semantic tool renderer without stalling Agent events or input.
    /// Returns whether a matching renderer was registered.
    pub fn request_tool_render(
        &mut self,
        id: ToolCallId,
        name: &str,
        arguments: serde_json::Value,
        output: Option<String>,
        is_error: bool,
    ) -> bool {
        let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                process
                    .contributions()
                    .tool_renderers
                    .iter()
                    .any(|tool| tool == name)
            })
            .cloned()
        else {
            return false;
        };
        self.renderer_tasks.retain(|task| !task.is_finished());
        let sender = self.background_tx.clone();
        let name = name.to_owned();
        let request = ToolRenderRequest {
            name: name.clone(),
            arguments,
            output,
            is_error,
            context: process.current_context(),
        };
        self.renderer_tasks.push(tokio::spawn(async move {
            let (update, diagnostic) =
                match tokio::time::timeout(RENDERER_RPC_DEADLINE, process.render_tool(request))
                    .await
                {
                    Err(_) => (
                        None,
                        Some(format!(
                            "warning: renderer for {name:?} exceeded {RENDERER_RPC_DEADLINE:?}"
                        )),
                    ),
                    Ok(Err(error)) => (
                        None,
                        Some(format!("warning: renderer for {name:?} failed: {error}")),
                    ),
                    Ok(Ok(rendered)) => (
                        Some(ExtensionToolRenderUpdate {
                            id,
                            segments: rendered.segments,
                        }),
                        None,
                    ),
                };
            let _ = sender
                .send(ExtensionBackgroundUpdate::Renderer { update, diagnostic })
                .await;
        }));
        true
    }

    /// Start one host-mediated autocomplete request for the active editor
    /// snapshot. A late result is fenced by the shell revision before display.
    pub fn request_editor_autocomplete(&mut self, snapshot: ShellEditorSnapshot) -> bool {
        self.prune_semantic_ui();
        self.autocomplete_tasks.retain(|task| !task.is_finished());
        if self.autocomplete_tasks.len() >= MAX_EXTENSION_AUTOCOMPLETE_TASKS {
            return true;
        }
        let Some(registration) = self.autocomplete_registrations.values().next().cloned() else {
            return false;
        };
        let process = registration.process;
        if !process.is_running()
            || process.health_snapshot().generation != registration.generation
            || process.extension_instance_id() != registration.extension_instance_id
        {
            return false;
        }
        let request = ExtensionAutocompleteRequest {
            text: snapshot.text.clone(),
            cursor: snapshot.cursor,
            revision: snapshot.revision,
        };
        let sender = self.background_tx.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {}: autocomplete requires the Tokio runtime",
                process.descriptor().manifest.name
            ));
            return false;
        };
        self.autocomplete_tasks.push(handle.spawn(async move {
            let (update, diagnostic) = match tokio::time::timeout(
                EXTENSION_AUTOCOMPLETE_DEADLINE,
                process.request_autocomplete(request),
            )
            .await
            {
                Err(_) => (
                    None,
                    Some(format!(
                        "warning: extension autocomplete exceeded {EXTENSION_AUTOCOMPLETE_DEADLINE:?}"
                    )),
                ),
                Ok(Err(error)) => (
                    None,
                    Some(format!("warning: extension autocomplete failed: {error}")),
                ),
                Ok(Ok(response)) => (
                    Some(ExtensionAutocompleteUpdate {
                        snapshot,
                        prefix: response.prefix,
                        items: response
                            .items
                            .into_iter()
                            .map(|item| ShellAutocompleteItem {
                                value: item.value,
                                label: item.label,
                                description: item.description,
                            })
                            .collect(),
                    }),
                    None,
                ),
            };
            let _ = sender
                .send(ExtensionBackgroundUpdate::Autocomplete { update, diagnostic })
                .await;
        }));
        true
    }

    pub(crate) fn set_telemetry(&mut self, telemetry: Option<octet_agent::TelemetryObserver>) {
        self.telemetry = telemetry;
    }

    /// Status is memory-only. Actual rejected records are new loss events, not
    /// repeatable configuration problems, and never affect session accounting.
    fn poll_telemetry(&mut self) {
        let Some(observer) = &self.telemetry else {
            return;
        };
        let status = observer.status();
        if status.rejected_records > self.telemetry_rejected {
            crate::output::stderr!(
                "warning: optional telemetry lost {} record(s); session accounting is unaffected",
                status.rejected_records - self.telemetry_rejected
            );
            self.telemetry_rejected = status.rejected_records;
        }
        if status.write_error != self.telemetry_error {
            if let Some(error) = status.write_error {
                crate::output::stderr!("warning: optional telemetry writer failed: {error:?}");
            }
            self.telemetry_error = status.write_error;
        }
    }

    async fn shutdown_telemetry(&mut self) {
        self.poll_telemetry();
        let Some(observer) = self.telemetry.take() else {
            return;
        };
        // Neither disk waits nor bounded writer joins belong on the async
        // control owner. Await the explicit deadline before normal exit/rebuild.
        if let Err(error) =
            tokio::task::spawn_blocking(move || shutdown_telemetry_observer(observer)).await
        {
            crate::output::stderr!("warning: optional telemetry shutdown worker failed: {error}");
        }
    }

    /// Drain completed renderer and autocomplete work without waiting.
    pub fn drain_background_updates(&mut self) -> ExtensionBackgroundUpdates {
        self.poll_telemetry();
        let mut updates = ExtensionBackgroundUpdates::default();
        while let Ok(update) = self.background_rx.try_recv() {
            match update {
                ExtensionBackgroundUpdate::Renderer { update, diagnostic } => {
                    self.diagnostics.extend(diagnostic);
                    updates.rendered_tools.extend(update);
                }
                ExtensionBackgroundUpdate::Autocomplete { update, diagnostic } => {
                    self.diagnostics.extend(diagnostic);
                    updates.autocomplete.extend(update);
                }
                ExtensionBackgroundUpdate::Shortcut {
                    extension,
                    context,
                    messages,
                } => {
                    self.enqueue_contexts(&extension, context);
                    updates.shortcut_messages.extend(messages);
                }
            }
        }
        updates
    }

    fn cancel_background_work(&mut self) {
        for task in self.renderer_tasks.drain(..) {
            task.abort();
        }
        for task in self.autocomplete_tasks.drain(..) {
            task.abort();
        }
        for task in self.shortcut_tasks.drain(..) {
            task.abort();
        }
        for task in self.confirmation_tasks.drain(..) {
            task.abort();
        }
        for task in self.input_tasks.drain(..) {
            task.abort();
        }
        for task in self.policy_supervisors.drain(..) {
            task.abort();
        }
        self.confirmation_denials.clear();
        self.input_cancellations.clear();
        while self.background_rx.try_recv().is_ok() {}
    }

    /// Delivers one completed, host-owned mutation to declared API `0.2`
    /// post-mutation hooks and returns only subset-validated rescan requests.
    ///
    /// Call this only after durable commit or completed rollback. Duplicate
    /// mutation identities are ignored across process reloads/restarts owned by
    /// this `ExecutableExtensions` instance. The returned requests are queued
    /// for the product resource resolver; hooks never get paths, contents, or
    /// permission to perform the mutation themselves.
    pub async fn notify_post_mutation(
        &mut self,
        mutation: PostMutationContext,
    ) -> Vec<PostMutationRescan> {
        if self
            .seen_post_mutation_ids
            .iter()
            .any(|seen| seen == mutation.mutation_id())
        {
            return Vec::new();
        }
        while self.seen_post_mutation_ids.len() >= MAX_SEEN_POST_MUTATION_IDS {
            self.seen_post_mutation_ids.pop_front();
        }
        self.seen_post_mutation_ids
            .push_back(mutation.mutation_id().to_owned());
        if mutation.kind() != PostMutationKind::Resource {
            for resource in mutation
                .affected_resources()
                .iter()
                .filter(|resource| mutation_resources::known(resource))
            {
                let current = self
                    .mutation_family_generations
                    .entry(resource.clone())
                    .or_default();
                *current = (*current).max(mutation.generation());
            }
        }

        let resource_owner = self.resource_owner.clone();
        let calls = self
            .processes
            .iter()
            .filter(|process| {
                process
                    .contributions()
                    .hooks
                    .contains(&ExtensionHook::PostMutation)
            })
            .cloned()
            .map(|process| {
                let name = process.descriptor().manifest.name.clone();
                let process_generation = process.health_snapshot().generation;
                let mutation = mutation.clone();
                let resource_owner = resource_owner.clone();
                async move {
                    let result = tokio::time::timeout(
                        POST_MUTATION_RPC_DEADLINE,
                        process.post_mutation(&mutation, resource_owner.as_deref()),
                    )
                    .await;
                    (name, process_generation, result)
                }
            });
        let results = futures_util::future::join_all(calls).await;
        let mut accepted = Vec::new();
        for (extension, process_generation, result) in results {
            let disposition = match result {
                Ok(Ok(disposition)) => disposition,
                Ok(Err(error)) => {
                    self.diagnostics.push(format!(
                        "warning: extension {extension:?} post_mutation hook failed: {error}"
                    ));
                    continue;
                }
                Err(_) => {
                    self.diagnostics.push(format!(
                        "warning: extension {extension:?} post_mutation hook exceeded {POST_MUTATION_RPC_DEADLINE:?}"
                    ));
                    continue;
                }
            };
            let Some(resource_ids) = disposition.resource_ids() else {
                continue;
            };
            if resource_ids.iter().any(|resource| {
                mutation
                    .affected_resources()
                    .binary_search(resource)
                    .is_err()
            }) {
                self.diagnostics.push(format!(
                    "warning: extension {extension:?} requested a post_mutation rescan outside the affected resource set"
                ));
                continue;
            }
            let request = PostMutationRescan {
                extension,
                mutation_id: mutation.mutation_id().to_owned(),
                kind: mutation.kind(),
                process_generation,
                generation: mutation.generation(),
                resource_ids: resource_ids.to_vec(),
            };
            while self.pending_post_mutation_rescans.len() >= MAX_PENDING_POST_MUTATION_RESCANS {
                self.pending_post_mutation_rescans.pop_front();
            }
            self.pending_post_mutation_rescans
                .push_back(request.clone());
            accepted.push(request);
        }
        accepted
    }

    /// Observe a completed user-configuration transaction. Never call for a
    /// preview, failed/partial write, or an in-memory-only setting change.
    /// The caller owns the stable ID and increasing resource generation.
    pub async fn notify_configuration_changed(
        &mut self,
        mutation_id: impl Into<String>,
        generation: u64,
        state: PostMutationState,
    ) -> Vec<PostMutationRescan> {
        let Some(mutation) = PostMutationContext::new(
            mutation_id,
            PostMutationKind::Configuration,
            ["resource:settings".to_owned()],
            generation,
            state,
        ) else {
            self.diagnostics
                .push("warning: rejected invalid configuration post_mutation notification");
            return Vec::new();
        };
        self.notify_post_mutation(mutation).await
    }

    /// Convenience bridge for a host-owned committing migration integration.
    ///
    /// Dry-run scanners must never invoke this method. A committing ingestion
    /// path must have a safely bound extension owner, pass the same stable ID
    /// on retry, and call this only after commit or a completed rollback.
    ///
    /// The post-mutation hook suite that exercises it drives real extension
    /// processes, so this exists on unix test builds only.
    #[cfg(all(test, unix))]
    pub async fn notify_migration_ingested(
        &mut self,
        mutation_id: impl Into<String>,
        affected_resources: impl IntoIterator<Item = String>,
        generation: u64,
        state: PostMutationState,
    ) -> Vec<PostMutationRescan> {
        let Some(mutation) = PostMutationContext::new(
            mutation_id,
            PostMutationKind::MigrationIngestion,
            affected_resources,
            generation,
            state,
        ) else {
            self.diagnostics
                .push("warning: rejected invalid migration post_mutation notification");
            return Vec::new();
        };
        self.notify_post_mutation(mutation).await
    }

    /// Drains host-validated rescan requests for the product resource owner.
    ///
    /// The queue contains no raw paths or contents and is bounded independently
    /// of extension event/progress channels.
    pub fn take_post_mutation_rescans(&mut self) -> Vec<PostMutationRescan> {
        self.pending_post_mutation_rescans.drain(..).collect()
    }

    /// Drains and re-resolves post-mutation rescan requests through the
    /// discovery configuration bound at construction.
    ///
    /// This is the product drain for mutations that have no reload path of their
    /// own (for example a configuration commit). It fails closed: when no
    /// discovery configuration is bound the queue is discarded with a bounded
    /// diagnostic rather than resolved against guessed roots, and a request for a
    /// stale generation or stopped process is dropped without re-entering it.
    pub fn drain_post_mutation_rescans(&mut self) -> Vec<String> {
        self.drain_post_mutation_report().into_notices()
    }

    fn drain_post_mutation_report(&mut self) -> ExtensionRescanReport {
        let Some(config) = self.rescan_config.clone() else {
            let dropped = self
                .take_post_mutation_rescans()
                .into_iter()
                .map(|request| request.resource_ids.len())
                .sum::<usize>();
            return ExtensionRescanReport {
                events: if dropped == 0 {
                    Vec::new()
                } else {
                    vec![format!(
                    "warning: discarded {dropped} post_mutation rescan request(s); no discovery configuration is bound"
                )]
                },
                ..Default::default()
            };
        };
        self.rescan_post_mutation_report(&config)
    }

    /// Re-resolves selected extension resources through the same trust,
    /// precedence, no-follow and byte bounds as initial discovery. This is
    /// read-only: changed sources require an explicit product rebuild, never
    /// implicit activation or a recursive reload from an observational hook.
    pub(crate) fn rescan_post_mutation_resources(&mut self, config: &Config) -> Vec<String> {
        self.rescan_post_mutation_report(config).into_notices()
    }

    fn rescan_post_mutation_report(&mut self, config: &Config) -> ExtensionRescanReport {
        let requests = self.take_post_mutation_rescans();
        let mut output = ExtensionRescanReport::default();
        let mut selected = BTreeMap::new();
        let mut families = BTreeMap::new();
        for request in requests {
            let requester_current = self.processes.iter().any(|process| {
                process.descriptor().manifest.name == request.extension
                    && process.is_running()
                    && process.health_snapshot().generation == request.process_generation
            });
            if !requester_current {
                output
                    .events
                    .push("warning: discarded stale post_mutation requesting process".into());
                continue;
            }
            for resource_id in request.resource_ids {
                if request.kind != PostMutationKind::Resource
                    && mutation_resources::known(&resource_id)
                {
                    if self.mutation_family_generations.get(&resource_id)
                        == Some(&request.generation)
                    {
                        families.insert(resource_id, request.generation);
                    } else {
                        output.events.push(
                            "warning: discarded stale post_mutation resource generation".into(),
                        );
                    }
                    continue;
                }
                let process = self.processes.iter().find(|process| {
                    opaque_extension_resource_id(&process.descriptor().manifest.name) == resource_id
                });
                let Some(process) = process.filter(|process| {
                    process.is_running()
                        && process.health_snapshot().generation == request.generation
                }) else {
                    output.events.push(
                        "warning: discarded stale or unavailable post_mutation resource rescan"
                            .into(),
                    );
                    continue;
                };
                selected.insert(
                    resource_id,
                    (process.descriptor().clone(), request.generation),
                );
            }
        }
        for (resource, generation) in families {
            output.events.push(mutation_resources::rescan(
                &resource,
                generation,
                config,
                self.rescan_global_config.as_deref(),
            ));
        }
        if selected.is_empty() {
            return output;
        }
        let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
        let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
        let mut problems = Vec::new();
        let (policy, _) = extension_policy(config, &mut problems);
        output.checked.push(("policy".into(), problems));
        for (_, (previous, generation)) in selected {
            let name = &previous.manifest.name;
            let key = format!("resource:{name}");
            let mut problems = Vec::new();
            let Some(resource) = snapshot
                .resources()
                .iter()
                .find(|resource| &resource.name == name)
            else {
                output.checked.push((
                    key,
                    vec![format!(
                        "warning: rescanned extension {name:?} is unavailable; run /reload"
                    )],
                ));
                continue;
            };
            let Some(mut current) =
                load_extension_descriptor(&resolver, resource, &policy, &mut problems)
            else {
                output.checked.push((key, problems));
                continue;
            };
            apply_experimental_streamable_http_mcp_gate(
                &mut current,
                config.experimental_streamable_http_mcp,
            );
            if current != previous {
                problems.push(format!("warning: rescanned extension {name:?} changed; run /reload before using the new resource"));
                output.checked.push((key, problems));
                continue;
            }
            output.checked.push((key, problems));
            output.details.push(format!(
                "rescanned extension {name:?} (generation {generation})"
            ));
        }
        output
    }

    async fn settle_session_lifecycle(&mut self) {
        if self.session_lifecycle_started {
            let outcome = self
                .last_lifecycle_outcome
                .unwrap_or(ExtensionLifecycleOutcome::Shutdown);
            if let Some(resource_owner) = self.resource_owner.clone() {
                let diagnostics =
                    settle_session_hooks_all(&self.processes, &resource_owner, outcome).await;
                self.diagnostics.extend(diagnostics);
            }
            if let Some(session_id) = self.session_id.clone() {
                let diagnostics = notify_lifecycle_all(
                    &self.processes,
                    ExtensionLifecycleEvent::SessionSettled {
                        session_id,
                        run_id: None,
                        outcome,
                        duration_ms: duration_millis(self.session_started_at.elapsed()),
                        reason: None,
                    },
                )
                .await;
                self.diagnostics.extend(diagnostics);
            }
            self.session_lifecycle_started = false;
        }
    }

    /// Releases this App/session's attachment to the durable process fleet.
    ///
    /// Isolated profiles are stopped. Explicitly shared workspace services are
    /// deliberately left with the runtime manager so a compatible replacement
    /// App can bind them without a stop/restart gap. Interactive callers must
    /// revoke the terminal grant with their shell before releasing this owner.
    pub async fn release_binding(&mut self) {
        // A replacement App must not inherit work queued against the old
        // owner. Isolated lifecycle processes are stopped below; shared and
        // legacy processes never receive this service.
        self.deactivate_session_lifecycle_driver();
        self.cancel_background_work();
        self.settle_session_lifecycle().await;
        for process in &self.processes {
            process.detach_dynamic_tool_catalog();
        }
        if let Some(binding) = self.runtime_binding.take() {
            // `ExtensionProcess::shutdown` is individually bounded. Do not
            // cancel binding release midway: a dropped release future has
            // already closed the binding and must finish detaching its keys.
            binding.release().await;
        } else {
            let processes = self.processes.clone();
            let shutdowns =
                futures_util::future::join_all(processes.iter().map(ExtensionProcess::shutdown));
            let _ = tokio::time::timeout(SHUTDOWN_DEADLINE, shutdowns).await;
        }
        self.processes.clear();
        self.receivers.clear();
        for summary in &mut self.summaries {
            summary.running = false;
        }
        self.shutdown_telemetry().await;
    }

    /// Synchronous App rebuild boundary that preserves the durable manager.
    pub fn release_binding_blocking(&mut self) {
        let _ = block_on_runtime(self.release_binding());
    }

    /// Gracefully stops every runtime owned by this host after releasing the
    /// current session binding. Each protocol shutdown has its own hard timeout
    /// in `ExtensionProcess`; the outer timeout prevents terminal restoration
    /// from being delayed indefinitely.
    pub async fn shutdown(&mut self) {
        self.release_binding().await;
        if let Some(manager) = self.runtime_manager.take() {
            let _ = tokio::time::timeout(SHUTDOWN_DEADLINE, manager.shutdown()).await;
        }
    }

    /// Synchronous terminal shutdown boundary.
    pub fn shutdown_blocking(&mut self) {
        let _ = block_on_runtime(self.shutdown());
    }

    // Compatibility convenience for in-crate lifecycle tests; production
    // callers retain typed reload outcomes and report problems separately.
    // Every caller drives real extension processes, so this exists on unix
    // test builds only.
    #[cfg(all(test, unix))]
    pub async fn reload(&mut self) -> Vec<String> {
        self.reload_report().await.into_notices()
    }

    pub(crate) async fn reload_report(&mut self) -> ExtensionReloadReport {
        self.cancel_background_work();
        // Both runtime ownership modes settle through the same notification
        // boundary. In particular, the product's manager-backed path must not
        // skip PostMutation delivery after a successful generation replacement.
        let results = if let Some(manager) = self.runtime_manager.clone() {
            let names = self
                .processes
                .iter()
                .map(|process| process.descriptor().manifest.name.clone())
                .collect::<BTreeSet<_>>();
            futures_util::future::join_all(names.into_iter().map(|name| {
                let manager = manager.clone();
                async move { (name.clone(), manager.reload(&name).await) }
            }))
            .await
            .into_iter()
            .flat_map(|(name, results)| {
                results
                    .into_iter()
                    .map(move |result| (name.clone(), result.map_err(|error| error.to_string())))
            })
            .collect::<Vec<_>>()
        } else {
            let reloads = self.processes.iter().cloned().map(|process| async move {
                let name = process.descriptor().manifest.name.clone();
                (
                    name,
                    process.reload().await.map_err(|error| error.to_string()),
                )
            });
            // Concurrent polling preserves input order without serializing
            // unrelated extension reloads behind a hung child.
            futures_util::future::join_all(reloads).await
        };
        let mut output = ExtensionReloadReport::default();
        let mut reloaded = BTreeSet::new();
        let mut completed_mutations = Vec::new();
        for (name, result) in results {
            match result {
                Ok(report) => {
                    reloaded.insert(name.clone());
                    output.processes.push((
                        name.clone(),
                        Ok(format!(
                            "reloaded {name} (generation {}, previous shutdown {})",
                            report.generation,
                            if report.previous_shutdown_graceful {
                                "clean"
                            } else {
                                "forced"
                            }
                        )),
                    ));
                    let resource = opaque_extension_resource_id(&name);
                    let mutation_id = format!("resource-reload:{resource}:{}", report.generation);
                    if let Some(mutation) = PostMutationContext::new(
                        mutation_id,
                        PostMutationKind::Resource,
                        vec![resource],
                        report.generation,
                        PostMutationState::Committed,
                    ) {
                        completed_mutations.push(mutation);
                    }
                }
                Err(error) => output.processes.push((
                    name.clone(),
                    Err(format!("unable to reload {name}: {error}")),
                )),
            }
        }
        self.await_reloaded_provider_registrations(&reloaded).await;
        for mutation in completed_mutations {
            let rescans = self.notify_post_mutation(mutation).await;
            output.details.extend(rescans.into_iter().map(|request| {
                format!(
                    "extension {:?} requested bounded rescan of {} resource(s)",
                    request.extension,
                    request.resource_ids.len()
                )
            }));
        }
        self.start_policy_supervisors();
        let (shortcuts, diagnostics) = register_extension_shortcuts(&self.processes);
        self.shortcuts = shortcuts;
        output.shortcuts = diagnostics;
        output.events = self.drain_events();
        output.events.extend(self.discard_stale_host_requests());
        // A generation replacement is a product resource mutation. Drain the
        // bounded rescan queue it just admitted so an admitted hook cannot leave
        // resolver work queued forever. Re-resolution reuses the same trusted
        // discovery path; a stale or unavailable owner is dropped with a
        // diagnostic and a changed source is never activated implicitly.
        output.rescans = self.drain_post_mutation_report();
        output
    }

    /// Waits for post-initialize provider declarations from just-reloaded owners
    /// before callers can reconcile their host-owned model routes.
    async fn await_reloaded_provider_registrations(&self, reloaded: &BTreeSet<String>) {
        if reloaded.is_empty() {
            return;
        }
        let processes = self
            .processes
            .iter()
            .filter(|process| reloaded.contains(&process.descriptor().manifest.name))
            .cloned()
            .collect::<Vec<_>>();
        self.provider_runtime
            .await_initial_registrations_async(&processes)
            .await;
    }

    fn schedule_confirmation_denials(&mut self) {
        self.confirmation_tasks.retain(|task| !task.is_finished());
        if tokio::runtime::Handle::try_current().is_err() {
            if !self.confirmation_denials.is_empty() {
                self.diagnostics.push(
                    "warning: extension confirmation denials require the octet Tokio runtime",
                );
            }
            return;
        }
        while self.confirmation_tasks.len() < CONFIRMATION_DENIAL_CONCURRENCY {
            let Some(pending) = self.confirmation_denials.pop_front() else {
                break;
            };
            self.confirmation_tasks.push(tokio::spawn(async move {
                let _ = pending
                    .process
                    .respond_to_confirmation(
                        pending.request_id,
                        pending.generation,
                        ConfirmationResponse { confirmed: false },
                    )
                    .await;
            }));
        }
    }

    fn schedule_input_cancellations(&mut self) {
        self.input_tasks.retain(|task| !task.is_finished());
        if tokio::runtime::Handle::try_current().is_err() {
            if !self.input_cancellations.is_empty() {
                self.diagnostics
                    .push("warning: extension input cancellation requires the octet Tokio runtime");
            }
            return;
        }
        while self.input_tasks.len() < INPUT_CANCELLATION_CONCURRENCY {
            let Some(pending) = self.input_cancellations.pop_front() else {
                break;
            };
            self.input_tasks.push(tokio::spawn(async move {
                let _ = pending
                    .process
                    .respond_to_input(
                        pending.request_id,
                        pending.generation,
                        ExtensionInputResponse { value: None },
                    )
                    .await;
            }));
        }
    }

    fn queue_confirmation_denial(&mut self, pending: PendingConfirmationDenial) {
        if self.confirmation_denials.len() >= CONFIRMATION_DENIAL_QUEUE_CAPACITY {
            self.diagnostics.push(format!(
                "warning: extension confirmation denial queue reached its {CONFIRMATION_DENIAL_QUEUE_CAPACITY}-request limit; newest request was dropped"
            ));
            return;
        }
        self.confirmation_denials.push_back(pending);
        self.schedule_confirmation_denials();
    }

    fn queue_input_cancellation(&mut self, pending: PendingInputCancellation) {
        if self.input_cancellations.len() >= INPUT_CANCELLATION_QUEUE_CAPACITY {
            self.diagnostics.push(format!(
                "warning: extension input cancellation queue reached its {INPUT_CANCELLATION_QUEUE_CAPACITY}-request limit; newest request was dropped"
            ));
            return;
        }
        self.input_cancellations.push_back(pending);
        self.schedule_input_cancellations();
    }

    fn apply_semantic_ui_contribution(
        &mut self,
        extension: String,
        process: &ExtensionProcess,
        generation: u64,
        contribution: ExtensionUiContribution,
    ) -> Result<(), String> {
        let health = process.health_snapshot();
        if !process.is_running() || health.generation != generation {
            return Err(format!(
                "discarded semantic UI contribution from stale generation {generation}"
            ));
        }
        let instance_id = process.extension_instance_id().to_owned();
        let view = self.semantic_ui.entry(extension).or_default();
        if view.generation != generation || view.extension_instance_id != instance_id {
            *view = SemanticUiView {
                extension_instance_id: instance_id,
                generation,
                ..SemanticUiView::default()
            };
        }
        match contribution {
            ExtensionUiContribution::Status {
                key,
                text,
                style_role,
                priority,
            } => {
                if let Some(text) = text {
                    if !view.statuses.contains_key(&key)
                        && view.statuses.len().saturating_add(view.widgets.len())
                            >= MAX_EXTENSION_UI_ENTRIES
                    {
                        return Err(format!(
                            "semantic UI entry limit {MAX_EXTENSION_UI_ENTRIES} reached"
                        ));
                    }
                    view.statuses.insert(
                        key,
                        SemanticUiStatus {
                            text,
                            style_role,
                            priority,
                        },
                    );
                } else {
                    view.statuses.remove(&key);
                }
            }
            ExtensionUiContribution::Widget {
                key,
                lines,
                placement,
                style_role,
                priority,
            } => {
                if let Some(lines) = lines {
                    if !view.widgets.contains_key(&key)
                        && view.statuses.len().saturating_add(view.widgets.len())
                            >= MAX_EXTENSION_UI_ENTRIES
                    {
                        return Err(format!(
                            "semantic UI entry limit {MAX_EXTENSION_UI_ENTRIES} reached"
                        ));
                    }
                    view.widgets.insert(
                        key,
                        SemanticUiWidget {
                            lines,
                            placement,
                            style_role,
                            priority,
                        },
                    );
                } else {
                    view.widgets.remove(&key);
                }
            }
            ExtensionUiContribution::Working {
                message,
                visible,
                frames,
                interval_ms,
            } => {
                view.working = Some(ShellExtensionWorking {
                    message,
                    visible,
                    frames,
                    interval_ms,
                });
            }
            ExtensionUiContribution::HiddenThinking { label } => {
                view.hidden_thinking_label = label;
            }
        }
        Ok(())
    }

    /// Apply one header/footer status surface against the live process
    /// generation. Keyed `status` surfaces flow through
    /// [`Self::apply_semantic_ui_contribution`] and are ignored here.
    fn apply_status_surface(
        &mut self,
        extension: String,
        process: &ExtensionProcess,
        contribution: ExtensionStatusContribution,
    ) {
        self.record_status_surface(
            extension,
            process.extension_instance_id().to_owned(),
            process.health_snapshot().generation,
            contribution,
        );
    }

    /// Store one header/footer surface under its exact instance and generation
    /// fence. Split from [`Self::apply_status_surface`] so the store step is
    /// testable without a live process.
    fn record_status_surface(
        &mut self,
        extension: String,
        instance_id: String,
        generation: u64,
        contribution: ExtensionStatusContribution,
    ) {
        let is_header = match contribution.surface {
            ExtensionUiSurface::Header => true,
            ExtensionUiSurface::Footer => false,
            ExtensionUiSurface::Status => return,
        };
        let view = self.semantic_ui.entry(extension).or_default();
        if view.generation != generation || view.extension_instance_id != instance_id {
            *view = SemanticUiView {
                extension_instance_id: instance_id,
                generation,
                ..SemanticUiView::default()
            };
        }
        let slot = (!contribution.text.is_empty()).then(|| SemanticUiStatus {
            text: bounded_surface_text(&contribution.text),
            style_role: contribution.style_role,
            priority: contribution.priority,
        });
        if is_header {
            view.header = slot;
        } else {
            view.footer = slot;
        }
    }

    fn prune_semantic_ui(&mut self) {
        self.semantic_ui.retain(|extension, view| {
            self.processes.iter().any(|process| {
                process.descriptor().manifest.name == *extension
                    && process.is_running()
                    && process.extension_instance_id() == view.extension_instance_id
                    && process.health_snapshot().generation == view.generation
            })
        });
        self.autocomplete_registrations
            .retain(|extension, registration| {
                self.processes.iter().any(|process| {
                    process.descriptor().manifest.name == *extension
                        && process.is_running()
                        && process.extension_instance_id() == registration.extension_instance_id
                        && process.health_snapshot().generation == registration.generation
                })
            });
    }

    fn semantic_ui_projection(&mut self) -> ShellExtensionUi {
        self.prune_semantic_ui();
        Self::project_semantic_ui(&self.semantic_ui)
    }

    /// Fold every retained semantic-UI view into one shell projection. Split
    /// from [`Self::semantic_ui_projection`] so the fold is testable without a
    /// live process fleet.
    fn project_semantic_ui(views: &BTreeMap<String, SemanticUiView>) -> ShellExtensionUi {
        let mut statuses = Vec::new();
        let mut above_editor = Vec::new();
        let mut below_editor = Vec::new();
        let mut header = Vec::new();
        let mut footer = Vec::new();
        let mut working = None;
        let mut hidden_thinking_label = None;
        for view in views.values() {
            for status in view.statuses.values() {
                statuses.push(ShellExtensionUiLine {
                    text: status.text.clone(),
                    style_role: status.style_role.clone(),
                    priority: status.priority,
                });
            }
            for widget in view.widgets.values() {
                let target = match widget.placement {
                    ExtensionWidgetPlacement::AboveEditor => &mut above_editor,
                    ExtensionWidgetPlacement::BelowEditor => &mut below_editor,
                };
                target.extend(
                    widget
                        .lines
                        .iter()
                        .cloned()
                        .map(|text| ShellExtensionUiLine {
                            text,
                            style_role: widget.style_role.clone(),
                            priority: widget.priority,
                        }),
                );
            }
            if working.is_none() {
                working = view.working.clone();
            }
            if hidden_thinking_label.is_none() {
                hidden_thinking_label = view.hidden_thinking_label.clone();
            }
            if let Some(surface) = &view.header {
                header.push(ShellExtensionUiLine {
                    text: surface.text.clone(),
                    style_role: surface.style_role.clone(),
                    priority: surface.priority,
                });
            }
            if let Some(surface) = &view.footer {
                footer.push(ShellExtensionUiLine {
                    text: surface.text.clone(),
                    style_role: surface.style_role.clone(),
                    priority: surface.priority,
                });
            }
        }
        let sort = |left: &ShellExtensionUiLine, right: &ShellExtensionUiLine| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.text.cmp(&right.text))
        };
        statuses.sort_by(sort);
        above_editor.sort_by(sort);
        below_editor.sort_by(sort);
        header.sort_by(sort);
        footer.sort_by(sort);
        statuses.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        above_editor.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        below_editor.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        header.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        footer.truncate(MAX_PROJECTED_EXTENSION_UI_LINES);
        ShellExtensionUi {
            statuses,
            above_editor,
            below_editor,
            header,
            footer,
            working,
            hidden_thinking_label,
        }
    }

    /// Project validated semantic extension UI into the host-owned interactive
    /// shell. Any stale process generation is discarded before rendering.
    pub fn sync_semantic_ui(&mut self, shell: &mut InteractiveShell) -> bool {
        shell.set_extension_ui(self.semantic_ui_projection())
    }

    /// Notify negotiated editor-handoff extensions whenever the host-owned
    /// editor snapshot changes. The cursor deliberately remains host-local;
    /// autocomplete receives it only in its explicit request payload.
    pub fn sync_editor_state(&mut self, snapshot: ShellEditorSnapshot) {
        let state = ExtensionEditorResponse {
            text: snapshot.text,
            revision: snapshot.revision,
            focused: snapshot.focused,
        };
        let generations = self
            .processes
            .iter()
            .filter(|process| process.is_running())
            .map(|process| {
                (
                    process.descriptor().manifest.name.clone(),
                    (
                        process.health_snapshot().generation,
                        process.extension_instance_id().to_owned(),
                    ),
                )
            })
            .collect();
        let delivery = EditorStateDelivery { state, generations };
        if self.last_editor_state.as_ref() == Some(&delivery) {
            return;
        }
        self.last_editor_state = Some(delivery.clone());
        for process in &self.processes {
            if let Err(error) = process.notify_editor_state(delivery.state.clone()) {
                self.diagnostics.push(format!(
                    "warning: {}: editor state notification failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
    }

    /// Broadcast one already-normalized input observation without granting any
    /// extension an input-consumption path.
    pub fn observe_terminal_input(&mut self, data: String) {
        for process in &self.processes {
            if let Err(error) =
                process.notify_terminal_input(ExtensionTerminalInput { data: data.clone() })
            {
                self.diagnostics.push(format!(
                    "warning: {}: terminal input observation failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
    }

    /// Broadcast one host-observed resize without letting extensions own layout.
    pub fn observe_terminal_resize(&mut self, columns: u16, rows: u16) {
        for process in &self.processes {
            if let Err(error) =
                process.notify_terminal_resize(ExtensionTerminalResize { columns, rows })
            {
                self.diagnostics.push(format!(
                    "warning: {}: terminal resize observation failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
    }

    fn queue_editor_response(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        response: ExtensionEditorResponse,
    ) {
        let name = process.descriptor().manifest.name.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {name}: host-owned editor response requires the Tokio runtime"
            ));
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(
                EXTENSION_UI_EDITOR_RESPONSE_DEADLINE,
                process.respond_to_editor(request_id, generation, response),
            )
            .await;
        });
    }

    fn queue_autocomplete_registration_response(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        accepted: bool,
    ) {
        let name = process.descriptor().manifest.name.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {name}: autocomplete registration response requires the Tokio runtime"
            ));
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(
                EXTENSION_UI_EDITOR_RESPONSE_DEADLINE,
                process.respond_to_autocomplete_registration(request_id, generation, accepted),
            )
            .await;
        });
    }

    /// Fan one host lifecycle notification out to every live process that
    /// negotiated `feature`. Failures stay bounded diagnostics, never panics.
    fn notify_lifecycle_v2(
        &mut self,
        feature: &str,
        notify: impl Fn(&ExtensionProcess) -> Result<(), String>,
    ) {
        let mut failures = Vec::new();
        for process in &self.processes {
            if !process.supports_feature(feature) {
                continue;
            }
            if let Err(error) = notify(process) {
                failures.push(format!(
                    "warning: {}: lifecycle notification failed: {error}",
                    process.descriptor().manifest.name
                ));
            }
        }
        for failure in failures {
            self.diagnostics.push(failure);
        }
    }

    /// Announce the start of one host compaction boundary.
    pub fn notify_compaction_started_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_compaction_started()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce the settled state of one host compaction boundary.
    pub fn notify_compaction_settled_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_compaction_settled()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce a failed host compaction boundary with a bounded reason.
    pub fn notify_compaction_failed_all(&mut self, reason: &str) {
        let reason = bounded_notification_reason(reason);
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_compaction_failed(reason)
                .map_err(|error| error.to_string())
        });
    }

    /// Announce that the foreground model selection changed.
    pub fn notify_model_selected_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_model_selected()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce that the foreground reasoning selection changed.
    pub fn notify_reasoning_selected_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_reasoning_selected()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce that the foreground session metadata changed.
    pub fn notify_session_info_changed_all(&mut self) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_session_info_changed()
                .map_err(|error| error.to_string())
        });
    }

    /// Announce the first streamed increment of one assistant message.
    pub fn notify_message_started_all(&mut self, message_id: &str) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_message_started(message_id)
                .map_err(|error| error.to_string())
        });
    }

    /// Forward one streamed assistant increment to the host coalescer, which
    /// owns every batching and flush decision.
    pub fn push_message_delta_all(&mut self, delta: &str) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .push_message_delta(delta)
                .map_err(|error| error.to_string())
        });
    }

    /// Close one assistant message boundary after flushing coalesced deltas.
    pub fn notify_message_settled_all(&mut self, message_id: &str) {
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_message_settled(message_id)
                .map_err(|error| error.to_string())
        });
    }

    /// Announce one executed user `!`/`!!` shell escape with bounded text.
    pub fn notify_user_bash_all(&mut self, command: &str) {
        let command = bounded_notification_command(command);
        self.notify_lifecycle_v2(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2, |process| {
            process
                .notify_user_bash(command)
                .map_err(|error| error.to_string())
        });
    }

    /// Answer one host-mediated extension request exactly once without blocking
    /// the terminal thread.
    fn queue_host_request_response(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        outcome: ExtensionRequestOutcome,
    ) {
        let name = process.descriptor().manifest.name.clone();
        let Ok(handle) = Handle::try_current() else {
            self.diagnostics.push(format!(
                "warning: {name}: host-owned extension request response requires the Tokio runtime"
            ));
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(
                EXTENSION_UI_EDITOR_RESPONSE_DEADLINE,
                process.respond_to_extension_request(request_id, generation, outcome),
            )
            .await;
        });
    }

    /// Refuse one request without changing host state.
    fn refuse_host_request(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        failure: ExtensionRequestFailure,
        message: impl Into<String>,
    ) {
        self.queue_host_request_response(
            process,
            request_id,
            generation,
            ExtensionRequestOutcome::Failed(failure, message.into()),
        );
    }

    /// Fence one drained request on foreground resource ownership, negotiated
    /// feature, and payload bounds before it can touch host state.
    #[allow(clippy::too_many_arguments)]
    fn admit_host_request(
        &mut self,
        process: Option<ExtensionProcess>,
        name: &str,
        request_id: ExtensionRequestId,
        generation: u64,
        owner: Option<octet_agent::extension_process::ExtensionResourceOwner>,
        operation: HostRequestOperation,
        interactive: bool,
    ) {
        let Some(process) = process else {
            self.diagnostics.push(format!(
                "warning: {name}: host-owned extension request has no process"
            ));
            return;
        };
        let owner_is_foreground =
            host_request_owner_is_foreground(owner.as_ref(), self.resource_owner.as_deref());
        if !owner_is_foreground {
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::NotForegroundOwner,
                format!(
                    "{} is refused: the request owner is not the foreground session",
                    host_request_operation_name(&operation)
                ),
            );
            return;
        }
        let feature = host_request_feature(&operation);
        if !process.supports_feature(feature) {
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::UnsupportedFeature,
                format!("{feature} is not a negotiated extension feature"),
            );
            return;
        }
        if let Err((failure, message)) = validate_host_request(&operation) {
            self.refuse_host_request(process, request_id, generation, failure, message);
            return;
        }
        if !interactive {
            let message = match &operation {
                HostRequestOperation::Composer(_) => {
                    "no foreground composer is available in this host mode".to_owned()
                }
                _ => "no foreground session is available in this host mode".to_owned(),
            };
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::InvalidRequest,
                message,
            );
            return;
        }
        if self.pending_host_requests.len() + self.pending_session_requests.len()
            >= HOST_REQUEST_QUEUE_CAPACITY
        {
            self.refuse_host_request(
                process,
                request_id,
                generation,
                ExtensionRequestFailure::InvalidRequest,
                "the host extension request queue is full".to_owned(),
            );
            return;
        }
        let pending = PendingHostRequest {
            process,
            request_id,
            generation,
            operation,
        };
        if matches!(
            pending.operation,
            HostRequestOperation::SessionEntry(_)
                | HostRequestOperation::ActiveTools { .. }
                | HostRequestOperation::ContextSnapshot(ExtensionContextOperation::SystemPrompt)
        ) {
            self.pending_session_requests.push_back(pending);
        } else {
            self.pending_host_requests.push_back(pending);
        }
    }

    /// Maps one active-tool application result to the extension-facing outcome.
    ///
    /// Enforcement lives in the agent: only narrowing inside the host-policed
    /// surface is accepted, and unknown or policy-excluded names are refused with
    /// no state change. This keeps the wire vocabulary honest for either direction.
    fn active_tools_outcome(
        result: Result<(), octet_agent::AgentError>,
    ) -> ExtensionRequestOutcome {
        match result {
            Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the active tool set was refused: {error}"),
            ),
        }
    }

    /// Install one runtime shortcut binding for the foreground generation.
    fn register_dynamic_shortcut(
        &mut self,
        name: &str,
        process: ExtensionProcess,
        generation: u64,
        shortcut_id: String,
        key: &str,
        description: String,
    ) -> ExtensionRequestOutcome {
        let parsed = match dynamic_shortcut_binding(key) {
            Ok(parsed) => parsed,
            Err((outcome, diagnostic)) => {
                // A host binding always wins. The refusal is typed and the
                // diagnostic is visible in the extension status surface.
                self.diagnostics
                    .push(format!("warning: extension {name:?}: {diagnostic}"));
                return outcome;
            }
        };
        if self.shortcuts.iter().any(|existing| existing.key == parsed) {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is already bound to an extension shortcut"),
            );
        }
        if self
            .dynamic_shortcuts
            .iter()
            .any(|existing| existing.key == parsed)
        {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut key {key:?} is already registered"),
            );
        }
        if self
            .dynamic_shortcuts
            .iter()
            .any(|existing| existing.extension == name && existing.shortcut_id == shortcut_id)
        {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("shortcut id {shortcut_id:?} is already registered"),
            );
        }
        if self.dynamic_shortcuts.len() >= MAX_EXTENSION_SHORTCUTS {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!("at most {MAX_EXTENSION_SHORTCUTS} runtime shortcuts are supported"),
            );
        }
        self.dynamic_shortcuts.push(RegisteredDynamicShortcut {
            extension: name.to_owned(),
            shortcut_id,
            key: parsed,
            description,
            process,
            generation,
        });
        ExtensionRequestOutcome::Ok(serde_json::json!({}))
    }

    /// Successfully initialized live generations, for detecting which bindings
    /// a resource rebuild actually replaced rather than merely retained.
    pub(crate) fn running_generations(&self) -> BTreeMap<String, (String, u64)> {
        self.processes
            .iter()
            .filter(|process| process.is_running())
            .map(|process| {
                (
                    process.descriptor().manifest.name.clone(),
                    (
                        process.extension_instance_id().to_owned(),
                        process.health_snapshot().generation,
                    ),
                )
            })
            .collect()
    }

    /// Pending host requests are possible interruptions, not proven losses.
    pub(crate) fn pending_host_request_count(&self) -> usize {
        self.pending_host_requests.len()
    }

    pub(crate) fn discard_stale_host_requests(&mut self) -> Vec<String> {
        let mut discarded = Vec::new();
        self.pending_host_requests.retain(|pending| {
            if let Some(notice) = pending.discard_notice() {
                discarded.push(notice);
                false
            } else {
                true
            }
        });
        self.diagnostics.extend(discarded.iter().cloned());
        discarded
    }

    /// Answer the requests the live shell can resolve. Session-entry requests
    /// stay queued for the product loop that owns the session store.
    fn drain_host_requests_into_shell(&mut self, shell: &mut InteractiveShell) {
        while let Some(pending) = self.pending_host_requests.pop_front() {
            if let Some(notice) = pending.discard_notice() {
                self.diagnostics.push(notice.clone());
                shell.notice(notice);
                continue;
            }
            let name = pending.process.descriptor().manifest.name.clone();
            let outcome = match pending.operation {
                HostRequestOperation::Composer(operation) => match operation {
                    ExtensionComposerOperation::Get => ExtensionRequestOutcome::Ok(
                        serde_json::json!({ "text": shell.extension_editor_snapshot().text }),
                    ),
                    ExtensionComposerOperation::Set { text } => {
                        shell.extension_set_editor(text);
                        ExtensionRequestOutcome::Ok(serde_json::json!({}))
                    }
                    ExtensionComposerOperation::Insert { text } => {
                        shell.extension_paste_editor(text);
                        ExtensionRequestOutcome::Ok(serde_json::json!({}))
                    }
                },
                HostRequestOperation::Shortcut {
                    shortcut_id,
                    key,
                    description,
                } => self.register_dynamic_shortcut(
                    &name,
                    pending.process.clone(),
                    pending.generation,
                    shortcut_id,
                    &key,
                    description,
                ),
                HostRequestOperation::MessageInjection(injection) => match injection {
                    ExtensionMessageInjection::User { text } => {
                        if text.trim().is_empty() {
                            ExtensionRequestOutcome::Failed(
                                ExtensionRequestFailure::InvalidRequest,
                                "injected message text must not be empty".to_owned(),
                            )
                        } else {
                            // The queued follow-up is admitted by the owning run
                            // loop through the real user-turn path.
                            shell.queue_follow_up(ComposedInput::from_text(text));
                            ExtensionRequestOutcome::Ok(serde_json::json!({}))
                        }
                    }
                    ExtensionMessageInjection::Assistant { .. } => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::UnsupportedFeature,
                        "this host build does not inject assistant messages".to_owned(),
                    ),
                    ExtensionMessageInjection::System { .. } => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::UnsupportedFeature,
                        "this host build does not inject system messages".to_owned(),
                    ),
                },
                HostRequestOperation::SessionEntry(_) => {
                    // Unreachable: session-entry requests use their own queue.
                    continue;
                }
                HostRequestOperation::ActiveTools { .. } => ExtensionRequestOutcome::Failed(
                    ExtensionRequestFailure::UnsupportedFeature,
                    "this host build does not apply tools/set_active".to_owned(),
                ),
                HostRequestOperation::Terminal(operation) => self.apply_terminal_host_request(
                    shell,
                    pending.process.clone(),
                    pending.generation,
                    operation,
                ),
                HostRequestOperation::ContextSnapshot(operation) => {
                    self.apply_context_snapshot_in_shell(shell, operation)
                }
            };
            self.queue_host_request_response(
                pending.process,
                pending.request_id,
                pending.generation,
                outcome,
            );
        }
    }

    /// Answer one read-only foreground context snapshot the live shell can
    /// resolve. `SystemPrompt` is routed to the session owner and can never
    /// reach this path; the two session-context operations resolve against the
    /// cached host state and the shell's admitted follow-up queue.
    fn apply_context_snapshot_in_shell(
        &self,
        shell: &InteractiveShell,
        operation: ExtensionContextOperation,
    ) -> ExtensionRequestOutcome {
        match operation {
            ExtensionContextOperation::SessionManager => self.context_session_manager_outcome(),
            ExtensionContextOperation::PendingMessages => {
                Self::context_pending_messages_outcome(shell)
            }
            ExtensionContextOperation::SystemPrompt => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "the system prompt is resolved by the session owner".to_owned(),
            ),
        }
    }

    /// Compose the active session-manager snapshot from the cached host state
    /// and the workspace root. A missing session or workspace is refused rather
    /// than answered with a fabricated placeholder.
    fn context_session_manager_outcome(&self) -> ExtensionRequestOutcome {
        let Some(session_id) = self.session_id.clone() else {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "no foreground session is available".to_owned(),
            );
        };
        if self.workspace.as_os_str().is_empty() {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "no foreground workspace is available".to_owned(),
            );
        }
        let state = self
            .host_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let active_skills = state
            .active_skills
            .iter()
            .take(MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS)
            .map(|skill| ContextSkillSummary {
                id: skill.id.clone(),
                name: skill.name.clone(),
            })
            .collect();
        let reasoning = state
            .reasoning
            .as_ref()
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let result = ContextSessionManagerResult {
            session_id,
            name: state.session_name,
            model: state.model,
            reasoning,
            active_skills,
            cwd: self.workspace.to_string_lossy().into_owned(),
        };
        match serde_json::to_value(result) {
            Ok(value) => ExtensionRequestOutcome::Ok(value),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the session snapshot is not serializable: {error}"),
            ),
        }
    }

    /// Count the follow-up messages the foreground shell has queued but not yet
    /// admitted to the agent. This is exactly the queue
    /// `session/send_user_message` feeds, so the count is observed, never guessed.
    fn context_pending_messages_outcome(shell: &InteractiveShell) -> ExtensionRequestOutcome {
        let pending = u32::try_from(shell.queued_follow_up_len()).unwrap_or(u32::MAX);
        match serde_json::to_value(ContextPendingMessagesResult { pending }) {
            Ok(value) => ExtensionRequestOutcome::Ok(value),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the pending-message count is not serializable: {error}"),
            ),
        }
    }

    /// Compose the bounded system-prompt disclosure from the live agent. The
    /// composed prompt is refused rather than truncated when it exceeds the wire
    /// disclosure bound.
    fn context_system_prompt_outcome(agent: &Agent) -> ExtensionRequestOutcome {
        let result = ContextSystemPromptResult {
            text: agent.system_prompt().to_owned(),
        };
        if let Err(error) = result.validate() {
            return ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, error);
        }
        match serde_json::to_value(result) {
            Ok(value) => ExtensionRequestOutcome::Ok(value),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the system prompt snapshot is not serializable: {error}"),
            ),
        }
    }

    /// Answers one `session/append_entry` request against the durable
    /// foreground session. The extension manifest name is the entry namespace
    /// and the live process generation is the provenance attestation.
    ///
    /// The wire protocol admits 64 KiB of entry data, but the durable store
    /// retains at most [`MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES`] encoded
    /// envelope bytes, so the smaller effective cap is enforced here and named
    /// in the refusal. A residual `SessionError::Limit` after that pre-check is
    /// a malformed value (unusable namespace, control characters, excessive
    /// nesting), never a size overflow, so it maps to `invalid_request`.
    fn apply_extension_entry_append(
        session: &mut Session,
        namespace: &str,
        process_generation: u64,
        entry_type: &str,
        data: Value,
    ) -> ExtensionRequestOutcome {
        if entry_type.is_empty() || entry_type.chars().any(char::is_control) {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "entry_type must be a non-empty string without control characters".to_owned(),
            );
        }
        if entry_type.len() > MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!("entry_type exceeds {MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES} bytes"),
            );
        }
        let envelope = serde_json::json!({ "entry_type": entry_type, "data": &data });
        let encoded = match serde_json::to_vec(&envelope) {
            Ok(encoded) => encoded,
            Err(error) => {
                return ExtensionRequestOutcome::Failed(
                    ExtensionRequestFailure::InvalidRequest,
                    format!("entry data is not serializable: {error}"),
                );
            }
        };
        if encoded.len() > MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!(
                    "entry data exceeds {MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES} stored bytes"
                ),
            );
        }
        match session.append_extension_entry(namespace, Some(process_generation), entry_type, data)
        {
            Ok(entry_id) => {
                ExtensionRequestOutcome::Ok(serde_json::json!({ "entry_id": entry_id.0 }))
            }
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the extension session entry was refused: {error}"),
            ),
        }
    }

    /// Answers one `session/set_label` request against the durable foreground
    /// session. An unknown entry id is refused with `invalid_request` and the
    /// store's refusal path leaves the session file byte-identical; control
    /// characters reach the store and are reported as a malformed request.
    fn apply_extension_entry_label(
        session: &mut Session,
        entry_id: &str,
        label: &str,
    ) -> ExtensionRequestOutcome {
        if label.len() > MAX_EXTENSION_SESSION_LABEL_BYTES {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::BoundsExceeded,
                format!("entry label exceeds {MAX_EXTENSION_SESSION_LABEL_BYTES} bytes"),
            );
        }
        match session.set_entry_label(&EntryId(entry_id.to_owned()), label) {
            Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
            Err(SessionError::UnknownEntry(id)) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("no session entry {id:?} exists in the foreground session"),
            ),
            Err(SessionError::Limit(message)) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the extension entry label was refused: {message}"),
            ),
            Err(error) => ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("the extension entry label could not be stored: {error}"),
            ),
        }
    }

    /// Resolve admitted session-entry and active-tool requests against the
    /// durable foreground session and the live session store. Returns true when
    /// durable session presentation changed; extension entries, their labels,
    /// and the active tool set are not part of the coding-agent shell
    /// presentation.
    ///
    /// Both operations need the live [`Agent`] (the durable session for entries
    /// and labels, the host-policed tool surface for `tools/set_active`), so the
    /// caller hands over the foreground agent instead of a bare session.
    pub fn apply_session_host_requests(
        &mut self,
        agent: &mut Agent,
        sessions: &SessionStore,
    ) -> bool {
        let mut changed = false;
        while let Some(pending) = self.pending_session_requests.pop_front() {
            if !pending.process.is_running()
                || pending.process.health_snapshot().generation != pending.generation
            {
                self.diagnostics.push(format!(
                    "warning: {}: discarded host-owned session request from stale generation {}",
                    pending.process.descriptor().manifest.name,
                    pending.generation
                ));
                continue;
            }
            if let HostRequestOperation::ActiveTools { names } = pending.operation {
                // Narrowing only: the agent refuses unknown or policy-excluded
                // names and can never widen the host-policed tool surface.
                let outcome =
                    Self::active_tools_outcome(agent.set_active_tool_names(Some(
                        names.iter().cloned().collect::<BTreeSet<_>>(),
                    )));
                self.queue_host_request_response(
                    pending.process,
                    pending.request_id,
                    pending.generation,
                    outcome,
                );
                continue;
            }
            if let HostRequestOperation::ContextSnapshot(operation) = pending.operation {
                let outcome = match operation {
                    ExtensionContextOperation::SystemPrompt => {
                        Self::context_system_prompt_outcome(agent)
                    }
                    // The two session-context reads resolve against the shell
                    // drain, never this agent-owning loop.
                    ExtensionContextOperation::SessionManager
                    | ExtensionContextOperation::PendingMessages => continue,
                };
                self.queue_host_request_response(
                    pending.process,
                    pending.request_id,
                    pending.generation,
                    outcome,
                );
                continue;
            }
            let session = agent.session_mut();
            // The extension's own manifest name is the durable metadata
            // namespace; the admitting generation is the provenance value.
            let namespace = pending.process.descriptor().manifest.name.clone();
            let HostRequestOperation::SessionEntry(operation) = pending.operation else {
                continue;
            };
            let outcome = match operation {
                ExtensionSessionEntryOperation::SetName { name } => match self.session_id.clone() {
                    None => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::InvalidRequest,
                        "no foreground session is available".to_owned(),
                    ),
                    Some(session_id) => match sessions.rename(&session_id, &name) {
                        Ok(_) => {
                            if pending
                                .process
                                .supports_feature(EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2)
                            {
                                if let Err(error) = pending.process.notify_session_info_changed() {
                                    self.diagnostics.push(format!(
                                        "warning: {}: session info notification failed: {error}",
                                        pending.process.descriptor().manifest.name
                                    ));
                                }
                            }
                            changed = true;
                            ExtensionRequestOutcome::Ok(serde_json::json!({}))
                        }
                        Err(error) => ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::InvalidRequest,
                            format!("the session name could not be stored: {error}"),
                        ),
                    },
                },
                ExtensionSessionEntryOperation::Append { entry_type, data } => {
                    Self::apply_extension_entry_append(
                        session,
                        &namespace,
                        pending.generation,
                        &entry_type,
                        data,
                    )
                }
                ExtensionSessionEntryOperation::SetLabel { entry_id, label } => {
                    Self::apply_extension_entry_label(session, &entry_id, &label)
                }
            };
            self.queue_host_request_response(
                pending.process,
                pending.request_id,
                pending.generation,
                outcome,
            );
        }
        changed
    }

    fn drain_editor_requests_into_shell(&mut self, shell: &mut InteractiveShell) {
        while let Some(pending) = self.pending_editor_requests.pop_front() {
            if !pending.process.is_running()
                || pending.process.health_snapshot().generation != pending.generation
            {
                self.diagnostics.push(format!(
                    "warning: {}: discarded host-owned editor request from stale generation {}",
                    pending.process.descriptor().manifest.name,
                    pending.generation
                ));
                continue;
            }
            let snapshot = match pending.request {
                ExtensionEditorRequest::Get => shell.extension_editor_snapshot(),
                ExtensionEditorRequest::Set { text } => shell.extension_set_editor(text),
                ExtensionEditorRequest::Paste { text } => shell.extension_paste_editor(text),
                ExtensionEditorRequest::Focus => shell.extension_focus_editor(),
            };
            self.queue_editor_response(
                pending.process,
                pending.request_id,
                pending.generation,
                ExtensionEditorResponse {
                    text: snapshot.text,
                    revision: snapshot.revision,
                    focused: snapshot.focused,
                },
            );
        }
    }

    /// Answers one `terminal/acquire` or `terminal/release` against the live
    /// foreground shell. Every caller receives exactly one typed answer: the
    /// minted grant, an empty release body, or a typed refusal.
    ///
    /// The terminal stays host-owned. The host leaves raw mode and parks its
    /// input before it answers an acquire, and re-enters only after a release
    /// it accepted; a refused caller never changes host terminal state.
    fn apply_terminal_host_request(
        &mut self,
        shell: &mut InteractiveShell,
        process: ExtensionProcess,
        generation: u64,
        operation: ExtensionTerminalOperation,
    ) -> ExtensionRequestOutcome {
        // Re-fence the generation even though the drain already discarded stale
        // generations: a reload between admission and drain must never move the
        // tty, and the child must never act on a grant it will not be told about.
        if !process.is_running() || process.health_snapshot().generation != generation {
            return ExtensionRequestOutcome::Failed(
                ExtensionRequestFailure::InvalidRequest,
                "the terminal request belongs to a stale process generation".to_owned(),
            );
        }
        let holder = TerminalHolder {
            owner: self.resource_owner.clone(),
            instance_id: process.extension_instance_id().to_owned(),
            generation,
            name: process.descriptor().manifest.name.clone(),
        };
        match operation {
            ExtensionTerminalOperation::Acquire => {
                // Read the size the host is leaving behind, then hand the tty
                // over: the answer is the last thing the host does here.
                let (columns, rows) = shell.terminal_dimensions();
                let granted = match self.terminal_arbiter.acquire(holder, columns, rows) {
                    Ok(granted) => granted,
                    Err((failure, message)) => {
                        return ExtensionRequestOutcome::Failed(failure, message);
                    }
                };
                shell.cede_terminal_input();
                shell.suspend();
                ExtensionRequestOutcome::Ok(serde_json::json!({
                    "grant_id": granted.grant_id,
                    "columns": granted.columns,
                    "rows": granted.rows,
                }))
            }
            ExtensionTerminalOperation::Release => {
                if let Err((failure, message)) = self.terminal_arbiter.release(&holder) {
                    return ExtensionRequestOutcome::Failed(failure, message);
                }
                // The holder's own release is not a revocation: no
                // `terminal/grant-lost` is fired for it.
                shell.release_terminal_input();
                match shell.resume() {
                    Ok(()) => ExtensionRequestOutcome::Ok(serde_json::json!({})),
                    Err(error) => ExtensionRequestOutcome::Failed(
                        ExtensionRequestFailure::InvalidRequest,
                        format!("the host could not re-enter its terminal: {error}"),
                    ),
                }
            }
        }
    }

    /// Restore the host terminal when the live grant stopped being valid: the
    /// holder died or restarted, or the foreground session moved on.
    ///
    /// The host revokes without waiting on the previous holder, so a crash
    /// mid-grant can never wedge the TUI. A holder that outlived its own grant
    /// is told through `terminal/grant-lost`; the holder's own release never
    /// reaches here.
    pub fn reconcile_terminal_grant_for_shell(&mut self, shell: &mut InteractiveShell) {
        let owner = self.resource_owner.clone();
        let live: BTreeSet<(String, u64)> = self
            .processes
            .iter()
            .filter(|process| process.is_running())
            .map(|process| {
                (
                    process.extension_instance_id().to_owned(),
                    process.health_snapshot().generation,
                )
            })
            .collect();
        let Some(revoked) = self.terminal_arbiter.revoke_if(|holder| {
            holder.owner == owner && live.contains(&(holder.instance_id.clone(), holder.generation))
        }) else {
            return;
        };
        let holder_still_live = live.contains(&(
            revoked.holder.instance_id.clone(),
            revoked.holder.generation,
        ));
        let reason = if holder_still_live {
            "the foreground session changed while the terminal was ceded"
        } else {
            "the foreground terminal grant holder is no longer running"
        };
        self.restore_revoked_terminal_grant(shell, revoked, reason);
    }

    /// Revoke before dropping or replacing this binding. Reconciliation alone
    /// cannot find the old grant after a replacement App owns a fresh arbiter.
    pub fn revoke_terminal_grant_for_shell(&mut self, shell: &mut InteractiveShell, reason: &str) {
        if let Some(revoked) = self.terminal_arbiter.revoke_if(|_| false) {
            self.restore_revoked_terminal_grant(shell, revoked, reason);
        }
    }

    fn restore_revoked_terminal_grant(
        &mut self,
        shell: &mut InteractiveShell,
        revoked: ActiveTerminalGrant,
        reason: &str,
    ) {
        shell.release_terminal_input();
        if let Err(error) = shell.resume() {
            self.diagnostics.push(format!(
                "warning: {}: the host could not re-enter its terminal after the grant was revoked: {error}",
                revoked.holder.name
            ));
        }
        // Only a holder that is still alive can hear the revocation; a dead
        // process is dropped instead of surfaced as a failed notification.
        if let Some(process) = self
            .processes
            .iter()
            .find(|process| {
                process.is_running()
                    && process.extension_instance_id() == revoked.holder.instance_id
                    && process.health_snapshot().generation == revoked.holder.generation
            })
            .cloned()
        {
            if let Err(error) = process.notify_terminal_grant_lost(reason) {
                self.diagnostics.push(format!(
                    "warning: {}: terminal/grant-lost could not be delivered: {error}",
                    revoked.holder.name
                ));
            }
        }
        self.diagnostics.push(format!(
            "{}: the foreground terminal grant was revoked ({reason})",
            revoked.holder.name
        ));
    }

    /// Whether an extension currently holds the ceded foreground terminal.
    pub fn terminal_grant_is_active(&self) -> bool {
        self.terminal_arbiter.active().is_some()
    }

    /// Drain extension events while an interactive shell owns the editor. This
    /// is deliberately separate from the generic event drain so headless hosts
    /// never accidentally grant an editor lease.
    pub fn drain_events_for_shell(&mut self, shell: &mut InteractiveShell) -> Vec<String> {
        let messages = self.drain_events_inner(true);
        self.drain_editor_requests_into_shell(shell);
        self.drain_host_requests_into_shell(shell);
        self.reconcile_terminal_grant_for_shell(shell);
        messages
    }

    /// Drain a fixed amount of extension work without letting a continuously
    /// ready process monopolize the input/render task. The start receiver
    /// rotates between calls and each receiver has a smaller per-call quota.
    pub fn drain_events(&mut self) -> Vec<String> {
        self.drain_events_inner(false)
    }

    fn drain_events_inner(&mut self, interactive: bool) -> Vec<String> {
        self.poll_telemetry();
        self.schedule_confirmation_denials();
        self.schedule_input_cancellations();
        let receiver_count = self.receivers.len();
        if receiver_count == 0 {
            return Vec::new();
        }

        let start = self.event_drain_cursor % receiver_count;
        let mut remaining = EVENT_DRAIN_BUDGET;
        let mut visited = 0usize;
        let mut messages = Vec::new();
        while visited < receiver_count && remaining > 0 {
            let index = (start + visited) % receiver_count;
            let name = self
                .processes
                .get(index)
                .map(|process| process.descriptor().manifest.name.clone())
                .unwrap_or_else(|| "extension".to_owned());
            let process = self.processes.get(index).cloned();
            let mut receiver_budget = EVENT_DRAIN_PER_RECEIVER_BUDGET.min(remaining);
            while receiver_budget > 0 {
                let event = self.receivers[index].try_recv();
                match event {
                    Ok(ExtensionEvent::Notification { notification }) => {
                        messages.push(format_notification(&name, &notification));
                    }
                    Ok(ExtensionEvent::ContextContributed { contribution }) => {
                        admit_context(
                            &mut self.pending_context,
                            &mut self.diagnostics,
                            &name,
                            contribution,
                        );
                    }
                    Ok(ExtensionEvent::StatusContributed { contribution }) => {
                        // Header and footer surfaces become bounded chrome; any
                        // other surface stays protocol-only. The source process
                        // generation is the freshness fence.
                        match process.as_ref() {
                            Some(process) => {
                                self.apply_status_surface(name.clone(), process, contribution);
                            }
                            None => self.diagnostics.push(format!(
                                "warning: {name}: status surface source process is unavailable"
                            )),
                        }
                    }
                    Ok(ExtensionEvent::UiContributed {
                        generation,
                        contribution,
                    }) => {
                        let Some(process) = process.as_ref() else {
                            self.diagnostics.push(format!(
                                "warning: {name}: semantic UI source process is unavailable"
                            ));
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        };
                        if let Err(error) = self.apply_semantic_ui_contribution(
                            name.clone(),
                            process,
                            generation,
                            contribution,
                        ) {
                            self.diagnostics.push(format!("warning: {name}: {error}"));
                        }
                    }
                    Ok(ExtensionEvent::EditorRequested {
                        request_id,
                        generation,
                        request,
                    }) => {
                        let Some(process) = process.clone() else {
                            self.diagnostics.push(format!(
                                "warning: {name}: host-owned editor request has no process"
                            ));
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        };
                        if interactive && self.pending_editor_requests.len() < EVENT_DRAIN_BUDGET {
                            self.pending_editor_requests
                                .push_back(PendingEditorRequest {
                                    process,
                                    request_id,
                                    generation,
                                    request,
                                });
                        } else {
                            self.queue_editor_response(
                                process,
                                request_id,
                                generation,
                                ExtensionEditorResponse {
                                    text: String::new(),
                                    revision: 0,
                                    focused: false,
                                },
                            );
                            if interactive {
                                self.diagnostics.push(format!(
                                    "warning: {name}: host-owned editor request queue is full"
                                ));
                            }
                        }
                    }
                    Ok(ExtensionEvent::ComposerRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::Composer(operation),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::SessionEntryRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::SessionEntry(operation),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::MessageInjectionRequested {
                        request_id,
                        generation,
                        owner,
                        injection,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::MessageInjection(injection),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ShortcutRequested {
                        request_id,
                        generation,
                        owner,
                        shortcut_id,
                        key,
                        description,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::Shortcut {
                                shortcut_id,
                                key,
                                description,
                            },
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ActiveToolsRequested {
                        request_id,
                        generation,
                        owner,
                        names,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::ActiveTools { names },
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::TerminalRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::Terminal(operation),
                            interactive,
                        );
                    }
                    Ok(ExtensionEvent::ContextSnapshotRequested {
                        request_id,
                        generation,
                        owner,
                        operation,
                    }) => {
                        self.admit_host_request(
                            process.clone(),
                            &name,
                            request_id,
                            generation,
                            owner,
                            HostRequestOperation::ContextSnapshot(operation),
                            interactive,
                        );
                    }
                    // Contract A's product half is still owed: there is no
                    // `HostRequestOperation` for a model view yet, so this
                    // request is not answered here. The Pi bridge does not issue
                    // `context/model` until that lands, so no extension is left
                    // waiting today; the arm keeps the drain exhaustive, matching
                    // the agent crate's own `ModelViewRequested` arm.
                    Ok(ExtensionEvent::ModelViewRequested { .. }) => {}
                    Ok(ExtensionEvent::AutocompleteRegistered {
                        request_id,
                        generation,
                        registration: _,
                    }) => {
                        let Some(process) = process.clone() else {
                            self.diagnostics.push(format!(
                                "warning: {name}: autocomplete registration has no process"
                            ));
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        };
                        let accepted = interactive
                            && process.is_running()
                            && process.health_snapshot().generation == generation;
                        if accepted {
                            self.autocomplete_registrations.insert(
                                name.clone(),
                                RegisteredAutocomplete {
                                    extension_instance_id: process
                                        .extension_instance_id()
                                        .to_owned(),
                                    process: process.clone(),
                                    generation,
                                },
                            );
                        }
                        self.queue_autocomplete_registration_response(
                            process, request_id, generation, accepted,
                        );
                    }
                    Ok(ExtensionEvent::PresentationUpdated {
                        generation,
                        resource_owner,
                        snapshot,
                    }) => {
                        let published_owner = match admit_presentation_owner(
                            resource_owner,
                            self.resource_owner.as_deref(),
                        ) {
                            Ok(owner) => owner,
                            Err(error) => {
                                self.diagnostics.push(format!("warning: {name}: {error}"));
                                remaining -= 1;
                                receiver_budget -= 1;
                                continue;
                            }
                        };
                        let active_generation = process
                            .as_ref()
                            .map(ExtensionProcess::health_snapshot)
                            .map(|health| health.generation);
                        let extension_instance_id = process
                            .as_ref()
                            .expect("extension event receivers align with processes")
                            .extension_instance_id()
                            .to_owned();
                        if let Err(error) = reduce_presentation_update(
                            &mut self.presentations,
                            name.clone(),
                            active_generation,
                            extension_instance_id,
                            published_owner,
                            generation,
                            snapshot,
                        ) {
                            self.diagnostics.push(format!("warning: {name}: {error}"));
                        }
                    }
                    Ok(ExtensionEvent::Diagnostic { message }) => {
                        self.diagnostics.push(format!("warning: {name}: {message}"));
                    }
                    Ok(ExtensionEvent::ConfirmationRequested {
                        request_id,
                        generation,
                        request,
                        ..
                    }) => {
                        if process.as_ref().is_some_and(|process| {
                            process.confirmation_answered(&request_id, generation)
                        }) {
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        }
                        // A request outside a frontend-controlled confirmation
                        // boundary is denied through a bounded tracked queue.
                        messages.push(format!(
                            "[{name}] confirmation denied (no active confirmation UI): {}",
                            request.prompt
                        ));
                        if let Some(process) = process.clone() {
                            self.queue_confirmation_denial(PendingConfirmationDenial {
                                process,
                                request_id,
                                generation,
                            });
                        } else {
                            self.diagnostics.push(format!(
                                "warning: {name}: confirmation could not be denied because its process is unavailable"
                            ));
                        }
                    }
                    // The policy supervisor answers independently of frontend
                    // event drains; observing an intent is not a denial.
                    Ok(ExtensionEvent::PolicyEvaluationRequested { .. }) => {}
                    Ok(ExtensionEvent::InputRequested {
                        request_id,
                        generation,
                        request,
                        ..
                    }) => {
                        if process
                            .as_ref()
                            .is_some_and(|process| process.input_answered(&request_id, generation))
                        {
                            remaining -= 1;
                            receiver_budget -= 1;
                            continue;
                        }
                        messages.push(format!(
                            "[{name}] input cancelled (no active input owner): {}",
                            request.prompt
                        ));
                        if let Some(process) = process.clone() {
                            self.queue_input_cancellation(PendingInputCancellation {
                                process,
                                request_id,
                                generation,
                            });
                        } else {
                            self.diagnostics.push(format!(
                                "warning: {name}: input could not be cancelled because its process is unavailable"
                            ));
                        }
                    }
                    Err(broadcast::error::TryRecvError::Empty)
                    | Err(broadcast::error::TryRecvError::Closed) => break,
                    Err(broadcast::error::TryRecvError::Lagged(count)) => {
                        messages.push(format!(
                            "[{name}] dropped {count} extension events because the consumer lagged"
                        ));
                    }
                }
                remaining -= 1;
                receiver_budget -= 1;
            }
            visited += 1;
        }
        self.event_drain_cursor = (start + visited.max(1)) % receiver_count;
        let active_owner = self.resource_owner.as_deref();
        self.presentations.retain(|name, view| {
            (view.resource_owner.is_none() || view.resource_owner.as_deref() == active_owner)
                && self.processes.iter().any(|process| {
                    process.descriptor().manifest.name == *name
                        && process.is_running()
                        && process.health_snapshot().generation == view.generation
                })
        });
        self.prune_semantic_ui();
        self.dynamic_shortcuts.retain(|shortcut| {
            self.processes.iter().any(|process| {
                process.extension_instance_id() == shortcut.process.extension_instance_id()
                    && process.is_running()
                    && process.health_snapshot().generation == shortcut.generation
            })
        });
        self.schedule_confirmation_denials();
        self.schedule_input_cancellations();
        messages
    }
}

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

async fn notify_lifecycle_all(
    processes: &[ExtensionProcess],
    event: ExtensionLifecycleEvent,
) -> Vec<String> {
    futures_util::future::join_all(processes.iter().map(|process| {
        let process = process.clone();
        let event = event.clone();
        async move {
            match tokio::time::timeout(
                LIFECYCLE_NOTIFY_DEADLINE,
                process.notify_lifecycle(&event),
            )
            .await
            {
                Err(_) => Some(format!(
                    "warning: extension {:?} lifecycle notification exceeded {LIFECYCLE_NOTIFY_DEADLINE:?}",
                    process.descriptor().manifest.name
                )),
                Ok(Err(error)) => Some(format!(
                    "warning: extension {:?} lifecycle notification failed: {error}",
                    process.descriptor().manifest.name
                )),
                Ok(Ok(())) => None,
            }
        }
    }))
    .await
    .into_iter()
    .flatten()
    .collect()
}

async fn start_session_hooks_all(
    processes: &[ExtensionProcess],
    resource_owner: &str,
) -> Vec<String> {
    futures_util::future::join_all(
        processes
            .iter()
            .filter(|process| process.declares_session_hooks())
            .map(|process| {
                let process = process.clone();
                let resource_owner = resource_owner.to_owned();
                async move {
                    match tokio::time::timeout(
                        LIFECYCLE_NOTIFY_DEADLINE,
                        process.start_session_hook_binding(resource_owner),
                    )
                    .await
                    {
                        Ok(Ok(())) => None,
                        Err(_) => Some(format!(
                            "warning: extension {:?} session_start hook exceeded {LIFECYCLE_NOTIFY_DEADLINE:?}",
                            process.descriptor().manifest.name
                        )),
                        Ok(Err(_)) => Some(format!(
                            "warning: extension {:?} session_start hook failed",
                            process.descriptor().manifest.name
                        )),
                    }
                }
            }),
    )
    .await
    .into_iter()
    .flatten()
    .collect()
}

async fn settle_session_hooks_all(
    processes: &[ExtensionProcess],
    resource_owner: &str,
    outcome: ExtensionLifecycleOutcome,
) -> Vec<String> {
    futures_util::future::join_all(
        processes
            .iter()
            .filter(|process| process.declares_session_hooks())
            .map(|process| {
                let process = process.clone();
                let resource_owner = resource_owner.to_owned();
                async move {
                    match process
                        .settle_session_hook_binding(&resource_owner, outcome)
                        .await
                    {
                        Ok(()) => None,
                        Err(_) => Some(format!(
                            "warning: extension {:?} session_end hook failed",
                            process.descriptor().manifest.name
                        )),
                    }
                }
            }),
    )
    .await
    .into_iter()
    .flatten()
    .collect()
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn clip_lifecycle_reason(reason: &str, limit: usize) -> String {
    if reason.len() <= limit {
        return reason.to_owned();
    }
    let marker = "[… truncated …]";
    let mut end = limit.saturating_sub(marker.len());
    while !reason.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{marker}", &reason[..end])
}

pub struct ExtensionPromptComposition {
    pub system: String,
    pub prompt: String,
    pub notifications: Vec<String>,
    pub pending_context_count: usize,
}

pub fn assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn latest_assistant_text(session: &Session) -> String {
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        if let octet_agent::EntryValue::Message(Message::Assistant(message)) = &entry.value {
            return assistant_text(message);
        }
        cursor = entry.parent.clone();
    }
    String::new()
}

fn context_contribution_bytes(contribution: &ContextContribution) -> Result<usize, String> {
    if contribution.label.len() > MAX_CONTEXT_LABEL_BYTES {
        return Err(format!(
            "label exceeds the {MAX_CONTEXT_LABEL_BYTES} byte limit"
        ));
    }
    if contribution.content.len() > MAX_CONTEXT_CONTRIBUTION_BYTES {
        return Err(format!(
            "content exceeds the {MAX_CONTEXT_CONTRIBUTION_BYTES} byte limit"
        ));
    }
    if contribution.label.contains('\0') || contribution.content.contains('\0') {
        return Err("label or content contains NUL".into());
    }
    let quoted_label = format!("{:?}", contribution.label);
    Ok("<octet-extension-context label="
        .len()
        .saturating_add(quoted_label.len())
        .saturating_add(">\n".len())
        .saturating_add(contribution.content.len())
        .saturating_add("\n</octet-extension-context>".len())
        // `join_around` separates every non-empty block from its neighbor.
        .saturating_add("\n\n".len()))
}

fn compose_context(
    base_system: &str,
    prompt: String,
    contributions: Vec<ContextContribution>,
) -> anyhow::Result<(String, String)> {
    if contributions.len() > MAX_PENDING_CONTEXT_ITEMS {
        anyhow::bail!(
            "extension context exceeds the {} contribution limit",
            MAX_PENDING_CONTEXT_ITEMS
        );
    }
    let mut total = 0usize;
    let mut system_prefix = Vec::new();
    let mut system_suffix = Vec::new();
    let mut prompt_prefix = Vec::new();
    let mut prompt_suffix = Vec::new();
    for contribution in contributions {
        let contribution_bytes = context_contribution_bytes(&contribution).map_err(|error| {
            anyhow::anyhow!("extension context {:?}: {error}", contribution.label)
        })?;
        total = total.saturating_add(contribution_bytes);
        if total > MAX_EXTENSION_CONTEXT_BYTES {
            anyhow::bail!(
                "extension context exceeds the {} byte aggregate limit",
                MAX_EXTENSION_CONTEXT_BYTES
            );
        }
        let block = format!(
            "<octet-extension-context label={:?}>\n{}\n</octet-extension-context>",
            contribution.label, contribution.content
        );
        match contribution.placement {
            ContextPlacement::SystemPrefix => system_prefix.push(block),
            ContextPlacement::SystemSuffix => system_suffix.push(block),
            ContextPlacement::PromptPrefix => prompt_prefix.push(block),
            ContextPlacement::PromptSuffix => prompt_suffix.push(block),
        }
    }
    let system = join_around(system_prefix, base_system.to_owned(), system_suffix);
    let prompt = join_around(prompt_prefix, prompt, prompt_suffix);
    Ok((system, prompt))
}

fn join_around(prefix: Vec<String>, center: String, suffix: Vec<String>) -> String {
    prefix
        .into_iter()
        .chain(std::iter::once(center))
        .chain(suffix)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_presentation_views(views: &[ExtensionPresentationView]) -> String {
    let mut lines = Vec::new();
    for view in views {
        lines.push(format!(
            "[{}] generation {} · revision {}",
            view.extension, view.generation, view.snapshot.revision
        ));
        if let Some(status) = &view.snapshot.status {
            let state = format!("{:?}", status.state).to_lowercase();
            lines.push(format!("  status: {state} · {}", status.label));
            if let Some(detail) = &status.detail {
                lines.push(format!("    {detail}"));
            }
        }
        for activity in &view.snapshot.activities {
            let state = format!("{:?}", activity.state).to_lowercase();
            let provenance = activity
                .provenance
                .as_deref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            lines.push(format!(
                "  activity {}: {state} · {}{provenance}",
                activity.kind, activity.summary
            ));
            lines.extend(
                activity
                    .references
                    .iter()
                    .map(|reference| format_presentation_reference("    ", reference)),
            );
        }
        if let Some(collection) = &view.snapshot.collection {
            lines.push(format!("  {}:", collection.title));
            let parents = collection
                .nodes
                .iter()
                .map(|node| (node.id.as_str(), node.parent_id.as_deref()))
                .collect::<BTreeMap<_, _>>();
            for node in &collection.nodes {
                let mut depth = 0usize;
                let mut parent = node.parent_id.as_deref();
                while let Some(id) = parent {
                    depth = depth.saturating_add(1);
                    parent = parents.get(id).copied().flatten();
                }
                let state = format!("{:?}", node.state).to_lowercase();
                let secondary = node
                    .secondary
                    .as_deref()
                    .map(|value| format!(" · {value}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "    {}- {} · {state}{secondary}",
                    "  ".repeat(depth),
                    node.label
                ));
                lines.extend(
                    node.references
                        .iter()
                        .map(|reference| format_presentation_reference("      ", reference)),
                );
            }
            if let Some(detail) = &collection.detail {
                lines.push(format!("  detail: {}", detail.title));
                lines.extend(detail.body.lines().map(|line| format!("    {line}")));
                lines.extend(
                    detail
                        .references
                        .iter()
                        .map(|reference| format_presentation_reference("    ", reference)),
                );
            }
        }
        for action in &view.snapshot.actions {
            lines.push(format!(
                "  action {}: /{}{}{}",
                action.label,
                action.command,
                if action.arguments.is_empty() { "" } else { " " },
                action.arguments.join(" ")
            ));
        }
    }
    lines.join("\n")
}

fn format_presentation_reference(
    indent: &str,
    reference: &octet_agent::ExtensionPresentationReference,
) -> String {
    let kind = match reference.kind {
        octet_agent::ExtensionPresentationReferenceKind::Session => "session",
        octet_agent::ExtensionPresentationReferenceKind::Artifact => "artifact",
        octet_agent::ExtensionPresentationReferenceKind::Resource => "resource",
        octet_agent::ExtensionPresentationReferenceKind::Url => "source",
    };
    let label = reference
        .label
        .as_deref()
        .map(|label| format!("{label} · "))
        .unwrap_or_default();
    let value = format!("{indent}{kind}: {label}{}", reference.id);
    if reference.kind == octet_agent::ExtensionPresentationReferenceKind::Session {
        format!(
            "{value}\n{indent}  inspect: /extensions inspect {}",
            reference.id
        )
    } else {
        value
    }
}

fn format_notification(
    extension: &str,
    notification: &octet_agent::extension_process::ExtensionNotification,
) -> String {
    let title = notification
        .title
        .as_deref()
        .map(|title| format!(" {title}:"))
        .unwrap_or_default();
    format!(
        "[{extension} {:?}]{title} {}",
        notification.level, notification.message
    )
}

fn extension_execution_context(
    process: &ExtensionProcess,
    resource_owner: Option<&str>,
) -> octet_agent::extension_process::ExtensionExecutionContext {
    resource_owner.map_or_else(
        || process.current_context(),
        |owner| process.current_context_for_resource_owner(owner.to_owned()),
    )
}

fn host_state(
    session: &Session,
    model: &Model,
    reasoning: &ReasoningConfig,
    sessions: &SessionStore,
) -> ExtensionHostState {
    let session_id = session
        .path()
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_owned);
    let session_name = session_id
        .as_deref()
        .and_then(|id| sessions.load_metadata(id).ok())
        .and_then(|metadata| metadata.name);
    let active_skills = session
        .head()
        .and_then(|head| session.resolve_active_skills(&head).ok())
        .map(|state| {
            state
                .active_skills
                .into_iter()
                .map(
                    |skill| octet_agent::extension_process::ExtensionActiveSkill {
                        id: skill.descriptor.id,
                        name: skill.descriptor.name,
                        version: skill.descriptor.version,
                    },
                )
                .collect()
        })
        .unwrap_or_default();
    ExtensionHostState {
        session_id,
        session_name,
        model: Some(model.spec.id.0.clone()),
        model_view: extension_model_view(model),
        reasoning: Some(serde_json::Value::String(format!("{reasoning:?}"))),
        active_skills,
    }
}

/// Pi's wire-API name for one octet protocol.
///
/// Every octet protocol has an exact Pi `KnownApi` spelling, so this projection
/// never invents a name.
fn pi_api_name(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "openai-completions",
        Protocol::OpenAiResponses => "openai-responses",
        Protocol::AnthropicMessages => "anthropic-messages",
        Protocol::BedrockConverse => "bedrock-converse-stream",
        Protocol::GoogleGenerativeAi => "google-generative-ai",
        Protocol::MistralConversations => "mistral-conversations",
        Protocol::PiMessages => "pi-messages",
    }
}

/// The Pi provider identity that owns a model's route.
///
/// Pi reports the provider rather than the wire route, so a multi-route
/// declaration (`opencode-anthropic`) reports its declaring provider. Providers
/// octet registers itself namespace their model id as `provider/model`, and an
/// endpoint with no declaration falls back to the endpoint identity octet
/// already knows.
fn pi_provider_id(model: &Model) -> String {
    let endpoint = model.endpoint.id.0.as_str();
    if let Some(declaration) =
        crate::providers::ALL_PROVIDER_DECLARATIONS
            .iter()
            .find(|declaration| {
                declaration
                    .routes
                    .iter()
                    .any(|route| route.endpoint_id == endpoint)
            })
    {
        return declaration.id.to_owned();
    }
    match model.spec.id.0.split_once('/') {
        Some((provider, _)) if !provider.is_empty() => provider.to_owned(),
        _ => endpoint.to_owned(),
    }
}

/// Whether one model-view field fits the bounded wire width.
fn model_field_fits(value: &str) -> bool {
    value.len() <= octet_agent::extension_process::MAX_EXTENSION_MODEL_FIELD_BYTES
}

/// Project one resolved model into Pi's `Model` shape.
///
/// Only fields octet can state truthfully are projected. The endpoint base URL
/// and credentials stay host-owned, so `baseUrl` is absent rather than
/// fabricated: a Pi extension observes `undefined`, never a URL octet did not
/// disclose. A model whose identifier, name, or provider exceeds the bounded
/// field width yields no view at all, so an extension never receives a
/// truncated identifier.
fn extension_model_view(
    model: &Model,
) -> Option<octet_agent::extension_process::ExtensionModelView> {
    let spec = &model.spec;
    let provider = pi_provider_id(model);
    let name = spec
        .display_name
        .clone()
        .or_else(|| octet_ai::model_metadata::model_display_name(&spec.id.0).map(str::to_owned))
        .unwrap_or_else(|| spec.api_name.clone());
    if !model_field_fits(&spec.id.0) || !model_field_fits(&provider) || !model_field_fits(&name) {
        return None;
    }
    let mut input = vec!["text".to_owned()];
    if spec
        .capabilities
        .input_modalities
        .contains(octet_ai::Modality::Image)
    {
        input.push("image".to_owned());
    }
    Some(octet_agent::extension_process::ExtensionModelView {
        id: spec.id.0.clone(),
        name: Some(name),
        api: pi_api_name(&spec.protocol).to_owned(),
        provider,
        reasoning: spec.capabilities.reasoning.is_some(),
        input,
        cost: spec.pricing.as_ref().map(|pricing| {
            octet_agent::extension_process::ExtensionModelCost {
                input: pricing.input.0,
                output: pricing.output.0,
                cache_read: pricing.cache_read.0,
                cache_write: pricing.cache_write_5m.0,
            }
        }),
        context_window: spec.limits.context_window,
        max_tokens: spec.limits.max_output_tokens,
    })
}

fn block_on_runtime<F>(future: F) -> anyhow::Result<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    let handle = Handle::try_current()
        .map_err(|_| anyhow::anyhow!("executable extensions require the octet Tokio runtime"))?;
    if handle.runtime_flavor() != RuntimeFlavor::MultiThread {
        anyhow::bail!("executable extensions require octet's multi-thread runtime");
    }
    Ok(tokio::task::block_in_place(|| handle.block_on(future)))
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
