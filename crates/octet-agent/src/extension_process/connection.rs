//! The framed protocol connection to an extension process and its writer.

use super::*;

/// Shared transport-size authority. API 0.3 stores the protocol-defined
/// payload limit (without LF); legacy transports retain their historical full
/// line limit. Initialization installs the selected API 0.3 payload bound once
/// before buffered post-handshake frames are released.
pub(super) struct ProtocolFrameLimit {
    pub(super) api_v03: bool,
    pub(super) bytes: AtomicUsize,
}

impl ProtocolFrameLimit {
    pub(super) fn new(max_message_bytes: usize, api_v03: bool) -> Self {
        Self {
            api_v03,
            bytes: AtomicUsize::new(if api_v03 {
                max_message_bytes.saturating_sub(1)
            } else {
                max_message_bytes
            }),
        }
    }

    pub(super) fn max_frame_bytes(&self) -> usize {
        self.bytes.load(Ordering::Acquire)
    }

    pub(super) fn max_message_bytes(&self) -> usize {
        if self.api_v03 {
            self.max_frame_bytes().saturating_add(1)
        } else {
            self.max_frame_bytes()
        }
    }

    pub(super) fn accepts_message_bytes(&self, bytes: usize) -> bool {
        bytes <= self.max_message_bytes()
    }

    pub(super) fn install_selected_api_v03(&self, max_frame_bytes: usize) {
        debug_assert!(self.api_v03);
        self.bytes.store(max_frame_bytes, Ordering::Release);
    }
}

pub(super) struct ProviderStreamIngress {
    pub(super) sender: mpsc::Sender<api_v03::ProviderStreamEvent>,
    pub(super) next_sequence: usize,
    pub(super) terminal: bool,
}

pub(super) type ProviderStreams = Arc<StdMutex<HashMap<String, ProviderStreamIngress>>>;

pub(super) struct ProcessConnection {
    pub(super) writer: mpsc::Sender<WriterFrame>,
    pub(super) child: Arc<Mutex<Child>>,
    pub(super) pending: PendingRequests,
    pub(super) resources: Resources,
    pub(super) resource_cleanup_changed: Arc<Notify>,
    pub(super) issued_resource_owners: IssuedResourceOwners,
    pub(super) session_leaf: Arc<session_leaf::SessionLeafMailbox>,
    pub(super) remote_ui: Arc<RemoteUiMailbox>,
    pub(super) pending_changed: Arc<Notify>,
    pub(super) child_requests: ChildRequests,
    pub(super) next_id: AtomicU64,
    pub(super) closed: Arc<AtomicBool>,
    pub(super) draining: Arc<AtomicBool>,
    pub(super) active_admissions: AtomicU64,
    pub(super) slots: StdRwLock<Arc<Semaphore>>,
    pub(super) frame_limit: Arc<ProtocolFrameLimit>,
    pub(super) shutdown_timeout: Duration,
    pub(super) cancellation_grace: Duration,
    pub(super) tombstone_ttl: Duration,
    pub(super) tombstones: Arc<StdMutex<RequestTombstones>>,
    pub(super) protocol: Arc<StdRwLock<ExtensionNegotiatedProtocol>>,
    pub(super) api_v03_contract: Arc<StdRwLock<Option<api_v03::NegotiatedContract>>>,
    pub(super) initialization_complete: Arc<AtomicBool>,
    pub(super) initialization_changed: Arc<Notify>,
    pub(super) catalog_guard: StdRwLock<()>,
    pub(super) tool_catalog: Arc<StdRwLock<Vec<ToolDefinition>>>,
    pub(super) catalog_revision: AtomicU64,
    pub(super) health: Arc<StdRwLock<ConnectionHealth>>,
    pub(super) events: broadcast::Sender<ExtensionEvent>,
    pub(super) generation: u64,
    pub(super) artifact_store: ArtifactStore,
    pub(super) artifact_leases: AtomicU64,
    pub(super) artifact_leases_changed: Notify,
    pub(super) artifacts_settled: AtomicBool,
    pub(super) provider_registry: Option<Arc<ExtensionProviderRegistry>>,
    pub(super) provider_owner: ExtensionProviderOwner,
    pub(super) provider_streams: ProviderStreams,
    pub(super) next_provider_stream_id: AtomicU64,
    pub(super) provider_stream_buffer: usize,
    pub(super) provider_stream_idle_timeout: Duration,
    pub(super) provider_stream_deadline: Duration,
    pub(super) provider_owner_removed: AtomicBool,
    pub(super) event_bus: Option<Arc<ExtensionEventBus>>,
    pub(super) process_group: ProcessGroupGuard,
    pub(super) message_deltas: StdMutex<MessageDeltaCoalescer>,
}

pub(super) fn connection_is_usable(connection: &ProcessConnection) -> bool {
    if connection.closed.load(Ordering::Acquire) {
        return false;
    }
    matches!(
        read_std_lock(&connection.health).state,
        ExtensionHealthState::Ready | ExtensionHealthState::Degraded
    )
}

#[derive(Clone, Debug)]
pub(super) enum PendingError {
    Closed(String),
    Protocol(String),
    Cancelled(String),
    Remote {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
}

pub(super) type PendingReply = Result<serde_json::Value, PendingError>;

pub(super) type PendingSender = oneshot::Sender<PendingReply>;

pub(super) struct PendingRequest {
    pub(super) method: String,
    pub(super) sender: PendingSender,
    pub(super) terminal: Arc<AtomicU8>,
    pub(super) frame_state: Arc<AtomicU8>,
    pub(super) cancellation_sent: Arc<AtomicBool>,
    /// Parent progress forwarded as ordinary status/output/decoration updates.
    pub(super) progress: Option<ToolProgressSink>,
    /// Parent progress allowed to own child confirmation/input replies. Model
    /// tools use this; commands retain their existing confirmation receiver.
    pub(super) child_interaction_progress: Option<ToolProgressSink>,
    pub(super) resource_owner: Option<ExtensionResourceOwner>,
    pub(super) last_progress_sequence: Option<u64>,
    pub(super) tool_call_policy_digest: Option<[u8; 32]>,
    pub(super) composition_files: Arc<CompositionFiles>,
}

pub(super) fn tool_call_policy_digest(tool: &str, arguments: &serde_json::Value) -> [u8; 32] {
    // JSON Values serialize deterministically (object keys are ordered).
    Sha256::digest(serde_json::to_vec(&(tool, arguments)).expect("JSON values serialize")).into()
}

pub(super) type PendingRequests = Arc<StdMutex<HashMap<u64, PendingRequest>>>;

pub(super) type IssuedResourceOwners = Arc<StdMutex<HashSet<ExtensionResourceOwner>>>;

pub(super) const CHILD_ACTIVE: u8 = 0;

pub(super) const CHILD_RESPONDING: u8 = 1;

pub(super) const CHILD_SETTLED: u8 = 2;

pub(super) struct ChildRequest {
    pub(super) exec_cancelled: bool,
    pub(super) parent_request_id: u64,
    pub(super) response_state: Arc<ChildResponseState>,
    pub(super) policy_intent: Option<ExtensionActionIntent>,
    pub(super) remote_ui: Option<RemoteUiChildRequest>,
}

pub(super) struct RegisteredChildRequest {
    pub(super) parent_request_id: Option<u64>,
    pub(super) progress: Option<ToolProgressSink>,
    pub(super) resource_owner: Option<ExtensionResourceOwner>,
    pub(super) response_state: Arc<ChildResponseState>,
}

pub(super) struct ChildResponseState {
    pub(super) state: AtomicU8,
    pub(super) changed: Notify,
    pub(super) cancel_on_response_abort: StdMutex<Option<String>>,
    pub(super) composition_cancellation: StdMutex<Option<CancellationToken>>,
    pub(super) session_leaf_cancel: StdMutex<Option<crate::session_leaf::SessionLeafCancelHandle>>,
}

pub(super) struct ChildResponseClaim {
    pub(super) child_requests: ChildRequests,
    pub(super) id: ExtensionRequestId,
    pub(super) response_state: Arc<ChildResponseState>,
    pub(super) admitted: bool,
    pub(super) abort_cancel: Option<(mpsc::Sender<WriterFrame>, Arc<ProtocolFrameLimit>)>,
}

impl ChildResponseClaim {
    pub(super) fn mark_admitted(&mut self) {
        let child_requests = Arc::clone(&self.child_requests);
        let mut children = lock_std_mutex(&child_requests);
        self.mark_admitted_with_children(&mut children);
    }

