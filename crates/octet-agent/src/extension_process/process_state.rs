//! Handshake types and the shared state behind an extension process handle.

use super::*;

/// Extension identity sent during initialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionIdentity {
    /// Stable extension name.
    pub name: String,
    /// Extension semantic version.
    pub version: String,
    /// Manifest file used to launch this process.
    pub manifest_path: PathBuf,
    /// Resource provenance.
    pub source: ExtensionSource,
}

/// Host-to-extension initialize parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitializeRequest {
    /// API version the host expects.
    pub api_version: String,
    /// octet crate version.
    pub octet_version: String,
    /// Extension identity and provenance.
    pub extension: ExtensionIdentity,
    /// Active workspace.
    pub workspace: PathBuf,
    /// Manifest-declared privileges.
    pub capabilities: ExtensionCapabilities,
    /// Manifest-declared contribution names.
    pub contributes: ManifestContributions,
    /// Initial session/model/skill state.
    pub host: ExtensionHostState,
    /// Declared CLI flag values projected into API `0.2` initialize so that
    /// `pi.getFlag` observes host-resolved values rather than local defaults.
    /// Reuses the API `0.3` [`api_v03::InitializeFlagValue`] shape. Omitted
    /// byte-for-byte for API `0.1` and for a host that has no flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag_values: Option<Vec<api_v03::InitializeFlagValue>>,
    /// Additive API `0.2` feature and limit negotiation. Frozen API `0.1`
    /// initialization omits this field byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ExtensionProtocolRequest>,
}

/// Extension-to-host initialize result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeResponse {
    /// API version implemented by the child.
    pub api_version: String,
    /// Complete schemas for manifest-declared tools.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Complete metadata for manifest-declared commands.
    #[serde(default)]
    pub commands: Vec<CommandDefinition>,
    /// Tool names for which this generation provides bounded semantic renderers.
    ///
    /// Runtime discovery requires the `dynamic_tool_renderers` feature; static
    /// manifests keep their existing exact declaration behavior.
    #[serde(default)]
    pub tool_renderers: Vec<String>,
    /// Complete metadata for manifest-declared shortcuts.
    #[serde(default)]
    pub shortcuts: Vec<ShortcutDefinition>,
    /// Negotiated API `0.2` features and limits. API `0.1` must omit it.
    #[serde(default)]
    pub protocol: Option<ExtensionProtocolResponse>,
}

/// Host-to-extension tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Tool name.
    pub name: String,
    /// Model-produced arguments.
    pub arguments: serde_json::Value,
    /// Frozen live-catalog revision used to resolve the handler. Present only
    /// for API `0.2` extensions that negotiated `dynamic_tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_revision: Option<u64>,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host-to-extension slash-command call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
    /// Command name without a leading slash.
    pub name: String,
    /// Tokenized user arguments.
    pub arguments: Vec<String>,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host-to-extension terminal shortcut call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShortcutRequest {
    /// Manifest-declared shortcut action name.
    pub name: String,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host-to-extension lifecycle hook call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookRequest {
    /// Hook boundary.
    pub hook: ExtensionHook,
    /// Boundary-specific semantic payload.
    pub payload: serde_json::Value,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host request for extension-provided prompt context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextRequest {
    /// Immediate prompt before extension context is composed, when available.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host request for one semantic UI surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusRequest {
    /// Surface to populate.
    pub surface: ExtensionUiSurface,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host request for the extension's `/extensions` options menu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuRequest {
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// Host request for semantic tool render output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolRenderRequest {
    /// Tool whose lifecycle/result is being rendered.
    pub name: String,
    /// Tool arguments.
    pub arguments: serde_json::Value,
    /// Completed result text, when available.
    #[serde(default)]
    pub output: Option<String>,
    /// Whether the completed result is an error.
    #[serde(default)]
    pub is_error: bool,
    /// Current execution metadata.
    pub context: ExtensionExecutionContext,
}

/// A running executable extension. Clones share the same supervised child and
/// can be registered through the existing native [`ExtensionHost`].
#[derive(Clone)]
pub struct ExtensionProcess {
    pub(super) inner: Arc<ExtensionProcessInner>,
}

pub(super) enum CatalogMutation {
    Register(Vec<ToolDefinition>),
    Unregister(Vec<String>),
}

pub(super) struct CatalogUpdateRequest {
    pub(super) request_id: ExtensionRequestId,
    pub(super) generation: u64,
    pub(super) mutation: CatalogMutation,
    pub(super) catalog: Arc<StdRwLock<Vec<ToolDefinition>>>,
    pub(super) writer: mpsc::Sender<WriterFrame>,
    pub(super) child_requests: ChildRequests,
    pub(super) max_message_bytes: usize,
}

#[derive(Default)]
pub(super) struct AnsweredConfirmations {
    pub(super) recent: VecDeque<(u64, ExtensionRequestId)>,
}

impl AnsweredConfirmations {
    pub(super) fn insert(&mut self, generation: u64, request_id: ExtensionRequestId) -> bool {
        if self.contains(generation, &request_id) {
            return false;
        }
        if self.recent.len() == ANSWERED_CONFIRMATION_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back((generation, request_id));
        true
    }

    pub(super) fn remove(&mut self, generation: u64, request_id: &ExtensionRequestId) {
        if let Some(index) = self.recent.iter().position(|(entry_generation, entry_id)| {
            *entry_generation == generation && entry_id == request_id
        }) {
            self.recent.remove(index);
        }
    }

    pub(super) fn contains(&self, generation: u64, request_id: &ExtensionRequestId) -> bool {
        self.recent.iter().any(|(entry_generation, entry_id)| {
            *entry_generation == generation && entry_id == request_id
        })
    }

    pub(super) fn retain_generation(&mut self, generation: u64) {
        self.recent
            .retain(|(entry_generation, _)| *entry_generation == generation);
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.recent.len()
    }
}

pub(super) struct AnsweredConfirmationReservation<'a> {
    pub(super) answered: &'a StdMutex<AnsweredConfirmations>,
    pub(super) generation: u64,
    pub(super) request_id: ExtensionRequestId,
    pub(super) committed: bool,
}

impl AnsweredConfirmationReservation<'_> {
    pub(super) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for AnsweredConfirmationReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            lock_std_mutex(self.answered).remove(self.generation, &self.request_id);
        }
    }
}

