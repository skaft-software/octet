//! Typed producer for an explicitly host-bound private session append leaf.
//!
//! The independent reader routes an explicitly host-bound append directly to
//! the queue, bypassing foreground event broadcast, and retains each receipt
//! through its known commit/failure. No new capability is advertised. This
//! module gives the reader queue admission only, never a Session or file writer.

use serde::{Deserialize, Serialize};

use super::ExtensionResourceOwner;
use super::*;
use crate::session_leaf::{
    SessionLeafBinding, SessionLeafCommit, SessionLeafError, SessionLeafGrant, SessionLeafProducer,
    SessionLeafReceipt, SessionLeafRevoker,
};

/// Exact append-only private request. No path, namespace or mutable host object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLeafAppendRequest {
    /// Single-use opaque host token.
    pub grant_id: String,
    /// Host activation fence from the owned hook snapshot.
    pub activation_epoch: u64,
    /// Host operation identity from that snapshot, never a retry authority.
    pub operation_id: String,
    /// Private extension-owned type.
    pub entry_type: String,
    /// Inert JSON under the existing durable private-entry bounds.
    pub data: serde_json::Value,
}

/// Bounded grant snapshot sent only on the private process channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLeafGrantSnapshot {
    /// Opaque single-use host token; never log it or send it to a provider.
    pub grant_id: String,
    /// Immutable host activation epoch.
    pub activation_epoch: u64,
    /// Immutable host operation identity.
    pub operation_id: String,
    /// Complete owner fence, for retained-context routing, not caller authority.
    pub owner: ExtensionResourceOwner,
    /// Expected parent entry; None means the empty root.
    pub expected_head: Option<String>,
}

impl From<&SessionLeafGrant> for SessionLeafGrantSnapshot {
    fn from(grant: &SessionLeafGrant) -> Self {
        Self {
            grant_id: grant.id().to_owned(),
            activation_epoch: grant.binding().activation_epoch,
            operation_id: grant.binding().operation_id.clone(),
            owner: grant.binding().owner.clone(),
            expected_head: grant.expected_head().map(|id| id.0.clone()),
        }
    }
}

/// Known durable result. Never construct it from queue admission or a timeout.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLeafAppendResult {
    /// Entry already committed by the sole Session writer.
    pub entry_id: String,
    /// Head already published by that same Session.
    pub head: String,
    /// Successor at the known new head; None if the activation was revoked.
    pub successor: Option<SessionLeafGrantSnapshot>,
}

impl From<SessionLeafCommit> for SessionLeafAppendResult {
    fn from(commit: SessionLeafCommit) -> Self {
        Self {
            entry_id: commit.entry_id.0,
            head: commit.head.0,
            successor: commit
                .successor
                .as_ref()
                .map(SessionLeafGrantSnapshot::from),
        }
    }
}

/// One immutable host-installed reader binding. Replacement must revoke the old
/// core consumer first; neither owner nor grant is retargeted to a new process.
#[derive(Clone)]
pub struct SessionLeafProcessProducer {
    producer: SessionLeafProducer,
    binding: SessionLeafBinding,
}

impl SessionLeafProcessProducer {
    /// Bind only to the authority captured before the owned hook is dispatched.
    pub fn new(producer: SessionLeafProducer, binding: SessionLeafBinding) -> Self {
        Self { producer, binding }
    }

    /// Direct lossless queue admission from the independent process reader.
    /// `owner` and `generation` must come from current host admission/connection,
    /// not from this request's JSON. Caller must settle its child-request slot
    /// from the receipt, respecting cancellation versus the commit claim.
    pub fn try_append(
        &self,
        owner: &ExtensionResourceOwner,
        generation: u64,
        request: SessionLeafAppendRequest,
    ) -> Result<SessionLeafReceipt, SessionLeafError> {
        if owner != &self.binding.owner
            || generation != self.binding.owner.process_generation
            || request.activation_epoch != self.binding.activation_epoch
            || request.operation_id != self.binding.operation_id
        {
            return Err(SessionLeafError::StaleBinding);
        }
        if request.grant_id.len() != 64
            || !request
                .grant_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SessionLeafError::StaleGrant);
        }
        self.producer.try_append(
            &request.grant_id,
            &self.binding,
            request.entry_type,
            request.data,
        )
    }
}