    fn mark_admitted_with_children(
        &mut self,
        children: &mut HashMap<ExtensionRequestId, ChildRequest>,
    ) {
        self.response_state
            .state
            .store(CHILD_SETTLED, Ordering::Release);
        if children
            .get(&self.id)
            .is_some_and(|child| Arc::ptr_eq(&child.response_state, &self.response_state))
        {
            children.remove(&self.id);
        }
        self.admitted = true;
        self.response_state.changed.notify_waiters();
    }
}

impl Drop for ChildResponseClaim {
    fn drop(&mut self) {
        if self.admitted {
            return;
        }
        let deferred_cancel = {
            let mut children = lock_std_mutex(&self.child_requests);
            if !children
                .get(&self.id)
                .is_some_and(|child| Arc::ptr_eq(&child.response_state, &self.response_state))
            {
                return;
            }
            let deferred_cancel =
                lock_std_mutex(&self.response_state.cancel_on_response_abort).take();
            if deferred_cancel.is_some() {
                cancel_composition_work(&self.response_state);
                self.response_state
                    .state
                    .store(CHILD_SETTLED, Ordering::Release);
                children.remove(&self.id);
            } else {
                let _ = self.response_state.state.compare_exchange(
                    CHILD_RESPONDING,
                    CHILD_ACTIVE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            self.response_state.changed.notify_waiters();
            deferred_cancel
        };
        if let (Some(reason), Some((writer, frame_limit))) =
            (deferred_cancel, self.abort_cancel.take())
        {
            let _ = queue_writer_value(
                &writer,
                &frame_limit,
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "method":methods::CANCEL_REQUEST,
                    "params":{"id":self.id,"reason":reason},
                }),
            );
        }
    }
}

pub(super) type ChildRequests = Arc<StdMutex<HashMap<ExtensionRequestId, ChildRequest>>>;

pub(super) struct PendingRegistration {
    pub(super) connection: Weak<ProcessConnection>,
    pub(super) id: u64,
    pub(super) cancellation_reason: Arc<StdMutex<String>>,
    pub(super) armed: bool,
}

impl PendingRegistration {
    pub(super) fn new(
        connection: &Arc<ProcessConnection>,
        id: u64,
        cancellation_reason: Arc<StdMutex<String>>,
    ) -> Self {
        Self {
            connection: Arc::downgrade(connection),
            id,
            cancellation_reason,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(connection) = self.connection.upgrade() else {
            return;
        };
        let reason = lock_std_mutex(&self.cancellation_reason).clone();
        connection.cancel_request(self.id, &reason);
    }
}

pub(super) struct WriterFrame {
    pub(super) line: Vec<u8>,
    pub(super) state: Arc<AtomicU8>,
    pub(super) completion: Option<oneshot::Sender<Result<(), PendingError>>>,
    pub(super) bus_delivery: Option<event_bus::Delivery>,
}

pub(super) struct ZeroizingBytes(pub(super) Vec<u8>);

impl Drop for ZeroizingBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Serialize)]
pub(super) struct ChildSuccessResponse<'a, T: ?Sized> {
    pub(super) jsonrpc: &'static str,
    pub(super) id: &'a ExtensionRequestId,
    pub(super) result: &'a T,
}

/// One prepared process-originated response envelope.
pub(super) enum ChildEnvelope<'a, T: ?Sized> {
    Success(&'a T),
    Error { code: i64, message: String },
}

#[derive(Serialize)]
pub(super) struct ChildErrorResponse<'a> {
    pub(super) jsonrpc: &'static str,
    pub(super) id: &'a ExtensionRequestId,
    pub(super) error: ChildErrorObject,
}

#[derive(Serialize)]
pub(super) struct ChildErrorObject {
    pub(super) code: i64,
    pub(super) message: String,
}

/// One bounded coalesced `message/updated` batch.
pub(super) struct MessageDeltaBatch {
    pub(super) delta: String,
    pub(super) deltas: u64,
}

impl Clone for MessageDeltaBatch {
    fn clone(&self) -> Self {
        Self {
            delta: self.delta.clone(),
            deltas: self.deltas,
        }
    }
}

impl MessageDeltaBatch {
    pub(super) fn into_updated(self, message_id: Option<String>) -> ExtensionMessageUpdated {
        ExtensionMessageUpdated {
            message_id,
            delta: self.delta,
            deltas: self.deltas,
        }
    }
}

/// Coalesces per-token assistant deltas into at most one `message/updated`
/// notification per batch. Every batch is bounded by
/// [`MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES`]; the coalescer never opens a
/// per-delta round trip.
#[derive(Default)]
pub(super) struct MessageDeltaCoalescer {
    pub(super) message_id: Option<String>,
    pub(super) pending: String,
    pub(super) deltas: u64,
    pub(super) first_pending_at: Option<Instant>,
    pub(super) batches_emitted: u64,
}

impl MessageDeltaCoalescer {
    pub(super) fn begin_message(&mut self, message_id: &str) {
        self.pending.clear();
        self.deltas = 0;
        self.first_pending_at = None;
        self.message_id = Some(message_id.to_owned());
    }

    pub(super) fn end_message(&mut self) {
        self.message_id = None;
    }

    /// Appends one delta and returns every batch that must be emitted now.
    pub(super) fn push(&mut self, delta: &str, now: Instant) -> Vec<MessageDeltaBatch> {
        let mut batches = Vec::new();
        if !delta.is_empty()
            && !self.pending.is_empty()
            && self.pending.len() + delta.len() > MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES
        {
            if let Some(batch) = self.take(now) {
                batches.push(batch);
            }
        }
        if !delta.is_empty() {
            if self.first_pending_at.is_none() {
                self.first_pending_at = Some(now);
            }
            self.pending.push_str(delta);
            self.deltas += 1;
        }
        if self.should_flush(now) {
            if let Some(batch) = self.take(now) {
                batches.push(batch);
            }
        }
        batches
    }

    pub(super) fn flush(&mut self, now: Instant) -> Vec<MessageDeltaBatch> {
        match self.take(now) {
            Some(batch) => vec![batch],
            None => Vec::new(),
        }
    }

