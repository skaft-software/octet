//! Extension events and the session lifecycle service.

use super::*;

/// Asynchronous process-to-host event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ExtensionEvent {
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
    /// Create a durable session without switching to it.
    Create,
    /// Fork the active durable session without switching to the fork.
    Fork,
    /// Make an existing workspace session active.
    Switch {
        /// Opaque, schema-bounded session identifier.
        session_id: String,
    },
    /// Reopen the active session from its durable descriptor.
    Reload,
}

/// Terminal disposition supplied by the product's active-session driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionSessionLifecycleError {
    /// The interactive active-session driver is not safely bound.
    Unavailable,
    /// The bounded operation reached the idle driver but failed.
    Failed,
}

pub(super) struct SessionLifecycleDriverState {
    pub(super) active: AtomicBool,
    pub(super) epoch: AtomicU64,
}

/// Sender half of the bounded active-session lifecycle service.
///
/// A product constructs this before process startup, offers it only to API 0.3
/// peers, and activates it after the current application/session is safe to
/// mutate. Deactivation fences queued work from a previous app generation.
#[derive(Clone)]
pub struct ExtensionSessionLifecycleService {
    pub(super) sender: mpsc::Sender<ExtensionSessionLifecycleRequest>,
    pub(super) state: Arc<SessionLifecycleDriverState>,
}

/// Receiver half held exclusively by the product's idle-boundary driver.
pub struct ExtensionSessionLifecycleReceiver {
    pub(super) receiver: mpsc::Receiver<ExtensionSessionLifecycleRequest>,
    pub(super) state: Arc<SessionLifecycleDriverState>,
}

/// A single admitted request awaiting an active-session outcome.
pub struct ExtensionSessionLifecycleRequest {
    pub(super) operation: ExtensionSessionLifecycleOperation,
    pub(super) epoch: u64,
    pub(super) response: oneshot::Sender<Result<String, ExtensionSessionLifecycleError>>,
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
        });
        Ok((
            Self {
                sender,
                state: Arc::clone(&state),
            },
            ExtensionSessionLifecycleReceiver { receiver, state },
        ))
    }

    /// Enables admission for the currently bound active application/session.
    pub fn activate(&self) {
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.state.active.store(true, Ordering::Release);
    }

    /// Rejects future work and fences queued work from the prior binding.
    pub fn deactivate(&self) {
        self.state.active.store(false, Ordering::Release);
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn try_submit(
        &self,
        operation: ExtensionSessionLifecycleOperation,
    ) -> Result<
        oneshot::Receiver<Result<String, ExtensionSessionLifecycleError>>,
        SessionLifecycleSubmitError,
    > {
        if !self.state.active.load(Ordering::Acquire) {
            return Err(SessionLifecycleSubmitError::Unavailable);
        }
        let epoch = self.state.epoch.load(Ordering::Acquire);
        if !self.state.active.load(Ordering::Acquire) {
            return Err(SessionLifecycleSubmitError::Unavailable);
        }
        let (response, receiver) = oneshot::channel();
        let request = ExtensionSessionLifecycleRequest {
            operation,
            epoch,
            response,
        };
        match self.sender.try_send(request) {
            Ok(()) => Ok(receiver),
            Err(mpsc::error::TrySendError::Full(_)) => Err(SessionLifecycleSubmitError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(SessionLifecycleSubmitError::Unavailable)
            }
        }
    }
}

impl ExtensionSessionLifecycleReceiver {
    /// Returns the next live request. Stale, deactivated, and cancelled requests
    /// are terminalized without exposing them to a replacement app binding.
    pub fn try_next(&mut self) -> Option<ExtensionSessionLifecycleRequest> {
        loop {
            let request = match self.receiver.try_recv() {
                Ok(request) => request,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    return None
                }
            };
            let current = self.state.active.load(Ordering::Acquire)
                && self.state.epoch.load(Ordering::Acquire) == request.epoch;
            if current && !request.response.is_closed() {
                return Some(request);
            }
            let _ = request
                .response
                .send(Err(ExtensionSessionLifecycleError::Unavailable));
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
    }

    /// Delivers exactly one product outcome to the extension process.
    pub fn respond(self, result: Result<String, ExtensionSessionLifecycleError>) {
        let _ = self.response.send(result);
    }
}
