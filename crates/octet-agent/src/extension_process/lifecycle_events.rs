//! Extension events and the session lifecycle service.

use super::*;

/// Asynchronous process-to-host event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ExtensionEvent {
    /// Transient registration update routed to the admitted resident MCP bridge.
    McpRegistrationRequested {
        /// Reverse request identity.
        request_id: ExtensionRequestId,
        /// Originating native process generation.
        generation: u64,
        /// Host-issued owner; never trusted from a registry descriptor.
        owner: ExtensionResourceOwner,
        /// Bounded sensitive snapshot; must not be logged.
        request: ExtensionMcpRequest,
    },
    /// Owner-fenced direct process request for the native frontend executor.
    ExecRequested {
        /// Reverse request ID.
        request_id: ExtensionRequestId,
        /// Native process generation.
        generation: u64,
        /// Host-issued resource owner.
        owner: ExtensionResourceOwner,
        /// Validated execution parameters.
        request: ExtensionExecRequest,
    },
    /// User-visible notification.
    Notification {
        /// Notification content.
        notification: ExtensionNotification,
    },
    /// Interactive confirmation request. The generation prevents a stale
    /// request from being answered after a reload.
    ConfirmationRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that originated the request.
        generation: u64,
        /// Originating host request for API `0.2` correlation.
        #[serde(default)]
        parent_request_id: Option<u64>,
        /// Confirmation content.
        request: ConfirmationRequest,
    },
    /// Host-policy classification requested by an API `0.2` extension.
    PolicyEvaluationRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that originated the request.
        generation: u64,
        /// Originating host request.
        parent_request_id: u64,
        /// Structured action intent.
        intent: ExtensionActionIntent,
    },
    /// Ephemeral frontend input requested by an API `0.2` extension.
    InputRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that originated the request.
        generation: u64,
        /// Originating host request.
        parent_request_id: u64,
        /// Frontend-visible request without any answer value.
        request: ExtensionInputRequest,
    },
    /// Unsolicited prompt context contribution.
    ContextContributed {
        /// Context content.
        contribution: ContextContribution,
    },
    /// Unsolicited semantic TUI contribution.
    StatusContributed {
        /// Status/header/footer content.
        contribution: ExtensionStatusContribution,
    },
    /// Bounded semantic UI state from an extension generation.
    UiContributed {
        /// Process generation that owns the snapshot.
        generation: u64,
        /// Complete keyed or global semantic contribution.
        contribution: ExtensionUiContribution,
    },
    /// A host-owned editor operation awaiting a frontend projection.
    EditorRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Bounded editor operation.
        request: ExtensionEditorRequest,
    },
    /// An extension registered one bounded autocomplete chain.
    AutocompleteRegistered {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the registration.
        generation: u64,
        /// Informational extension-local registration revision.
        registration: ExtensionAutocompleteRegistration,
    },
    /// Frontend-neutral semantic state for activity and detail inspectors.
    PresentationUpdated {
        /// Process generation that owns the complete snapshot.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        resource_owner: Option<ExtensionResourceOwner>,
        /// Monotonic extension-owned state snapshot.
        snapshot: ExtensionPresentationSnapshot,
    },
    /// One host-owned composer operation awaiting a foreground projection.
    ComposerRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Bounded composer operation.
        operation: ExtensionComposerOperation,
    },
    /// One extension-owned durable session entry operation.
    SessionEntryRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Bounded session entry operation.
        operation: ExtensionSessionEntryOperation,
    },
    /// One bounded message injection awaiting the foreground session.
    MessageInjectionRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Bounded injected message.
        injection: ExtensionMessageInjection,
    },
    /// One runtime shortcut registration awaiting the host keymap.
    ShortcutRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Extension-owned action identifier reported back by `shortcut/trigger`.
        shortcut_id: String,
        /// Portable terminal key spelling.
        key: String,
        /// User-facing summary of the action.
        description: String,
    },
    /// One active-tool replacement awaiting the host tool policy.
    ActiveToolsRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Complete replacement active tool set.
        names: Vec<String>,
    },
    /// One foreground terminal handoff operation awaiting the frontend.
    TerminalRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Bounded handoff operation.
        operation: ExtensionTerminalOperation,
    },
    /// One owner-fenced cached UI operation awaiting the foreground frontend.
    RemoteUiRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the operation.
        generation: u64,
        /// Complete host-issued resource owner.
        owner: ExtensionResourceOwner,
        /// Validated open or close operation; frames use a separate mailbox.
        operation: ExtensionRemoteUiOperation,
    },
    /// One read-only context snapshot awaiting the foreground session.
    ///
    /// The child request stays registered until the frontend answers through
    /// `respond_to_extension_request`.
    ContextSnapshotRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Bounded snapshot operation.
        operation: ExtensionContextOperation,
    },
    /// One read-only model view awaiting the foreground session.
    ///
    /// The child request stays registered until the frontend answers through
    /// `respond_to_extension_request`.
    ModelViewRequested {
        /// Process-originated JSON-RPC ID.
        request_id: ExtensionRequestId,
        /// Process generation that owns the request.
        generation: u64,
        /// Host-derived resource owner, or process scope when absent.
        owner: Option<ExtensionResourceOwner>,
        /// Bounded model operation.
        operation: ExtensionModelOperation,
    },
    /// Bounded stderr or protocol diagnostic.
    Diagnostic {
        /// Human-readable diagnostic text.
        message: String,
    },
}