    pub(super) fn should_flush(&self, now: Instant) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        if self.pending.len() >= MESSAGE_DELTA_FLUSH_BYTES
            || self.deltas >= MESSAGE_DELTA_FLUSH_DELTAS
        {
            return true;
        }
        self.first_pending_at
            .is_some_and(|first| now.duration_since(first) >= MESSAGE_DELTA_FLUSH_INTERVAL)
    }

    pub(super) fn take(&mut self, _now: Instant) -> Option<MessageDeltaBatch> {
        if self.pending.is_empty() {
            self.deltas = 0;
            self.first_pending_at = None;
            return None;
        }
        let batch = MessageDeltaBatch {
            delta: std::mem::take(&mut self.pending),
            deltas: self.deltas,
        };
        self.deltas = 0;
        self.first_pending_at = None;
        self.batches_emitted += 1;
        Some(batch)
    }

    pub(super) fn active_message_id(&self) -> Option<String> {
        self.message_id.clone()
    }
}

impl Drop for WriterFrame {
    fn drop(&mut self) {
        // Some API 0.2 frames carry approval capabilities or secret values.
        // Erasing every bounded frame avoids a fragile sensitive/non-sensitive
        // distinction in the shared writer queue.
        self.line.fill(0);
    }
}

pub(super) struct ArtifactDecodeLease {
    pub(super) connection: Arc<ProcessConnection>,
}

pub(super) struct RequestAdmissionLease {
    pub(super) connection: Arc<ProcessConnection>,
}

impl Drop for RequestAdmissionLease {
    fn drop(&mut self) {
        self.connection
            .active_admissions
            .fetch_sub(1, Ordering::AcqRel);
        self.connection.pending_changed.notify_waiters();
    }
}

impl Drop for ArtifactDecodeLease {
    fn drop(&mut self) {
        self.connection
            .artifact_leases
            .fetch_sub(1, Ordering::AcqRel);
        self.connection.artifact_leases_changed.notify_waiters();
        self.connection.pending_changed.notify_waiters();
    }
}

#[derive(Default)]
pub(super) struct RequestTombstones {
    pub(super) entries: VecDeque<(u64, Instant)>,
}

impl RequestTombstones {
    pub(super) fn insert(&mut self, id: u64, ttl: Duration) {
        self.purge();
        self.remove(id);
        while self.entries.len() >= MAX_TOMBSTONES {
            self.entries.pop_front();
        }
        self.entries.push_back((
            id,
            Instant::now().checked_add(ttl).unwrap_or_else(Instant::now),
        ));
    }

    pub(super) fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(entry_id, _)| *entry_id != id);
        before != self.entries.len()
    }

    pub(super) fn contains(&mut self, id: u64) -> bool {
        self.purge();
        self.entries.iter().any(|(entry_id, _)| *entry_id == id)
    }

    pub(super) fn purge(&mut self) {
        let now = Instant::now();
        self.entries.retain(|(_, expires)| *expires > now);
    }
}

pub(super) struct ConnectionHealth {
    pub(super) state: ExtensionHealthState,
    pub(super) last_error: Option<String>,
}