/// Private authorization added only to explicitly bound append requests.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLeafAppendAuthorization {
    /// Single-use host token.
    pub grant_id: String,
    /// Host activation fence.
    pub activation_epoch: u64,
    /// Host operation identity.
    pub operation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeafWireAppend {
    parent_request_id: u64,
    #[serde(default)]
    resource_owner: Option<ExtensionResourceOwner>,
    entry_type: String,
    data: serde_json::Value,
    session_leaf: SessionLeafAppendAuthorization,
}

impl OwnerScopedHostRequest for LeafWireAppend {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        self.resource_owner.as_ref()
    }
}

struct BoundLeaf {
    producer: SessionLeafProcessProducer,
    revoker: SessionLeafRevoker,
    snapshot: SessionLeafGrantSnapshot,
}

#[derive(Default)]
pub(super) struct SessionLeafMailbox {
    bound: StdMutex<Option<Arc<BoundLeaf>>>,
}

impl SessionLeafMailbox {
    pub(super) fn clear(&self) {
        if let Some(bound) = lock_std_mutex(&self.bound).take() {
            bound.revoker.revoke();
        }
    }
}

/// Host-bound, pinned-process activation. Drop revokes outstanding grants and
/// settles queued receipts; claimed commits retain their real outcome.
pub struct SessionLeafProcessLease {
    process: ExtensionProcess,
    connection: Arc<ProcessConnection>,
    bound: Arc<BoundLeaf>,
}

impl ExtensionProcess {
    /// Install the direct producer on the actual current reader before hook
    /// dispatch. No capability is negotiated here. Only a host-issued grant for
    /// this process instance/generation and manifest namespace can bind.
    pub fn bind_session_leaf(
        &self,
        producer: SessionLeafProducer,
        revoker: SessionLeafRevoker,
        grant: SessionLeafGrant,
    ) -> Result<SessionLeafProcessLease, ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let binding = grant.binding();
        if binding.owner.extension_instance_id != self.inner.instance_id
            || binding.owner.process_generation != connection.generation
            || binding.namespace != self.inner.descriptor.manifest.name
            || connection.closed.load(Ordering::Acquire)
            || connection.draining.load(Ordering::Acquire)
            || !read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_SESSION_ENTRIES)
        {
            return Err(ExtensionRuntimeError::Protocol(
                "private session leaf cannot bind this process authority".into(),
            ));
        }
        let bound = Arc::new(BoundLeaf {
            producer: SessionLeafProcessProducer::new(producer, binding.clone()),
            revoker,
            snapshot: SessionLeafGrantSnapshot::from(&grant),
        });
        let mut slot = lock_std_mutex(&connection.session_leaf.bound);
        if slot.is_some() {
            return Err(ExtensionRuntimeError::Protocol(
                "private session leaf activation already bound".into(),
            ));
        }
        *slot = Some(Arc::clone(&bound));
        drop(slot);
        Ok(SessionLeafProcessLease {
            process: self.clone(),
            connection,
            bound,
        })
    }
}

impl SessionLeafProcessLease {
    /// Initial immutable grant snapshot for the owned hook request.
    pub fn grant_snapshot(&self) -> &SessionLeafGrantSnapshot {
        &self.bound.snapshot
    }