/// Host-owned identity for one admitted extension operation.
///
/// JSON-RPC request IDs restart in every process generation, so consumers must
/// match both fields before handling an operation-scoped child request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionOperationToken {
    /// Process generation that admitted the operation.
    pub generation: u64,
    /// Host JSON-RPC request ID within that generation.
    pub parent_request_id: u64,
}

impl ExtensionOperationToken {
    /// Returns whether a child event belongs to this exact operation.
    pub fn owns(self, generation: u64, parent_request_id: u64) -> bool {
        self.generation == generation && self.parent_request_id == parent_request_id
    }
}

/// One requested mutation of the active host session.
///
/// These operations deliberately target the product's active session, unlike
/// the API 0.2 `agent/*` service which manages extension-owned child sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionSessionLifecycleOperation {
    /// Select the foreground model/reasoning without replacing its binding.
    ModelControl(ExtensionModelControl),
    /// Resolve only at an observed idle foreground boundary; does not mutate state.
    WaitForIdle,
    /// Pi `newSession`: create a durable session and switch to it.
    Create,
    /// Pi newSession options; setup authority is minted at admission.
    CreateWithOptions {
        /// Optional inert parent-session file reference.
        parent_session: Option<String>,
        /// Original live request and owner that authorize setup.
        setup_parent: Option<(u64, ExtensionResourceOwner)>,
    },
    /// One synchronous mutation or completion of the real new-session setup.
    Setup {
        /// Original request authorizing this setup continuation.
        parent_request_id: u64,
        /// Host-issued foreground process/session identity.
        owner: ExtensionResourceOwner,
        /// Admitted extension metadata namespace.
        namespace: String,
        /// Bounded journal mutation or setup completion.
        mutation: serde_json::Value,
    },
    /// Pi `fork`: fork the active session at an entry and switch to the fork.
    Fork {
        /// Entry to fork at; the active head when absent.
        entry_id: Option<String>,
        /// Pi `position: "at"` keeps the entry; `"before"` forks from its parent.
        at: bool,
    },
    /// Make an existing workspace session active.
    Switch {
        /// Opaque, schema-bounded session identifier.
        session_id: String,
    },
    /// Pi `reload`: reload extensions and resources at the idle boundary.
    Reload,
    /// Compact local history at the actual idle boundary, then await after-hooks.
    Compact {
        /// Optional bounded instructions for the native summary operation.
        instructions: Option<String>,
    },
}

/// The actual newly committed native compaction, never the current metadata leaf.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExtensionSessionCompactionResult {
    /// Durable Compaction entry identifier.
    pub entry_id: String,
    /// Validated summary stored in that entry.
    pub summary: String,
    /// Retained history boundary stored in that entry.
    pub first_kept: String,
}

/// Terminal disposition supplied by the product's active-session driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionSessionLifecycleError {
    /// A Pi before-session hook cancelled replacement without changing sessions.
    Cancelled,
    /// The interactive active-session driver is not safely bound.
    Unavailable,
    /// The bounded operation reached the idle driver but failed.
    Failed,
}