pub(super) struct ExtensionProcessInner {
    pub(super) descriptor: DiscoveredExtension,
    pub(super) config: ExtensionRuntimeConfig,
    pub(super) host_state: StdRwLock<ExtensionHostState>,
    pub(super) contributions: ExtensionContributions,
    pub(super) connection: StdRwLock<Arc<ProcessConnection>>,
    pub(super) events: broadcast::Sender<ExtensionEvent>,
    pub(super) initial_events: StdMutex<Option<broadcast::Receiver<ExtensionEvent>>>,
    pub(super) answered_confirmations: StdMutex<AnsweredConfirmations>,
    pub(super) answered_inputs: StdMutex<AnsweredConfirmations>,
    pub(super) generation: AtomicU64,
    pub(super) next_generation: AtomicU64,
    pub(super) instance_id: String,
    pub(super) generation_changed: Arc<Notify>,
    pub(super) reload_guard: Mutex<()>,
    pub(super) supervisor_cancelled: AtomicBool,
    pub(super) artifact_store: ArtifactStore,
    pub(super) approval_store: Arc<ExtensionApprovalStore>,
    pub(super) lifecycle: StdMutex<ActiveLifecycleState>,
    pub(super) session_hooks: StdMutex<BTreeMap<String, ActiveSessionHookBinding>>,
    pub(super) dynamic_tool_registration: StdMutex<Option<DynamicToolRegistration>>,
    pub(super) dynamic_tool_registration_ready: Notify,
    pub(super) delegation_service: Arc<StdRwLock<Option<ExtensionDelegationService>>>,
    pub(super) catalog_updates: mpsc::Sender<CatalogUpdateRequest>,
}

/// Stable IDs attached to global tool lifecycle observations for the active
/// model turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionLifecycleTurnContext {
    /// Stable session ID.
    pub session_id: String,
    /// Stable run ID.
    pub run_id: String,
    /// Stable turn ID.
    pub turn_id: String,
}

#[derive(Default)]
pub(super) struct ActiveLifecycleState {
    pub(super) sessions: HashMap<String, ActiveLifecycleSession>,
    pub(super) turns: HashMap<String, ActiveLifecycleTurn>,
    pub(super) tools: HashMap<(String, String), ActiveLifecycleTool>,
}

#[derive(Clone)]
pub(super) struct LifecycleEndpoint {
    pub(super) generation: u64,
    pub(super) connection: Arc<ProcessConnection>,
}

/// Host-owned lifecycle state for one typed API `0.3` session-hook binding.
/// The payload contains only the opaque owner key and generation fence.
#[derive(Clone)]
pub(super) struct ActiveSessionHookBinding {
    pub(super) session_id: String,
    pub(super) start_reason: &'static str,
    pub(super) started_at: Instant,
    pub(super) endpoint: LifecycleEndpoint,
    pub(super) start_outcome: Arc<SessionHookStartOutcome>,
}

#[derive(Clone)]
pub(super) struct ActiveLifecycleTurn {
    pub(super) context: ExtensionLifecycleTurnContext,
    pub(super) started_at: Instant,
    pub(super) endpoint: LifecycleEndpoint,
    pub(super) start_queued: bool,
}

pub(super) struct ActiveLifecycleTool {
    pub(super) name: String,
    pub(super) started_at: Instant,
    pub(super) context: ExtensionLifecycleTurnContext,
    pub(super) endpoint: LifecycleEndpoint,
}

#[derive(Clone)]
pub(super) struct ActiveLifecycleSession {
    pub(super) session_id: String,
    pub(super) run_id: Option<String>,
    pub(super) started_at: Instant,
    pub(super) endpoint: LifecycleEndpoint,
}