    /// Owned, borrow-free existing `hook/run` future on the pinned connection.
    /// Adds top-level `session_leaf` to that request. This does not invent a new
    /// hook kind; the hook must already be actually declared and implemented.
    pub fn run_hook(
        self,
        hook: ExtensionHook,
        payload: serde_json::Value,
        context: ExtensionExecutionContext,
    ) -> impl std::future::Future<Output = Result<ExtensionHookOutput, ExtensionRuntimeError>>
           + Send
           + 'static {
        async move {
            if hook.is_session_hook()
                || !self.process.inner.contributions.hooks.contains(&hook)
                || context.resource_owner.as_ref() != Some(&self.bound.snapshot.owner)
                || !lock_std_mutex(&self.connection.session_leaf.bound)
                    .as_ref()
                    .is_some_and(|bound| Arc::ptr_eq(bound, &self.bound))
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "private session leaf hook authority changed or hook unavailable".into(),
                ));
            }
            let mut params = serde_json::to_value(HookRequest {
                hook,
                payload,
                context,
            })
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
            params["session_leaf"] = serde_json::to_value(&self.bound.snapshot)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
            self.process
                .request_typed_on_connection(
                    Arc::clone(&self.connection),
                    methods::HOOK_RUN,
                    &params,
                    Some(self.bound.snapshot.owner.clone()),
                )
                .await
        }
    }
}

impl Drop for SessionLeafProcessLease {
    fn drop(&mut self) {
        self.bound.revoker.revoke();
        let mut slot = lock_std_mutex(&self.connection.session_leaf.bound);
        if slot
            .as_ref()
            .is_some_and(|bound| Arc::ptr_eq(bound, &self.bound))
        {
            slot.take();
        }
    }
}

pub(super) fn cancel_leaf_child(state: &ProtocolReadState, id: &ExtensionRequestId) -> bool {
    let cancellation = lock_std_mutex(&state.child_requests)
        .get(id)
        .and_then(|child| lock_std_mutex(&child.response_state.session_leaf_cancel).clone());
    if let Some(cancellation) = cancellation {
        cancellation.cancel();
        true
    } else {
        false
    }
}

fn refusal(error: SessionLeafError) -> (i64, &'static str) {
    match error {
        SessionLeafError::Cancelled => (
            JSON_RPC_REQUEST_CANCELLED,
            "session_leaf_cancelled_before_commit",
        ),
        SessionLeafError::Full => (-32002, "session_leaf_queue_full"),
        SessionLeafError::InvalidPayload => (-32602, "session_leaf_invalid_payload"),
        SessionLeafError::Persistence => (-32002, "session_leaf_persistence_failed_reconcile"),
        _ => (-32002, "session_leaf_authority_refused"),
    }
}