pub(super) struct SessionLifecycleDriverState {
    pub(super) active: AtomicBool,
    pub(super) epoch: AtomicU64,
    pub(super) changed: Notify,
}

/// Sender half of the bounded active-session lifecycle service.
///
/// A product constructs this before process startup, offers it only to API 0.3/0.4
/// peers, and activates it after the current application/session is safe to
/// mutate. Deactivation fences queued work from a previous app generation.
#[derive(Clone)]
pub struct ExtensionSessionLifecycleService {
    capacity: Arc<Semaphore>,
    compaction: bool,
    model_control: bool,
    pub(super) sender: mpsc::Sender<ExtensionSessionLifecycleRequest>,
    pub(super) state: Arc<SessionLifecycleDriverState>,
}

/// Receiver half held exclusively by the product's idle-boundary driver.
pub struct ExtensionSessionLifecycleReceiver {
    pub(super) receiver: mpsc::Receiver<ExtensionSessionLifecycleRequest>,
    deferred: Option<ExtensionSessionLifecycleRequest>,
    pub(super) state: Arc<SessionLifecycleDriverState>,
}

/// A single admitted request awaiting an active-session outcome.
pub struct ExtensionSessionLifecycleRequest {
    _capacity: tokio::sync::OwnedSemaphorePermit,
    pub(super) operation: ExtensionSessionLifecycleOperation,
    pub(super) epoch: u64,
    response: SessionLifecycleResponse,
    state: Arc<SessionLifecycleDriverState>,
    cancellation: CancellationToken,
    authority: Option<SessionCompactionAuthority>,
}

enum SessionLifecycleResponse {
    Setup(oneshot::Sender<Result<serde_json::Value, String>>),
    SessionId(oneshot::Sender<Result<String, ExtensionSessionLifecycleError>>),
    Compaction(oneshot::Sender<Result<ExtensionSessionCompactionResult, String>>),
    ModelControl(oneshot::Sender<Result<serde_json::Value, String>>),
}

impl SessionLifecycleResponse {
    fn is_closed(&self) -> bool {
        match self {
            Self::Setup(response) => response.is_closed(),
            Self::SessionId(response) => response.is_closed(),
            Self::Compaction(response) => response.is_closed(),
            Self::ModelControl(response) => response.is_closed(),
        }
    }