pub(super) fn update_health(
    health: &StdRwLock<ConnectionHealth>,
    state: ExtensionHealthState,
    error: Option<String>,
) {
    let mut health = write_std_lock(health);
    health.state = state;
    if let Some(mut error) = error {
        truncate_utf8(&mut error, MAX_LIFECYCLE_REASON_BYTES);
        health.last_error = Some(error);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_protocol_writer(
    mut stdin: ChildStdin,
    mut frames: mpsc::Receiver<WriterFrame>,
    closed: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    pending: PendingRequests,
    pending_changed: Arc<Notify>,
    remote_ui: Arc<RemoteUiMailbox>,
    health: Arc<StdRwLock<ConnectionHealth>>,
    events: broadcast::Sender<ExtensionEvent>,
    child: Arc<Mutex<Child>>,
    termination: ProcessTerminationHandle,
    frame_limit: Arc<ProtocolFrameLimit>,
) {
    while let Some(mut frame) = frames.recv().await {
        if frame
            .bus_delivery
            .as_ref()
            .is_some_and(|delivery| !delivery.is_current() && !delivery.control_expired())
        {
            continue;
        }
        if frame
            .state
            .compare_exchange(
                FRAME_QUEUED,
                FRAME_WRITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            if let Some(completion) = frame.completion.take() {
                let _ = completion.send(Err(PendingError::Cancelled(
                    "frame cancelled before write".into(),
                )));
            }
            continue;
        }

        if !frame_limit.accepts_message_bytes(frame.line.len()) {
            let message = format!(
                "outbound message exceeded negotiated {} byte limit",
                frame_limit.max_message_bytes()
            );
            closed.store(true, Ordering::Release);
            remote_ui.clear();
            update_health(
                &health,
                ExtensionHealthState::Crashed,
                Some(message.clone()),
            );
            let error = PendingError::Protocol(message.clone());
            if let Some(completion) = frame.completion.take() {
                let _ = completion.send(Err(error.clone()));
            }
            fail_all_pending(&pending, &pending_changed, error);
            let _ = events.send(ExtensionEvent::Diagnostic { message });
            if !draining.load(Ordering::Acquire) {
                reap_failed_extension(child, termination).await;
            }
            return;
        }

        let write = async {
            stdin
                .write_all(&frame.line)
                .await
                .map_err(|error| error.to_string())?;
            stdin.flush().await.map_err(|error| error.to_string())
        };
        let result = match &frame.bus_delivery {
            Some(delivery) => delivery.guard_write(write).await,
            None => write.await,
        };
        match result {
            Ok(()) => {
                frame.state.store(FRAME_WRITTEN, Ordering::Release);
                if let Some(completion) = frame.completion.take() {
                    let _ = completion.send(Ok(()));
                }
            }
            Err(message) => {
                closed.store(true, Ordering::Release);
                remote_ui.clear();
                update_health(
                    &health,
                    ExtensionHealthState::Crashed,
                    Some(format!("stdin write failed: {message}")),
                );
                let error = PendingError::Closed(format!("stdin write failed: {message}"));
                if let Some(completion) = frame.completion.take() {
                    let _ = completion.send(Err(error.clone()));
                }
                fail_all_pending(&pending, &pending_changed, error);
                let _ = events.send(ExtensionEvent::Diagnostic {
                    message: format!("extension stdin write failed: {message}"),
                });
                if !draining.load(Ordering::Acquire) {
                    reap_failed_extension(child, termination).await;
                }
                return;
            }
        }
    }

    remote_ui.clear();
    if !closed.swap(true, Ordering::AcqRel) {
        let coordinated = draining.load(Ordering::Acquire);
        let state = if coordinated {
            ExtensionHealthState::Stopped
        } else {
            ExtensionHealthState::Crashed
        };
        update_health(
            &health,
            state,
            (!draining.load(Ordering::Acquire)).then(|| "extension writer closed".into()),
        );
        fail_all_pending(
            &pending,
            &pending_changed,
            PendingError::Closed("extension writer closed".into()),
        );
        if !coordinated {
            reap_failed_extension(child, termination).await;
        }
    }
}

pub(super) async fn reap_failed_extension(
    child: Arc<Mutex<Child>>,
    termination: ProcessTerminationHandle,
) {
    termination.terminate();
    let mut child = child.lock().await;
    if tokio::time::timeout(DEFAULT_SHUTDOWN_TIMEOUT, child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

impl ProcessConnection {
    pub(super) fn max_message_bytes(&self) -> usize {
        self.frame_limit.max_message_bytes()
    }

    pub(super) fn max_frame_bytes(&self) -> usize {
        self.frame_limit.max_frame_bytes()
    }

    pub(super) fn acquire_artifact_lease(self: &Arc<Self>) -> ArtifactDecodeLease {
        self.artifact_leases.fetch_add(1, Ordering::AcqRel);
        ArtifactDecodeLease {
            connection: Arc::clone(self),
        }
    }

    pub(super) fn settle_artifacts(&self) {
        if !self.artifacts_settled.swap(true, Ordering::AcqRel) {
            let _ = self.artifact_store.settle_generation(self.generation);
        }
    }

    pub(super) fn remove_provider_owner(&self) {
        if !self.provider_owner_removed.swap(true, Ordering::AcqRel) {
            if let Some(bus) = &self.event_bus {
                bus.remove(&self.provider_owner.extension_instance_id, self.generation);
            }
            if let Some(registry) = &self.provider_registry {
                registry.remove_owner(&self.provider_owner);
            }
        }
    }

    pub(super) fn activate_post_initialize(&self) {
        self.initialization_complete.store(true, Ordering::Release);
        self.initialization_changed.notify_waiters();
    }

    pub(super) fn cancel_provider_stream(&self, stream_id: &str, reason: &str) {
        let removed = lock_std_mutex(&self.provider_streams).remove(stream_id);
        if removed.is_some()
            && !self.closed.load(Ordering::Acquire)
            && self
                .require_api_v03_host_method(methods::PROVIDER_CANCEL)
                .is_ok()
        {
            let _ = self.queue_notification(
                methods::PROVIDER_CANCEL,
                serde_json::json!({"stream_id": stream_id, "reason": reason}),
            );
        }
    }

    pub(super) fn cancel_all_provider_streams(&self, reason: &str) {
        let ids = lock_std_mutex(&self.provider_streams)
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for stream_id in ids {
            self.cancel_provider_stream(&stream_id, reason);
        }
    }

    pub(super) fn settle_provider_stream(&self, stream_id: &str) {
        lock_std_mutex(&self.provider_streams).remove(stream_id);
    }

    pub(super) async fn request(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        self.request_inner(
            method, params, timeout, true, true, None, None, None, None, None, None,
        )
        .await
    }

    pub(super) async fn request_with_resource_owner(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        resource_owner: Option<ExtensionResourceOwner>,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        self.request_inner(
            method,
            params,
            timeout,
            true,
            true,
            None,
            None,
            None,
            resource_owner,
            None,
            None,
        )
        .await
    }

    pub(super) async fn request_with_operation(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        resource_owner: Option<ExtensionResourceOwner>,
        request_started: oneshot::Sender<ExtensionOperationToken>,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        self.request_inner(
            method,
            params,
            timeout,
            true,
            true,
            None,
            None,
            None,
            resource_owner,
            Some(request_started),
            None,
        )
        .await
    }

    /// Runs a cancellable command with ordinary progress, while preserving the
    /// command frontend as the owner of extension confirmation and input.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request_with_command_progress(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        cancellation: CancellationToken,
        progress: ToolProgressSink,
        resource_owner: Option<ExtensionResourceOwner>,
        request_started: oneshot::Sender<ExtensionOperationToken>,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        self.request_inner(
            method,
            params,
            timeout,
            true,
            true,
            Some(cancellation),
            Some(progress),
            None,
            resource_owner,
            Some(request_started),
            None,
        )
        .await
    }

    pub(super) async fn request_during_shutdown(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        self.request_inner(
            method, params, timeout, false, false, None, None, None, None, None, None,
        )
        .await
    }

    /// Sends one host-owned lifecycle finalizer while a generation is draining.
    /// It deliberately bypasses ordinary request admission so an accepted
    /// replacement can settle the old binding before the old child exits.
    pub(super) async fn request_lifecycle(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        resource_owner: ExtensionResourceOwner,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        self.request_inner(
            method,
            params,
            timeout,
            false,
            false,
            None,
            None,
            None,
            Some(resource_owner),
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request_tool(
        self: &Arc<Self>,
        definition: ToolDefinition,
        params: serde_json::Value,
        timeout: Duration,
        owner: Option<ExtensionResourceOwner>,
        cancellation: Option<CancellationToken>,
        progress: Option<ToolProgressSink>,
        request_started: Option<oneshot::Sender<ExtensionOperationToken>>,
        policy: Option<DynamicToolRegistration>,
    ) -> Result<ToolCallOutput, ExtensionRuntimeError> {
        let admission = Arc::new(ToolResultAdmission {
            definition,
            policy,
            output: StdMutex::new(None),
        });
        self.request_inner(
            methods::TOOL_CALL,
            params,
            timeout,
            true,
            true,
            cancellation,
            progress.clone(),
            progress,
            owner,
            request_started,
            Some(Arc::clone(&admission)),
        )
        .await?;
        let output = lock_std_mutex(&admission.output)
            .take()
            .expect("successful tool result admitted");
        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request_inner(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        cancel_on_host_shutdown: bool,
        use_request_slot: bool,
        cancellation: Option<CancellationToken>,
        progress: Option<ToolProgressSink>,
        child_interaction_progress: Option<ToolProgressSink>,
        resource_owner: Option<ExtensionResourceOwner>,
        request_started: Option<oneshot::Sender<ExtensionOperationToken>>,
        tool_admission: Option<Arc<ToolResultAdmission>>,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeError::Closed("stdout is closed".into()));
        }
        if use_request_slot && self.draining.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeError::Closed(
                "extension generation is draining".into(),
            ));
        }

        let deadline = Instant::now() + timeout;
        let admission_cancellation = cancellation.clone();
        let resource_enabled =
            read_std_lock(&self.protocol).supports(EXTENSION_FEATURE_RESOURCE_REFS_V1);
        let bulk_enabled =
            read_std_lock(&self.protocol).supports(EXTENSION_FEATURE_BULK_OBJECTS_V1);
        let bulk_inputs = if bulk_enabled && tool_admission.is_some() {
            Some(collect_blob_refs(&params["arguments"])?)
        } else {
            None
        };
        let resource_epoch = lock_std_mutex(&self.resources).retirement_epoch;
        let resource_inputs = if resource_enabled {
            tool_admission
                .as_ref()
                .map(|a| operation_inputs(&a.definition, &params["arguments"]))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if resource_enabled
            && resource_owner.is_none()
            && tool_admission
                .as_ref()
                .and_then(|a| a.definition.operation.as_ref())
                .is_some_and(|o| !o.resource_inputs.is_empty() || !o.resource_outputs.is_empty())
        {
            return Err(resource_error("resource_unavailable"));
        }
        if resource_owner.is_none() && bulk_inputs.as_ref().is_some_and(|refs| !refs.is_empty()) {
            return Err(resource_error("blob_unavailable"));
        }
        let cancellation_reason = Arc::new(StdMutex::new("request dropped".to_owned()));
        let operation_reason = Arc::clone(&cancellation_reason);
        let connection = Arc::clone(self);
        let operation = async move {
            let _admission = if use_request_slot {
                Some(connection.acquire_request_admission()?)
            } else {
                None
            };
            let _slot =
                if use_request_slot {
                    let slots = read_std_lock(&connection.slots).clone();
                    Some(slots.acquire_owned().await.map_err(|_| {
                        ExtensionRuntimeError::Closed("request queue is closed".into())
                    })?)
                } else {
                    None
                };
            if use_request_slot && connection.draining.load(Ordering::Acquire) {
                return Err(ExtensionRuntimeError::Closed(
                    "extension generation is draining".into(),
                ));
            }
            let mut params = params;
            session_leaf::attach_request_session_mirror(
                &connection,
                resource_owner.as_ref(),
                &mut params,
            )?;
            let id = connection.next_id.fetch_add(1, Ordering::Relaxed);
            let message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            });
            let line = connection.serialize_message(&message)?;
            // Capacity waits hold no pins. Reserve the writer before the final,
            // synchronous liveness/catalog recheck and exclusive admission.
            let writer = connection
                .writer
                .reserve()
                .await
                .map_err(|_| ExtensionRuntimeError::Closed("extension writer closed".into()))?;
            if connection.closed.load(Ordering::Acquire)
                || (use_request_slot && connection.draining.load(Ordering::Acquire))
            {
                return Err(resource_error("resource_unavailable"));
            }
            if resource_enabled || bulk_enabled {
                if let Some(admission) = &tool_admission {
                    // Policy is live; definition/handler/revision stay frozen.
                    if admission.definition.operation.is_some()
                        && admission.policy.as_ref().is_some_and(|policy| {
                            !policy.permits_operation(&admission.definition.name)
                        })
                    {
                        return Err(ExtensionRuntimeError::Protocol(
                            "operation execution policy denied".into(),
                        ));
                    }
                    if let Some(owner) = &resource_owner {
                        lock_std_mutex(&connection.resources).admit(
                            id,
                            owner.clone(),
                            resource_epoch,
                            &resource_inputs,
                            bulk_inputs.as_deref(),
                            &admission.definition,
                            &message["params"]["arguments"],
                        )?;
                    }
                }
            }
            let (reply_tx, reply_rx) = oneshot::channel();
            let terminal = Arc::new(AtomicU8::new(REQUEST_ACTIVE));
            let frame_state = Arc::new(AtomicU8::new(FRAME_QUEUED));
            let cancellation_sent = Arc::new(AtomicBool::new(false));
            if let Some(owner) = &resource_owner {
                lock_std_mutex(&connection.issued_resource_owners).insert(owner.clone());
            }
            lock_std_mutex(&connection.pending).insert(
                id,
                PendingRequest {
                    method: method.to_owned(),
                    sender: reply_tx,
                    terminal,
                    frame_state: Arc::clone(&frame_state),
                    cancellation_sent,
                    progress,
                    child_interaction_progress,
                    resource_owner: resource_owner.clone(),
                    last_progress_sequence: None,
                    composition_files: Arc::new(CompositionFiles::default()),
                    tool_call_policy_digest: (method == methods::TOOL_CALL)
                        .then(|| {
                            Some(tool_call_policy_digest(
                                message["params"]["name"].as_str()?,
                                message["params"].get("arguments")?,
                            ))
                        })
                        .flatten(),
                },
            );
            if let Some(request_started) = request_started {
                let _ = request_started.send(ExtensionOperationToken {
                    generation: connection.generation,
                    parent_request_id: id,
                });
            }
            let mut registration =
                PendingRegistration::new(&connection, id, Arc::clone(&operation_reason));
            writer.send(WriterFrame {
                line,
                state: frame_state,
                completion: None,
                bus_delivery: None,
            });

            let reply = match reply_rx.await {
                Ok(reply) => reply,
                Err(_) => Err(PendingError::Closed("response channel closed".into())),
            };
            // Receiving a terminal is execution settlement, NOT result admission.
            // Complete decoding precedes the shared owner/cancellation disposition gate.
            let reply = reply.map_err(|error| pending_error(error, method))?;
            if let Some(admission) = &tool_admission {
                let output = decode_tool_call_output(
                    &connection,
                    &admission.definition,
                    resource_owner.as_ref().map(|o| o.session_id.as_str()),
                    reply.clone(),
                )?;
                let outputs = if resource_enabled {
                    operation_outputs(&admission.definition, &output)?
                } else {
                    Vec::new()
                };
                let blobs = if bulk_enabled {
                    let blobs = collect_blob_refs(
                        output
                            .structured_content
                            .as_ref()
                            .unwrap_or(&serde_json::Value::Null),
                    )?;
                    if !collect_blob_refs(&output.metadata)?.is_empty()
                        || (!blobs.is_empty()
                            && (output.is_error || admission.definition.output_schema.is_none()))
                    {
                        return Err(resource_error("blob_unavailable"));
                    }
                    let storage = lock_std_mutex(&connection.resources)
                        .bulk
                        .clone()
                        .expect("negotiated bulk store");
                    let transfer = storage.lock().transfer_directory().to_owned();
                    validate_no_bulk_locators(&reply, &transfer)?;
                    blobs
                } else {
                    Vec::new()
                };
                let diagnostics = diagnostic_blob_ids(
                    &connection,
                    resource_owner.as_ref().map(|o| o.session_id.as_str()),
                    &output.metadata,
                )?;
                if !diagnostics.is_empty() && !bulk_enabled {
                    return Err(resource_error("unsupported_feature"));
                }
                #[cfg(test)]
                {
                    let barrier = {
                        lock_std_mutex(&connection.resources)
                            .before_result_admission
                            .take()
                    };
                    if let Some(barrier) = barrier {
                        barrier.pause().await;
                    }
                }
                if resource_enabled || bulk_enabled {
                    if resource_owner.is_some() {
                        let mut registry = lock_std_mutex(&connection.resources);
                        if admission_cancellation
                            .as_ref()
                            .is_some_and(|c| c.is_cancelled())
                        {
                            return Err(ExtensionRuntimeError::Cancelled {
                                method: method.to_owned(),
                                reason: "user".into(),
                            });
                        }
                        if Instant::now() >= deadline {
                            return Err(ExtensionRuntimeError::Timeout {
                                method: method.to_owned(),
                            });
                        }
                        admit_reference_outputs(
                            &mut registry,
                            id,
                            &outputs,
                            bulk_enabled.then_some(blobs.as_slice()),
                            &diagnostics,
                            output.is_error,
                        )?;
                        let _ = connection.events.send(ExtensionEvent::Diagnostic {
                            message: format!(
                                "reference publication {}: request={id}, resources={}, blobs={}",
                                if output.is_error {
                                    "abandoned"
                                } else {
                                    "committed"
                                },
                                outputs.len(),
                                blobs.len()
                            ),
                        });
                    } else if !outputs.is_empty() || !blobs.is_empty() || !diagnostics.is_empty() {
                        return Err(resource_error(if !outputs.is_empty() {
                            "resource_unavailable"
                        } else {
                            "blob_unavailable"
                        }));
                    }
                    connection.resource_cleanup_changed.notify_one();
                }
                *lock_std_mutex(&admission.output) = Some(output);
            }
            registration.disarm();
            // Pending cancellation carries a reason, while this future retains
            // the admitted JSON-RPC method. Keep that provenance on shutdown or
            // reload just as on the direct cancellation and timeout paths.
            Ok(reply)
        };
        tokio::pin!(operation);
        let timed = tokio::time::timeout(timeout, &mut operation);
        tokio::pin!(timed);

        let wait_for_cancellation = async {
            if let Some(cancellation) = cancellation {
                cancellation.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(wait_for_cancellation);

        if cancel_on_host_shutdown {
            tokio::select! {
                biased;
                _ = host_shutdown_requested() => {
                    *lock_std_mutex(&cancellation_reason) = "host shutdown".into();
                    Err(ExtensionRuntimeError::Closed("host is shutting down".into()))
                },
                _ = &mut wait_for_cancellation => {
                    *lock_std_mutex(&cancellation_reason) = "user".into();
                    Err(ExtensionRuntimeError::Cancelled {
                        method: method.to_owned(),
                        reason: "user".into(),
                    })
                },
                result = &mut timed => match result {
                    Ok(result) => result,
                    Err(_) => {
                        *lock_std_mutex(&cancellation_reason) = "timeout".into();
                        Err(ExtensionRuntimeError::Timeout { method: method.to_owned() })
                    }
                },
            }
        } else {
            match timed.await {
                Ok(result) => result,
                Err(_) => {
                    *lock_std_mutex(&cancellation_reason) = "timeout".into();
                    Err(ExtensionRuntimeError::Timeout {
                        method: method.to_owned(),
                    })
                }
            }
        }
    }

    pub(super) fn require_api_v03_host_method(
        &self,
        method: &str,
    ) -> Result<(), ExtensionRuntimeError> {
        if !is_canonical_api(&read_std_lock(&self.protocol).version) {
            return Ok(());
        }
        let contract = read_std_lock(&self.api_v03_contract)
            .clone()
            .ok_or_else(|| {
                ExtensionRuntimeError::Protocol(
                    "API 0.3 contract is unavailable before initialization".into(),
                )
            })?;
        api_v03::require_method(&contract, method, api_v03::MethodDirection::HostToExtension)
            .map_err(api_v03_protocol_error)
    }

    pub(super) fn serialize_message(
        &self,
        message: &serde_json::Value,
    ) -> Result<Vec<u8>, ExtensionRuntimeError> {
        let is_api_v03 = is_canonical_api(&read_std_lock(&self.protocol).version);
        let mut line = if is_api_v03 {
            api_v03::parse_json_rpc_envelope(message.clone()).map_err(api_v03_protocol_error)?;
            api_v03::canonical_frame(message, self.max_frame_bytes())
                .map_err(api_v03_protocol_error)?
                .into_bytes()
        } else {
            serde_json::to_vec(message)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?
        };
        line.push(b'\n');
        let max_message_bytes = self.max_message_bytes();
        if line.len() > max_message_bytes {
            return Err(ExtensionRuntimeError::MessageTooLarge {
                limit: max_message_bytes,
            });
        }
        Ok(line)
    }

    pub(super) fn queue_notification(&self, method: &str, params: serde_json::Value) -> bool {
        if is_canonical_api(&read_std_lock(&self.protocol).version)
            && method == methods::CANCEL_REQUEST
            && api_v03::parse_cancel_request_params(params.clone()).is_err()
        {
            return false;
        }
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let Ok(line) = self.serialize_message(&message) else {
            return false;
        };
        let queued = self
            .writer
            .try_send(WriterFrame {
                line,
                state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                completion: None,
                bus_delivery: None,
            })
            .is_ok();
        if !queued {
            update_health(
                &self.health,
                ExtensionHealthState::Degraded,
                Some(format!(
                    "bounded writer queue rejected `{method}` notification"
                )),
            );
            let _ = self.events.send(ExtensionEvent::Diagnostic {
                message: format!("dropped `{method}` because the extension writer queue is full"),
            });
        }
        queued
    }

    pub(super) fn cancel_request(self: &Arc<Self>, id: u64, reason: &str) {
        let request = {
            let mut pending = lock_std_mutex(&self.pending);
            if lock_std_mutex(&self.resources).cancel_parent(id) {
                let _ = self.events.send(ExtensionEvent::Diagnostic { message: format!("resource parent abandoned: request={id}; cancellation requested, execution independently tracked") });
            }
            self.resource_cleanup_changed.notify_one();
            let Some(request) = pending.get(&id) else {
                return;
            };
            if request
                .terminal
                .compare_exchange(
                    REQUEST_ACTIVE,
                    REQUEST_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                return;
            }
            let request = pending.remove(&id);
            // Checkpoint commit holds pending through native mutation and ACK
            // admission. Publish retirement before releasing that disposition;
            // there must be no cancelled-terminal/current-surface window.
            lock_std_mutex(&self.tombstones).insert(id, self.tombstone_ttl);
            self.remote_ui.settle_parent(id, true);
            if read_std_lock(&self.protocol).supports(EXTENSION_FEATURE_REMOTE_UI) {
                if let Some(owner) = request
                    .as_ref()
                    .and_then(|request| request.resource_owner.as_ref())
                {
                    lock_std_mutex(&self.issued_resource_owners).remove(owner);
                    self.remote_ui.discard_owner(owner);
                }
            }
            request
        };
        let Some(request) = request else {
            return;
        };
        self.pending_changed.notify_waiters();
        let _ = request
            .sender
            .send(Err(PendingError::Cancelled(reason.to_owned())));
        self.cancel_children(id, reason);

        let frame_was_admitted = request
            .frame_state
            .compare_exchange(
                FRAME_QUEUED,
                FRAME_SKIPPED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err();
        if !frame_was_admitted {
            lock_std_mutex(&self.resources).settle_execution(id);
            self.resource_cleanup_changed.notify_one();
            lock_std_mutex(&self.tombstones).remove(id);
        }
        let cancellation_supported = read_std_lock(&self.protocol).supports("request_cancellation");
        if frame_was_admitted
            && cancellation_supported
            && !request.cancellation_sent.swap(true, Ordering::AcqRel)
        {
            let _ = self.queue_notification(
                methods::CANCEL_REQUEST,
                serde_json::json!({"id": id, "reason": reason}),
            );
            self.schedule_cancellation_escalation(id);
        } else if frame_was_admitted && !cancellation_supported {
            self.schedule_cancellation_escalation(id);
        }
    }

    pub(super) fn cancel_children(&self, parent_request_id: u64, reason: &str) {
        let child_ids = cancel_active_children(&self.child_requests, parent_request_id, reason);
        for id in child_ids {
            let _ = self.queue_notification(
                methods::CANCEL_REQUEST,
                serde_json::json!({"id": id, "reason": reason}),
            );
        }
    }

    pub(super) fn schedule_cancellation_escalation(self: &Arc<Self>, id: u64) {
        let connection = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(connection.cancellation_grace).await;
            let unresolved = lock_std_mutex(&connection.tombstones).contains(id)
                || lock_std_mutex(&connection.resources).execution_pending(id);
            if unresolved && !connection.closed.load(Ordering::Acquire) {
                update_health(
                    &connection.health,
                    ExtensionHealthState::Degraded,
                    Some(format!(
                        "request {id} did not acknowledge cancellation within {:?}",
                        connection.cancellation_grace
                    )),
                );
                connection.terminate().await;
            }
        });
    }

    pub(super) fn cancel_all_pending(self: &Arc<Self>, reason: &str) {
        let ids = lock_std_mutex(&self.pending)
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel_request(id, reason);
        }
    }

    /// Synchronous editor-only mutation/ACK disposition. Reserve capacity before
    /// claiming anything, then hold pending -> children -> surface through commit.
    /// Cancellation and retirement take these same locks, never the reverse.
    pub(super) fn commit_editor_checkpoint(
        &self,
        id: &ExtensionRequestId,
        owner: &ExtensionResourceOwner,
        checkpoint: &ExtensionEditorCheckpoint,
        commit: impl FnOnce() -> Result<(), (ExtensionRequestFailure, String)>,
    ) -> Result<(), (ExtensionRequestFailure, String)> {
        let stale = || {
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "editor checkpoint disposition is no longer current".to_owned(),
            )
        };
        checkpoint.validate()?;
        let response = serde_json::json!({
            "jsonrpc":"2.0", "id":id, "result":{
                "input_revision":checkpoint.input_revision,
                "checkpoint_revision":checkpoint.checkpoint_revision,
            }
        });
        // Match send_child_envelope_admitted's version-specific wire path; do
        // not silently apply another API version's envelope/number restrictions.
        let mut line = if is_canonical_api(&read_std_lock(&self.protocol).version) {
            api_v03::parse_json_rpc_envelope(response.clone())
                .map_err(|error| (ExtensionRequestFailure::InvalidRequest, error.to_string()))?;
            api_v03::canonical_json(&response)
                .map_err(|error| (ExtensionRequestFailure::InvalidRequest, error.to_string()))?
                .into_bytes()
        } else {
            serde_json::to_vec(&response).expect("editor checkpoint acknowledgement is JSON")
        };
        line.push(b'\n');
        if line.len() > self.max_message_bytes() {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "editor checkpoint acknowledgement exceeds the transport bound".into(),
            ));
        }
        let permit = self.writer.try_reserve().map_err(|error| {
            (
                ExtensionRequestFailure::InvalidRequest,
                format!("editor checkpoint writer admission unavailable: {error}"),
            )
        })?;
        let pending = lock_std_mutex(&self.pending);
        if !lock_std_mutex(&self.issued_resource_owners).contains(owner) {
            return Err(stale());
        }
        let mut children = lock_std_mutex(&self.child_requests);
        let child = children.get(id).ok_or_else(stale)?;
        if child.parent_request_id != 0
            && pending
                .get(&child.parent_request_id)
                .is_none_or(|parent| parent.terminal.load(Ordering::Acquire) != REQUEST_ACTIVE)
        {
            return Err(stale());
        }
        let response_state = Arc::clone(&child.response_state);
        response_state
            .state
            .compare_exchange(
                CHILD_ACTIVE,
                CHILD_RESPONDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| stale())?;
        let mut claim = ChildResponseClaim {
            child_requests: Arc::clone(&self.child_requests),
            id: id.clone(),
            response_state,
            admitted: false,
            abort_cancel: Some((self.writer.clone(), Arc::clone(&self.frame_limit))),
        };
        let result = self
            .remote_ui
            .with_editor_checkpoint(owner, checkpoint, || {
                if self.closed.load(Ordering::Acquire) || self.draining.load(Ordering::Acquire) {
                    return Err(stale());
                }
                // The callback is only local shell validation/mutation: no await,
                // process/mailbox reentry, or IO. Nothing fallible follows mutation.
                commit()?;
                claim.mark_admitted_with_children(&mut children);
                permit.send(WriterFrame {
                    line,
                    state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                    completion: None,
                    bus_delivery: None,
                });
                Ok(())
            });
        drop(children);
        // Failed validation restores the existing claim only after its map lock
        // is released; true parent cancellation remains excluded until then.
        drop(claim);
        drop(pending);
        result
    }

    pub(super) async fn send_child_response<T: Serialize + ?Sized>(
        &self,
        id: ExtensionRequestId,
        result: &T,
    ) -> Result<(), ExtensionRuntimeError> {
        self.send_child_response_admitted(id, result)
            .await
            .map(|_| ())
    }

    pub(super) async fn send_child_response_admitted<T: Serialize + ?Sized>(
        &self,
        id: ExtensionRequestId,
        result: &T,
    ) -> Result<ChildResponseAdmission, ExtensionRuntimeError> {
        self.send_child_envelope_admitted(id, ChildEnvelope::Success(result))
            .await
    }

    /// Answers one process-originated request with a typed contract failure.
    pub(super) async fn send_child_error_response(
        &self,
        id: ExtensionRequestId,
        code: i64,
        message: String,
    ) -> Result<(), ExtensionRuntimeError> {
        self.send_child_envelope_admitted::<()>(id, ChildEnvelope::Error { code, message })
            .await
            .map(|_| ())
    }

    pub(super) async fn send_child_envelope_admitted<T: Serialize + ?Sized>(
        &self,
        id: ExtensionRequestId,
        envelope: ChildEnvelope<'_, T>,
    ) -> Result<ChildResponseAdmission, ExtensionRuntimeError> {
        let response = match envelope {
            ChildEnvelope::Success(result) => serde_json::to_value(ChildSuccessResponse {
                jsonrpc: "2.0",
                id: &id,
                result,
            })
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            ChildEnvelope::Error { code, message } => serde_json::to_value(ChildErrorResponse {
                jsonrpc: "2.0",
                id: &id,
                error: ChildErrorObject { code, message },
            })
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
        };
        let mut line = if is_canonical_api(&read_std_lock(&self.protocol).version) {
            api_v03::parse_json_rpc_envelope(response.clone()).map_err(api_v03_protocol_error)?;
            api_v03::canonical_json(&response)
                .map_err(api_v03_protocol_error)?
                .into_bytes()
        } else {
            serde_json::to_vec(&response)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?
        };
        line.push(b'\n');
        let max_message_bytes = self.max_message_bytes();
        if line.len() > max_message_bytes {
            line.fill(0);
            return Err(ExtensionRuntimeError::MessageTooLarge {
                limit: max_message_bytes,
            });
        }
        let line = ZeroizingBytes(line);
        loop {
            let (response_state, remote_ui_response) = {
                let children = lock_std_mutex(&self.child_requests);
                let Some(child) = children.get(&id) else {
                    return Ok(ChildResponseAdmission::AlreadySettled);
                };
                let remote_ui_response = child
                    .remote_ui
                    .as_ref()
                    .map(|request| request.prepare_response(response.get("result")))
                    .transpose()
                    .map_err(ExtensionRuntimeError::Protocol)?
                    .flatten();
                (Arc::clone(&child.response_state), remote_ui_response)
            };
            match response_state.state.compare_exchange(
                CHILD_ACTIVE,
                CHILD_RESPONDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let mut claim = ChildResponseClaim {
                        child_requests: Arc::clone(&self.child_requests),
                        id: id.clone(),
                        response_state,
                        admitted: false,
                        abort_cancel: Some((self.writer.clone(), Arc::clone(&self.frame_limit))),
                    };
                    let (completed, completion) = oneshot::channel();
                    let admission = self.writer.reserve();
                    tokio::pin!(admission);
                    let permit = tokio::select! {
                        biased;
                        _ = host_shutdown_requested() => return Err(
                            ExtensionRuntimeError::Closed("host is shutting down".into())
                        ),
                        result = tokio::time::timeout(
                            CONFIRMATION_RESPONSE_TIMEOUT,
                            &mut admission,
                        ) => result.map_err(|_| ExtensionRuntimeError::Timeout {
                            method: "extension/response admission".to_owned(),
                        })?.map_err(|_| ExtensionRuntimeError::Closed(
                            "extension writer closed".into()
                        ))?,
                    };
                    // Install remote UI geometry before making the acknowledgement
                    // visible to the child. A reserved slot lets this commit and
                    // terminal admission run together without an await or RPC.
                    if let Some(response) = remote_ui_response {
                        response.commit().map_err(ExtensionRuntimeError::Protocol)?;
                    }
                    // Writer admission is the sole terminal outcome boundary:
                    // after this non-awaiting step cancellation cannot enqueue
                    // a competing $/cancelRequest for the same child request.
                    claim.mark_admitted();
                    permit.send(WriterFrame {
                        line: line.0.clone(),
                        state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                        completion: Some(completed),
                        bus_delivery: None,
                    });
                    let completed = tokio::select! {
                        biased;
                        _ = host_shutdown_requested() => return Err(
                            ExtensionRuntimeError::Closed("host is shutting down".into())
                        ),
                        result = tokio::time::timeout(
                            CONFIRMATION_RESPONSE_TIMEOUT,
                            completion,
                        ) => result.map_err(|_| ExtensionRuntimeError::Timeout {
                            method: "extension/response write".to_owned(),
                        })?,
                    };
                    completed
                        .map_err(|_| {
                            ExtensionRuntimeError::Closed("extension writer closed".into())
                        })?
                        .map_err(|error| pending_error(error, "request"))?;
                    return Ok(ChildResponseAdmission::Queued);
                }
                Err(CHILD_RESPONDING) => {
                    let changed = response_state.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if response_state.state.load(Ordering::Acquire) == CHILD_RESPONDING {
                        changed.await;
                    }
                }
                Err(CHILD_SETTLED) => return Ok(ChildResponseAdmission::AlreadySettled),
                Err(_) => continue,
            }
        }
    }

    pub(super) fn begin_drain(&self) -> bool {
        if self.draining.swap(true, Ordering::AcqRel) {
            return false;
        }
        read_std_lock(&self.slots).close();
        self.session_leaf.clear();
        self.remote_ui.clear();
        update_health(&self.health, ExtensionHealthState::Draining, None);
        true
    }

    pub(super) fn resume_after_failed_drain(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let limit = read_std_lock(&self.protocol).max_concurrent_requests;
        *write_std_lock(&self.slots) = Arc::new(Semaphore::new(limit));
        self.draining.store(false, Ordering::Release);
        update_health(&self.health, ExtensionHealthState::Ready, None);
        true
    }

    pub(super) fn acquire_request_admission(
        self: &Arc<Self>,
    ) -> Result<RequestAdmissionLease, ExtensionRuntimeError> {
        if self.draining.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeError::Closed(
                "extension generation is draining".into(),
            ));
        }
        self.active_admissions.fetch_add(1, Ordering::AcqRel);
        if self.draining.load(Ordering::Acquire) {
            self.active_admissions.fetch_sub(1, Ordering::AcqRel);
            self.pending_changed.notify_waiters();
            return Err(ExtensionRuntimeError::Closed(
                "extension generation is draining".into(),
            ));
        }
        Ok(RequestAdmissionLease {
            connection: Arc::clone(self),
        })
    }

    pub(super) async fn drain(
        self: &Arc<Self>,
        deadline: Duration,
        cancellation_reason: &str,
    ) -> bool {
        let settled = self.quiesce(deadline).await;
        if !settled {
            self.cancel_all_pending(cancellation_reason);
        } else {
            self.settle_artifacts();
        }
        settled
    }

    pub(super) async fn quiesce(self: &Arc<Self>, deadline: Duration) -> bool {
        self.begin_drain();
        tokio::time::timeout(deadline, async {
            loop {
                let changed = self.pending_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if lock_std_mutex(&self.pending).is_empty()
                    && !lock_std_mutex(&self.resources).has_executions()
                    && self.active_admissions.load(Ordering::Acquire) == 0
                    && self.artifact_leases.load(Ordering::Acquire) == 0
                {
                    break;
                }
                changed.await;
            }
        })
        .await
        .is_ok()
    }

    pub(super) async fn shutdown(self: &Arc<Self>) -> bool {
        lock_std_mutex(&self.resources).retire_generation();
        self.begin_drain();
        self.cancel_all_pending("shutdown");
        self.cancel_all_provider_streams("shutdown");
        self.remove_provider_owner();
        let quiescent = tokio::time::timeout(self.shutdown_timeout, async {
            loop {
                let changed = self.artifact_leases_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.artifact_leases.load(Ordering::Acquire) == 0 {
                    break;
                }
                changed.await;
            }
        })
        .await
        .is_ok();
        let acknowledged = if self.closed.load(Ordering::Acquire) {
            false
        } else {
            let is_api_v03 = is_canonical_api(&read_std_lock(&self.protocol).version);
            let params = if is_api_v03 {
                let params = api_v03::ShutdownParams {};
                if api_v03::validate_shutdown_params(&params).is_err() {
                    return false;
                }
                match serde_json::to_value(params) {
                    Ok(value) => value,
                    Err(_) => return false,
                }
            } else {
                serde_json::json!({})
            };
            match self
                .request_during_shutdown(methods::SHUTDOWN, params, self.shutdown_timeout)
                .await
            {
                Ok(value) if is_api_v03 => api_v03::parse_shutdown_result(value)
                    .and_then(|result| {
                        api_v03::validate_shutdown_result(&result)?;
                        Ok(())
                    })
                    .is_ok(),
                Ok(_) => true,
                Err(_) => false,
            }
        };

        let exited = {
            let mut child = self.child.lock().await;
            match tokio::time::timeout(self.shutdown_timeout, child.wait()).await {
                Ok(Ok(_)) => {
                    self.process_group.terminate_now();
                    true
                }
                Ok(Err(_)) => {
                    self.kill_process_group();
                    false
                }
                Err(_) => {
                    self.kill_process_group();
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    false
                }
            }
        };
        self.closed.store(true, Ordering::Release);
        lock_std_mutex(&self.resources).terminate_generation();
        self.resource_cleanup_changed.notify_one();
        update_health(&self.health, ExtensionHealthState::Stopped, None);
        if quiescent {
            self.settle_artifacts();
        }
        acknowledged && exited
    }

    pub(super) async fn terminate(&self) {
        lock_std_mutex(&self.resources).retire_generation();
        self.draining.store(true, Ordering::Release);
        self.session_leaf.clear();
        self.remote_ui.clear();
        lock_std_mutex(&self.child_requests).clear();
        self.cancel_all_provider_streams("terminated");
        self.remove_provider_owner();
        self.kill_process_group();
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
        self.closed.store(true, Ordering::Release);
        lock_std_mutex(&self.resources).terminate_generation();
        self.resource_cleanup_changed.notify_one();
        let current = read_std_lock(&self.health).state;
        if !matches!(
            current,
            ExtensionHealthState::Degraded
                | ExtensionHealthState::Crashed
                | ExtensionHealthState::Parked
        ) {
            update_health(&self.health, ExtensionHealthState::Stopped, None);
        }
    }

    pub(super) fn kill_process_group(&self) {
        self.process_group.terminate_now();
    }
}

impl Drop for ProcessConnection {
    fn drop(&mut self) {
        self.session_leaf.clear();
        self.remote_ui.clear();
        lock_std_mutex(&self.child_requests).clear();
        self.remove_provider_owner();
        lock_std_mutex(&self.provider_streams).clear();
        self.process_group.terminate_now();
        self.settle_artifacts();
    }
}