pub(super) fn dispatch_append_request(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<bool, String> {
    let bound = lock_std_mutex(&state.session_leaf.bound).clone();
    if bound.is_none() && params.get("session_leaf").is_none() {
        return Ok(false);
    }
    let id = parse_child_request_id(object, methods::SESSION_APPEND_ENTRY)?;
    let Some(bound) = bound else {
        reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::UnsupportedFeature,
            "private session leaf is not bound",
        )?;
        return Ok(true);
    };
    validate_remote_ui_envelope(object, true)
        .map_err(|_| "invalid private session leaf envelope".to_owned())?;
    let Some((request, admitted)) = admit_host_request::<LeafWireAppend>(
        state,
        object,
        methods::SESSION_APPEND_ENTRY,
        EXTENSION_FEATURE_SESSION_ENTRIES,
        params,
    )?
    else {
        return Ok(true);
    };
    if state.closed.load(Ordering::Acquire) || state.draining.load(Ordering::Acquire) {
        refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "private session leaf generation closed".into(),
            ),
        )?;
        return Ok(true);
    }
    let receipt = {
        // Serialize child cancellation against installing its leaf cancellation
        // handle. This uses the same children -> leaf lock order as cancellation.
        let children = lock_std_mutex(&state.child_requests);
        let Some(child) = children.get(&admitted.request_id) else {
            return Ok(true);
        };
        if child.response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE {
            return Ok(true);
        }
        let result = bound.producer.try_append(
            &admitted.owner,
            state.generation,
            SessionLeafAppendRequest {
                grant_id: request.session_leaf.grant_id,
                activation_epoch: request.session_leaf.activation_epoch,
                operation_id: request.session_leaf.operation_id,
                entry_type: request.entry_type,
                data: request.data,
            },
        );
        match result {
            Ok(receipt) => {
                *lock_std_mutex(&child.response_state.session_leaf_cancel) =
                    Some(receipt.cancellation());
                receipt
            }
            Err(error) => {
                drop(children);
                let (code, message) = refusal(error);
                try_queue_child_response(
                    &state.child_requests,
                    &admitted.request_id,
                    &state.writer,
                    state.max_message_bytes(),
                    serde_json::json!({"jsonrpc":"2.0","id":admitted.request_id,
                        "error":{"code":code,"message":message}}),
                )?;
                return Ok(true);
            }
        }
    };
    let writer = state.writer.clone();
    let children = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let closed = Arc::clone(&state.closed);
    let termination = state.termination.clone();
    let max_bytes = state.max_message_bytes();
    let id = admitted.request_id;
    tokio::spawn(async move {
        // Never select parent cancellation/timeout against this waiter: before
        // claim cancellation yields a definite error; after claim it must finish.
        let result = receipt.wait().await;
        let response = match result {
            Ok(commit) => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":SessionLeafAppendResult::from(commit)})
            }
            Err(error) => {
                let (code, message) = refusal(error);
                serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        let delivered = async {
            let mut line = serde_json::to_vec(&response).map_err(|_| ())?;
            line.push(b'\n');
            if line.len() > max_bytes {
                return Err(());
            }
            let permit = tokio::time::timeout(CONFIRMATION_RESPONSE_TIMEOUT, writer.reserve())
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?;
            let response_state = lock_std_mutex(&children)
                .get(&id)
                .map(|child| Arc::clone(&child.response_state));
            let Some(response_state) = response_state else {
                return Err(());
            };
            let mut claim = ChildResponseClaim {
                child_requests: Arc::clone(&children),
                id: id.clone(),
                response_state,
                admitted: false,
                abort_cancel: None,
            };
            claim.mark_admitted();
            permit.send(WriterFrame {
                line,
                state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                completion: None,
                bus_delivery: None,
            });
            Ok(())
        }
        .await;
        if delivered.is_err() {
            // Lost reply is ambiguous, never a zero-write result or replay grant.
            bound.revoker.revoke();
            closed.store(true, Ordering::Release);
            update_health(
                &health,
                ExtensionHealthState::Degraded,
                Some("private session leaf reply lost; reconciliation required".into()),
            );
            settle_child_request(&children, &id);
            if let Some(termination) = termination {
                termination.terminate();
            }
        }
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::session_leaf::{SessionLeafCancellation, SessionLeafConsumer};
    use serde_json::json;

    fn fixture(session: &Session) -> SessionLeafBinding {
        SessionLeafBinding {
            activation_epoch: 10,
            owner: ExtensionResourceOwner {
                session_id: session.resource_owner_key(),
                extension_instance_id: "process-leaf-test".into(),
                process_generation: 4,
            },
            namespace: "octet.test".into(),
            operation_id: "projection:10".into(),
        }
    }

    fn reader_binding(
        binding: &SessionLeafBinding,
        producer: SessionLeafProducer,
        revoker: SessionLeafRevoker,
        grant: &SessionLeafGrant,
    ) -> (
        ProtocolReadState,
        mpsc::Receiver<WriterFrame>,
        broadcast::Receiver<ExtensionEvent>,
    ) {
        let (events, received) = broadcast::channel(8);
        let (mut state, frames) = super::super::tests::protocol_read_state_for_test(
            ManifestContributions::default(),
            events,
        );
        state.instance_id = binding.owner.extension_instance_id.clone();
        state.generation = binding.owner.process_generation;
        {
            let mut protocol = write_std_lock(&state.protocol);
            protocol.version = EXTENSION_API_VERSION_0_4.into();
            protocol
                .features
                .insert(EXTENSION_FEATURE_SESSION_ENTRIES.into());
        }
        *lock_std_mutex(&state.session_leaf.bound) = Some(Arc::new(BoundLeaf {
            producer: SessionLeafProcessProducer::new(producer, binding.clone()),
            revoker,
            snapshot: SessionLeafGrantSnapshot::from(grant),
        }));
        super::super::tests::insert_test_parent(&state, 1, Some(binding.owner.clone()));
        (state, frames, received)
    }

    fn wire_request(grant: &SessionLeafGrant, id: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":"session/append_entry",
            "params":{"parent_request_id":1,"entry_type":"checkpoint","data":{"revision":1},
                "session_leaf":{"grant_id":grant.id(),"activation_epoch":grant.binding().activation_epoch,
                    "operation_id":grant.binding().operation_id}}})).unwrap()
    }

    async fn reply(frames: &mut mpsc::Receiver<WriterFrame>) -> serde_json::Value {
        let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .unwrap()
            .unwrap();
        serde_json::from_slice(&frame.line).unwrap()
    }

    #[tokio::test]
    async fn actual_protocol_reader_queues_directly_without_broadcast_or_optimistic_ack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut session = Session::create(&path).unwrap();
        let binding = fixture(&session);
        let (mut consumer, producer, grant) =
            SessionLeafConsumer::new(&session, binding.clone()).unwrap();
        let (state, mut frames, mut events) =
            reader_binding(&binding, producer, consumer.revoker(), &grant);
        handle_protocol_line(&wire_request(&grant, 100), &state).unwrap();
        assert!(frames.try_recv().is_err());
        assert!(events.try_recv().is_err());
        assert!(consumer.ready().await);
        assert!(session.entries().is_empty());
        consumer.consume_next(&mut session, &binding).unwrap();
        let response = reply(&mut frames).await;
        assert_eq!(response["result"]["entry_id"], session.head().unwrap().0);
        assert_eq!(response["result"]["head"], session.head().unwrap().0);
        assert_eq!(
            response["result"]["successor"]["expected_head"],
            session.head().unwrap().0
        );
        assert!(lock_std_mutex(&state.child_requests).is_empty());
        assert!(events.try_recv().is_err());
        let head = session.head();
        drop(consumer);
        drop(session);
        assert_eq!(Session::open(&path).unwrap().head(), head);
    }

    #[tokio::test]
    async fn child_and_parent_cancellation_before_claim_reply_zero_write() {
        for parent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
            let binding = fixture(&session);
            let (mut consumer, producer, grant) =
                SessionLeafConsumer::new(&session, binding.clone()).unwrap();
            let (state, mut frames, _) =
                reader_binding(&binding, producer, consumer.revoker(), &grant);
            handle_protocol_line(&wire_request(&grant, 100), &state).unwrap();
            if parent {
                cancel_children_from_reader(&state, 1, "parent cancelled");
            } else {
                handle_protocol_line(
                    br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":100}}"#,
                    &state,
                )
                .unwrap();
            }
            let response = reply(&mut frames).await;
            assert_eq!(response["error"]["code"], JSON_RPC_REQUEST_CANCELLED);
            assert_eq!(
                response["error"]["message"],
                "session_leaf_cancelled_before_commit"
            );
            assert!(!consumer.consume_next(&mut session, &binding).unwrap());
            assert!(session.entries().is_empty());
            assert!(lock_std_mutex(&state.child_requests).is_empty());
        }
    }

    #[tokio::test]
    async fn child_parent_and_revocation_after_claim_wait_for_actual_commit() {
        for cancel in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
            let binding = fixture(&session);
            let (mut consumer, producer, grant) =
                SessionLeafConsumer::new(&session, binding.clone()).unwrap();
            let (state, mut frames, _) =
                reader_binding(&binding, producer, consumer.revoker(), &grant);
            handle_protocol_line(&wire_request(&grant, 100), &state).unwrap();
            consumer
                .consume_with_claim_for_test(&mut session, &binding, || {
                    match cancel {
                        0 => handle_protocol_line(
                            br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":100}}"#,
                            &state,
                        )
                        .unwrap(),
                        1 => cancel_children_from_reader(&state, 1, "parent cancelled"),
                        _ => state.session_leaf.clear(),
                    }
                    assert!(frames.try_recv().is_err());
                    assert_eq!(lock_std_mutex(&state.child_requests).len(), 1);
                })
                .unwrap();
            let response = reply(&mut frames).await;
            assert_eq!(response["result"]["head"], session.head().unwrap().0);
            if cancel == 2 {
                assert!(response["result"]["successor"].is_null());
            }
            assert!(frames.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn unbound_missing_foreign_and_replayed_leaf_requests_fail_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let binding = fixture(&session);
        let (consumer, producer, grant) =
            SessionLeafConsumer::new(&session, binding.clone()).unwrap();
        let (state, mut frames, mut events) =
            reader_binding(&binding, producer, consumer.revoker(), &grant);
        let mut missing: serde_json::Value =
            serde_json::from_slice(&wire_request(&grant, 100)).unwrap();
        missing["params"]
            .as_object_mut()
            .unwrap()
            .remove("session_leaf");
        handle_protocol_line(&serde_json::to_vec(&missing).unwrap(), &state).unwrap();
        assert!(reply(&mut frames).await.get("error").is_some());
        let mut foreign: serde_json::Value =
            serde_json::from_slice(&wire_request(&grant, 101)).unwrap();
        foreign["params"]["session_leaf"]["activation_epoch"] = json!(99);
        handle_protocol_line(&serde_json::to_vec(&foreign).unwrap(), &state).unwrap();
        assert_eq!(
            reply(&mut frames).await["error"]["message"],
            "session_leaf_authority_refused"
        );
        handle_protocol_line(&wire_request(&grant, 102), &state).unwrap();
        handle_protocol_line(&wire_request(&grant, 103), &state).unwrap();
        assert_eq!(
            reply(&mut frames).await["error"]["message"],
            "session_leaf_authority_refused"
        );
        state.session_leaf.clear();
        assert!(reply(&mut frames).await.get("error").is_some());
        handle_protocol_line(&wire_request(&grant, 104), &state).unwrap();
        assert!(reply(&mut frames).await.get("error").is_some());
        assert!(events.try_recv().is_err());
        assert!(session.entries().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn lost_writer_reply_is_fail_closed_not_noncommit_or_replay() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let binding = fixture(&session);
        let (mut consumer, producer, grant) =
            SessionLeafConsumer::new(&session, binding.clone()).unwrap();
        let (state, mut frames, _) = reader_binding(&binding, producer, consumer.revoker(), &grant);
        for _ in 0..8 {
            queue_writer_value(&state.writer, &state.frame_limit, json!({"padding":true})).unwrap();
        }
        handle_protocol_line(&wire_request(&grant, 100), &state).unwrap();
        consumer.consume_next(&mut session, &binding).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(CONFIRMATION_RESPONSE_TIMEOUT + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(state.closed.load(Ordering::Acquire));
        assert_eq!(session.entries().len(), 1);
        assert_eq!(
            read_std_lock(&state.health).state,
            ExtensionHealthState::Degraded
        );
        assert!(lock_std_mutex(&state.child_requests).is_empty());
        for _ in 0..8 {
            assert!(frames.try_recv().is_ok());
        }
        assert!(frames.try_recv().is_err());
        assert_eq!(
            consumer.issue_grant(&session).err(),
            Some(SessionLeafError::Revoked)
        );
    }

    fn request(grant: &SessionLeafGrant) -> SessionLeafAppendRequest {
        SessionLeafAppendRequest {
            grant_id: grant.id().into(),
            activation_epoch: grant.binding().activation_epoch,
            operation_id: grant.binding().operation_id.clone(),
            entry_type: "private-checkpoint".into(),
            data: json!({"revision":2}),
        }
    }

    #[tokio::test]
    async fn producer_ack_is_only_authoritative_commit_and_successor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut session = Session::create(&path).unwrap();
        let binding = fixture(&session);
        let (mut consumer, producer, grant) =
            SessionLeafConsumer::new(&session, binding.clone()).unwrap();
        let producer = SessionLeafProcessProducer::new(producer, binding.clone());
        let receipt = producer
            .try_append(&binding.owner, 4, request(&grant))
            .unwrap();
        let reply = receipt.wait();
        tokio::pin!(reply);
        assert!(matches!(
            futures_util::poll!(&mut reply),
            std::task::Poll::Pending
        ));
        assert!(session.entries().is_empty());
        consumer.consume_next(&mut session, &binding).unwrap();
        let result = SessionLeafAppendResult::from(reply.await.unwrap());
        assert_eq!(result.entry_id, session.head().unwrap().0);
        assert_eq!(result.head, result.entry_id);
        let successor = result.successor.unwrap();
        assert_eq!(
            successor.expected_head.as_deref(),
            Some(result.entry_id.as_str())
        );
        assert_eq!(successor.owner, binding.owner);
        assert_eq!(successor.activation_epoch, 10);
        assert_ne!(successor.grant_id, grant.id());
        assert_eq!(
            producer
                .try_append(&binding.owner, 4, request(&grant))
                .err(),
            Some(SessionLeafError::StaleGrant)
        );
        drop(consumer);
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.head().unwrap().0, result.entry_id);
    }

    #[test]
    fn strict_append_shape_has_no_namespace_path_or_other_operations() {
        let valid = json!({"grant_id":"a".repeat(64),"activation_epoch":1,
            "operation_id":"projection:1","entry_type":"state","data":{}});
        assert!(serde_json::from_value::<SessionLeafAppendRequest>(valid.clone()).is_ok());
        for field in [
            "namespace",
            "path",
            "resource_owner",
            "head",
            "operation",
            "agent",
        ] {
            let mut invalid = valid.clone();
            invalid[field] = json!("forged");
            assert!(serde_json::from_value::<SessionLeafAppendRequest>(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn current_process_owner_generation_activation_and_operation_are_fenced() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let binding = fixture(&session);
        let (consumer, producer, grant) =
            SessionLeafConsumer::new(&session, binding.clone()).unwrap();
        let producer = SessionLeafProcessProducer::new(producer, binding.clone());
        for field in 0..7 {
            let mut owner = binding.owner.clone();
            let mut generation = 4;
            let mut request = request(&grant);
            match field {
                0 => owner.session_id = "foreign".into(),
                1 => owner.extension_instance_id = "foreign".into(),
                2 => owner.process_generation += 1,
                3 => generation += 1,
                4 => request.activation_epoch += 1,
                5 => request.operation_id = "other-operation".into(),
                _ => request.grant_id = "forged".into(),
            }
            let expected = if field == 6 {
                SessionLeafError::StaleGrant
            } else {
                SessionLeafError::StaleBinding
            };
            assert_eq!(
                producer.try_append(&owner, generation, request).err(),
                Some(expected)
            );
        }
        let receipt = producer
            .try_append(&binding.owner, 4, request(&grant))
            .unwrap();
        assert_eq!(
            receipt.cancellation().cancel(),
            SessionLeafCancellation::Prevented
        );
        assert_eq!(
            receipt.wait().await.unwrap_err(),
            SessionLeafError::Cancelled
        );
        consumer.revoker().revoke();
        assert!(session.entries().is_empty());
    }

    #[tokio::test]
    async fn head_switch_activation_aba_rejects_retained_old_calls() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let old_binding = fixture(&session);
        let (old_consumer, old_producer, old_grant) =
            SessionLeafConsumer::new(&session, old_binding.clone()).unwrap();
        let old_producer = SessionLeafProcessProducer::new(old_producer, old_binding.clone());
        old_consumer.revoker().revoke();
        let mut next_binding = old_binding.clone();
        next_binding.activation_epoch += 2; // A -> B -> A, same session + process
        let (_new_consumer, new_producer, _new_grant) =
            SessionLeafConsumer::new(&session, next_binding.clone()).unwrap();
        let new_producer = SessionLeafProcessProducer::new(new_producer, next_binding.clone());
        assert_eq!(
            old_producer
                .try_append(&old_binding.owner, 4, request(&old_grant))
                .err(),
            Some(SessionLeafError::Revoked)
        );
        assert_eq!(
            new_producer
                .try_append(&old_binding.owner, 4, request(&old_grant))
                .err(),
            Some(SessionLeafError::StaleBinding)
        );
        let mut retargeted = request(&old_grant);
        retargeted.activation_epoch = next_binding.activation_epoch;
        assert_eq!(
            new_producer
                .try_append(&next_binding.owner, 4, retargeted)
                .err(),
            Some(SessionLeafError::StaleGrant)
        );
        assert!(session.entries().is_empty());
    }
}