    fn unavailable(self) {
        match self {
            Self::SessionId(response) => {
                let _ = response.send(Err(ExtensionSessionLifecycleError::Unavailable));
            }
            Self::Setup(response) => { let _ = response.send(Err("session setup owner retired".into())); }
            Self::ModelControl(response) => {
                let _ = response.send(Err("model selection owner retired".into()));
            }
            Self::Compaction(response) => {
                let _ = response.send(Err(
                    "session compaction was cancelled or its owner retired".into()
                ));
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct SessionCompactionAuthority {
    pub(super) parent_request_id: u64,
    pub(super) callback: bool,
    pub(super) owner: ExtensionResourceOwner,
    pub(super) issued: IssuedResourceOwners,
    pub(super) closed: Arc<AtomicBool>,
    pub(super) draining: Arc<AtomicBool>,
    pub(super) response: Arc<ChildResponseState>,
}

impl SessionCompactionAuthority {
    pub(super) fn is_current(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && !self.draining.load(Ordering::Acquire)
            && self.response.state.load(Ordering::Acquire) == CHILD_ACTIVE
            && lock_std_mutex(&self.issued).contains(&self.owner)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionLifecycleSubmitError {
    Unavailable,
    Full,
}

impl ExtensionSessionLifecycleService {
    /// Constructs a bounded service and its sole product-owned receiver.
    pub fn channel(
        capacity: usize,
    ) -> Result<(Self, ExtensionSessionLifecycleReceiver), &'static str> {
        if capacity == 0 || capacity > MAX_EXTENSION_SESSION_LIFECYCLE_QUEUE {
            return Err("session lifecycle queue capacity is outside its bounded range");
        }
        let (sender, receiver) = mpsc::channel(capacity);
        let state = Arc::new(SessionLifecycleDriverState {
            active: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            changed: Notify::new(),
        });
        Ok((
            Self {
                capacity: Arc::new(Semaphore::new(capacity)),
                compaction: false,
                model_control: false,
                sender,
                state: Arc::clone(&state),
            },
            ExtensionSessionLifecycleReceiver {
                receiver,
                deferred: None,
                state,
            },
        ))
    }

    /// Opt in before process startup only when this receiver has a native idle
    /// compaction consumer. Ordinary lifecycle embedders do not offer compaction.
    pub fn with_compaction(mut self) -> Self {
        self.compaction = true;
        self
    }

    /// Opt in only when the foreground driver implements in-place selection.
    pub fn with_model_control(mut self) -> Self {
        self.model_control = true;
        self
    }

    pub(super) fn supports_model_control(&self) -> bool { self.model_control }

    pub(super) fn try_submit_model_control(
        &self, operation: ExtensionModelControl, authority: SessionCompactionAuthority,
    ) -> Result<oneshot::Receiver<Result<serde_json::Value, String>>, SessionLifecycleSubmitError> {
        if !self.model_control || !authority.is_current() {
            return Err(SessionLifecycleSubmitError::Unavailable);
        }
        let (response, receiver) = oneshot::channel();
        self.try_submit_request(
            ExtensionSessionLifecycleOperation::ModelControl(operation),
            SessionLifecycleResponse::ModelControl(response),
            CancellationToken::default(), Some(authority),
        )?;
        Ok(receiver)
    }

    pub(super) fn supports_compaction(&self) -> bool {
        self.compaction
    }

    /// Enables admission for the currently bound active application/session.
    pub fn activate(&self) {
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.state.active.store(true, Ordering::Release);
        self.state.changed.notify_waiters();
    }

    /// Rejects future work and fences queued work from the prior binding.
    pub fn deactivate(&self) {
        self.state.active.store(false, Ordering::Release);
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.state.changed.notify_waiters();
    }

    pub(super) fn try_submit(
        &self,
        operation: ExtensionSessionLifecycleOperation,
    ) -> Result<
        oneshot::Receiver<Result<String, ExtensionSessionLifecycleError>>,
        SessionLifecycleSubmitError,
    > {
        let (response, receiver) = oneshot::channel();
        self.try_submit_request(
            operation,
            SessionLifecycleResponse::SessionId(response),
            CancellationToken::default(),
            None,
        )?;
        Ok(receiver)
    }

    pub(super) fn try_submit_setup(&self, operation: ExtensionSessionLifecycleOperation) -> Result<oneshot::Receiver<Result<serde_json::Value, String>>, SessionLifecycleSubmitError> {
        let (response, receiver) = oneshot::channel();
        self.try_submit_request(operation, SessionLifecycleResponse::Setup(response), CancellationToken::default(), None)?;
        Ok(receiver)
    }

    pub(super) fn try_submit_compaction(
        &self,
        instructions: Option<String>,
        cancellation: CancellationToken,
        authority: SessionCompactionAuthority,
    ) -> Result<
        oneshot::Receiver<Result<ExtensionSessionCompactionResult, String>>,
        SessionLifecycleSubmitError,
    > {
        if !self.compaction || !authority.is_current() {
            return Err(SessionLifecycleSubmitError::Unavailable);
        }
        let (response, receiver) = oneshot::channel();
        self.try_submit_request(
            ExtensionSessionLifecycleOperation::Compact { instructions },
            SessionLifecycleResponse::Compaction(response),
            cancellation,
            Some(authority),
        )?;
        Ok(receiver)
    }

    fn try_submit_request(
        &self,
        operation: ExtensionSessionLifecycleOperation,
        response: SessionLifecycleResponse,
        cancellation: CancellationToken,
        authority: Option<SessionCompactionAuthority>,
    ) -> Result<(), SessionLifecycleSubmitError> {
        if !self.state.active.load(Ordering::Acquire) {
            return Err(SessionLifecycleSubmitError::Unavailable);
        }
        let epoch = self.state.epoch.load(Ordering::Acquire);
        if !self.state.active.load(Ordering::Acquire) {
            return Err(SessionLifecycleSubmitError::Unavailable);
        }
        let capacity = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionLifecycleSubmitError::Full)?;
        let request = ExtensionSessionLifecycleRequest {
            _capacity: capacity,
            operation,
            epoch,
            response,
            state: Arc::clone(&self.state),
            cancellation,
            authority,
        };
        match self.sender.try_send(request) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(SessionLifecycleSubmitError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(SessionLifecycleSubmitError::Unavailable)
            }
        }
    }
}

impl ExtensionSessionLifecycleReceiver {
    /// Take an idle barrier without reordering an earlier session mutation.
    /// The foreground command pump uses this only while its shell reports no run.
    pub fn try_next_idle_wait(&mut self) -> Option<ExtensionSessionLifecycleRequest> {
        let request = self.try_next()?;
        if matches!(
            request.operation(),
            ExtensionSessionLifecycleOperation::WaitForIdle
        ) {
            Some(request)
        } else {
            self.deferred = Some(request);
            None
        }
    }

    /// Returns the next live request. Stale, deactivated, and cancelled requests
    /// are terminalized without exposing them to a replacement app binding.
    pub fn try_next(&mut self) -> Option<ExtensionSessionLifecycleRequest> {
        loop {
            let request = match self.deferred.take() {
                Some(request) => request,
                None => match self.receiver.try_recv() {
                    Ok(request) => request,
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => return None,
                },
            };
            let current = self.state.active.load(Ordering::Acquire)
                && self.state.epoch.load(Ordering::Acquire) == request.epoch;
            if current && !request.is_cancelled() {
                return Some(request);
            }
            request.cancellation.cancel();
            request.response.unavailable();
        }
    }
}

impl ExtensionSessionLifecycleRequest {
    /// Returns the requested operation. The product must settle the request once.
    pub fn operation(&self) -> &ExtensionSessionLifecycleOperation {
        &self.operation
    }

    /// Returns whether protocol cancellation or process shutdown already won.
    pub fn is_cancelled(&self) -> bool {
        self.response.is_closed()
            || self.cancellation.is_cancelled()
            || self.authority.as_ref().is_some_and(|authority| {
                !authority.is_current()
                    || !self.state.active.load(Ordering::Acquire)
                    || self.state.epoch.load(Ordering::Acquire) != self.epoch
            })
    }

    /// Token shared with protocol cancellation and owner/epoch retirement.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// The issued compaction owner, rechecked against the real foreground Session.
    pub fn resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        self.authority.as_ref().map(|authority| &authority.owner)
    }

    /// Original numeric request correlation, never append authority. The callback
    /// receives a fresh host request and a separately issued native leaf grant.
    pub fn compaction_callback_parent(&self) -> Option<u64> {
        self.authority.as_ref().filter(|authority| authority.callback)
            .map(|authority| authority.parent_request_id)
    }

    /// Settle only after the durable, in-place model selection has succeeded.
    pub fn respond_model_control(self, result: Result<serde_json::Value, String>) {
        let SessionLifecycleResponse::ModelControl(response) = self.response else {
            unreachable!("model selections use their own response type")
        };
        let _ = response.send(result);
    }

    /// Settle a setup operation only after the actual native mutation.
    pub fn respond_setup(self, result: Result<serde_json::Value, String>) {
        let SessionLifecycleResponse::Setup(response) = self.response else { unreachable!("setup uses its own response type") };
        let _ = response.send(result);
    }

    /// Deliver replacement cancellation/errors to either receipt shape.
    pub fn respond_replacement(self, result: Result<String, ExtensionSessionLifecycleError>) {
        if matches!(&self.response, SessionLifecycleResponse::Setup(_)) {
            self.respond_setup(match result {
                Ok(id) => Ok(serde_json::json!({"session_id":id})),
                Err(ExtensionSessionLifecycleError::Cancelled) => Ok(serde_json::json!({"cancelled":true})),
                Err(error) => Err(format!("session replacement failed: {error:?}")),
            });
        } else { self.respond(result); }
    }

    /// Delivers exactly one ordinary lifecycle outcome to the extension process.
    pub fn respond(self, result: Result<String, ExtensionSessionLifecycleError>) {
        let SessionLifecycleResponse::SessionId(response) = self.response else {
            unreachable!("compaction requests use respond_compaction")
        };
        let _ = response.send(result);
    }

    /// Settle only after native compaction and its after-hook. An error may follow
    /// a durable commit: it is never permission to roll back or replay the request.
    pub fn respond_compaction(self, result: Result<ExtensionSessionCompactionResult, String>) {
        let SessionLifecycleResponse::Compaction(response) = self.response else {
            unreachable!("ordinary lifecycle requests use respond")
        };
        let _ = response.send(result);
    }
}