/// Events a candidate reports before cutover are retained, in order, for the
/// generation that is about to become active. The bound matches the candidate's
/// own event-channel capacity, so a relay can never silently outrun its peer;
/// beyond it the candidate is retired rather than discarding its report.
pub(super) const MAX_DEFERRED_CANDIDATE_EVENTS: usize =
    super::process_group::EXTENSION_EVENT_CAPACITY;

pub(super) fn candidate_event_requires_host_response(event: &ExtensionEvent) -> bool {
    matches!(
        event,
        ExtensionEvent::ConfirmationRequested { .. }
            | ExtensionEvent::ExecRequested { .. }
            | ExtensionEvent::McpRegistrationRequested { .. }
            | ExtensionEvent::PolicyEvaluationRequested { .. }
            | ExtensionEvent::InputRequested { .. }
            | ExtensionEvent::RemoteUiRequested { .. }
    )
}

pub(super) async fn forward_candidate_events(
    inner: Weak<ExtensionProcessInner>,
    generation: u64,
    candidate: Weak<ProcessConnection>,
    mut events: broadcast::Receiver<ExtensionEvent>,
) {
    // A candidate reports its contributions, downloads, and diagnostics as soon
    // as it is initialized, which is before this host cuts the generation over.
    // Those events describe the generation that is about to be active, so they
    // are retained in order and delivered at cutover instead of being dropped:
    // a discarded registration leaves the peer registered in its own view and
    // unavailable in the host's. A candidate that outruns its own cutover is
    // retired rather than silently discarding what it already reported.
    let mut deferred = VecDeque::new();
    let mut deferred_requests = 0usize;
    loop {
        let Some(current) = inner.upgrade() else {
            return;
        };
        let generation_changed = Arc::clone(&current.generation_changed);
        let public_events = current.events.clone();
        drop(current);
        let generation_changed = generation_changed.notified();
        tokio::pin!(generation_changed);
        generation_changed.as_mut().enable();
        let active = inner
            .upgrade()
            .is_some_and(|current| current.generation.load(Ordering::Acquire) == generation);
        if active {
            while let Some(event) = deferred.pop_front() {
                if candidate_event_requires_host_response(&event) {
                    deferred_requests = deferred_requests.saturating_sub(1);
                }
                let _ = public_events.send(event);
            }
        }
        let received = tokio::select! {
            event = events.recv() => Some(event),
            _ = &mut generation_changed => None,
        };
        let Some(received) = received else {
            continue;
        };
        let active = inner
            .upgrade()
            .is_some_and(|current| current.generation.load(Ordering::Acquire) == generation);
        match received {
            Ok(event) if active => {
                let _ = public_events.send(event);
            }
            Ok(event) => {
                let request = candidate_event_requires_host_response(&event);
                // Requests keep their own smaller bound: the host must answer
                // them, and an unbounded backlog would pin the reload.
                let overflowed = (request && deferred_requests >= MAX_CHILD_WORKERS)
                    || deferred.len() >= MAX_DEFERRED_CANDIDATE_EVENTS;
                if overflowed {
                    if let Some(candidate) = candidate.upgrade() {
                        candidate.terminate().await;
                    }
                    return;
                }
                if request {
                    deferred_requests += 1;
                }
                deferred.push_back(event);
            }
            Err(broadcast::error::RecvError::Lagged(count)) => {
                if active {
                    let _ = public_events.send(ExtensionEvent::Diagnostic {
                        message: format!(
                            "extension event stream dropped {count} candidate event(s)"
                        ),
                    });
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

pub(super) fn clear_matching_lifecycle_turn(
    lifecycle: &mut ActiveLifecycleState,
    resource_owner: Option<&str>,
    turn_id: &str,
) {
    lifecycle.turns.retain(|owner, turn| {
        resource_owner.is_some_and(|expected| expected != owner) || turn.context.turn_id != turn_id
    });
    lifecycle.tools.retain(|(owner, _), tool| {
        resource_owner.is_some_and(|expected| expected != owner) || tool.context.turn_id != turn_id
    });
}

/// Retained host dispatch outcome, separate from ownership retained before dispatch.
/// Finishing a cancelled wait does not establish that extension execution stopped.
#[derive(Default)]
pub(super) struct SessionHookStartOutcome {
    finished: AtomicBool,
    succeeded: AtomicBool,
}
impl SessionHookStartOutcome {
    pub(super) fn succeeded(&self) -> bool {
        self.finished.load(Ordering::Acquire) && self.succeeded.load(Ordering::Relaxed)
    }
}
/// Dropping an in-flight host attempt refuses discovery, never implying success.
pub(super) struct SessionHookStartAttempt(pub(super) Arc<SessionHookStartOutcome>);
impl SessionHookStartAttempt {
    pub(super) fn finish(&self, succeeded: bool) {
        self.0.succeeded.store(succeeded, Ordering::Relaxed);
    }
}
impl Drop for SessionHookStartAttempt {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
    }
}
