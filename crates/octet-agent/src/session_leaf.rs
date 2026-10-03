//! Bounded append-only reverse requests serviced by the owning session driver.
//!
//! This is an integration primitive, not a negotiated process capability. Before
//! dispatching a hook, the host must install its producer and select an owned
//! [`SessionLeafConsumer::ready`] future alongside the borrow-free hook future.
//! Only that driver supplies `&mut Session`; producers never receive a writer.
//! A receipt acknowledges only the existing synced session append, not admission.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;
use tokio::sync::{oneshot, Notify};

use crate::extension_process::ExtensionResourceOwner;
use crate::session::{
    is_valid_extension_metadata_namespace, EntryId, Session,
    MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES, MAX_EXTENSION_ENTRY_TYPE_BYTES,
};
use crate::tools::deferred::DeferredRunStore;

/// Maximum admitted requests, including a claimed commit.
pub const MAX_SESSION_LEAF_REQUESTS: usize = 8;
/// Maximum aggregate encoded payload bytes, including a claimed commit.
pub const MAX_SESSION_LEAF_BYTES: usize = 64 * 1024;
/// Maximum unused grants in one activation.
pub const MAX_SESSION_LEAF_GRANTS: usize = 16;

/// Host-selected authority for one hook activation. Never inferred from a caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLeafBinding {
    /// Monotonic activation fence, changed even when returning to an old owner.
    pub activation_epoch: u64,
    /// Actual session namespace plus process instance and generation.
    pub owner: ExtensionResourceOwner,
    /// Registered namespace; the extension cannot choose its provenance.
    pub namespace: String,
    /// Host operation identity, not an extension retry key.
    pub operation_id: String,
}

/// Immutable single-use append grant. Debug output deliberately hides its token.
#[derive(Clone)]
pub struct SessionLeafGrant {
    id: String,
    binding: Arc<SessionLeafBinding>,
    head: Option<EntryId>,
    file_bytes: u64,
}

impl std::fmt::Debug for SessionLeafGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLeafGrant")
            .field("head", &self.head)
            .finish_non_exhaustive()
    }
}

impl SessionLeafGrant {
    /// Opaque private-transport token; never log or put into provider context.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The immutable host binding this grant authorizes.
    pub fn binding(&self) -> &SessionLeafBinding {
        &self.binding
    }

    /// Expected parent of the one authorized append.
    pub fn expected_head(&self) -> Option<&EntryId> {
        self.head.as_ref()
    }
}

/// Known durable outcome, produced only after sync and in-memory publication.
#[derive(Debug)]
pub struct SessionLeafCommit {
    /// Durably committed entry.
    pub entry_id: EntryId,
    /// Published active head (the same entry for this append-only operation).
    pub head: EntryId,
    /// Fresh grant at the known new head; absent if revoked or unavailable.
    pub successor: Option<SessionLeafGrant>,
}

/// Static, payload-free refusal. Persistence failure is never retry authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionLeafError {
    /// Authority was revoked before claim.
    #[error("session leaf revoked")]
    Revoked,
    /// Cancellation won before claim; no session write was attempted.
    #[error("session leaf cancelled before commit")]
    Cancelled,
    /// An unknown, foreign, consumed or replayed grant.
    #[error("session leaf grant is stale or foreign")]
    StaleGrant,
    /// Current host owner, epoch, operation or process fence differs.
    #[error("session leaf binding changed")]
    StaleBinding,
    /// The supplied Session is not the original writer incarnation.
    #[error("session leaf session changed")]
    StaleSession,
    /// Head or append revision changed outside the known append chain.
    #[error("session leaf head or revision changed")]
    StaleHead,
    /// The payload exceeds the existing private-entry contract.
    #[error("session leaf payload is invalid or oversized")]
    InvalidPayload,
    /// Count, aggregate bytes or unused-grant capacity is exhausted.
    #[error("session leaf queue is full")]
    Full,
    /// Append/sync failed; possibly partial, lane closed, never retry blindly.
    #[error("session leaf persistence failed; reconciliation required")]
    Persistence,
    /// The owning consumer closed without claiming the request.
    #[error("session leaf consumer closed")]
    Closed,
}

/// Result of cancelling, ordered atomically against the commit claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionLeafCancellation {
    /// Cancellation settled the receipt, without any write.
    Prevented,
    /// Commit already claimed; await the receipt for the real outcome.
    Claimed,
    /// Receipt was already settled (possibly committed).
    Settled,
}

struct Pending {
    bytes: usize,
    claimed: bool,
    reply: oneshot::Sender<Result<SessionLeafCommit, SessionLeafError>>,
}

struct Request {
    id: u64,
    grant: SessionLeafGrant,
    entry_type: String,
    data: Value,
}

#[derive(Default)]
struct State {
    terminal: Option<SessionLeafError>,
    grants: HashMap<String, SessionLeafGrant>,
    queue: VecDeque<Request>,
    pending: HashMap<u64, Pending>,
    bytes: usize,
    next_request: u64,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Notify,
}

impl Shared {
    fn settle(state: &mut State, id: u64, result: Result<SessionLeafCommit, SessionLeafError>) {
        if let Some(pending) = state.pending.remove(&id) {
            state.bytes -= pending.bytes;
            // A lost reply does not undo a commit or restore its consumed grant.
            let _ = pending.reply.send(result);
        }
    }

    fn close(&self, error: SessionLeafError) {
        let mut state = self.state.lock().expect("session leaf mutex poisoned");
        state.terminal.get_or_insert(error);
        state.grants.clear();
        while let Some(request) = state.queue.pop_front() {
            Self::settle(&mut state, request.id, Err(error));
        }
        // Claimed requests remain charged and settle after their actual write.
        self.ready.notify_waiters();
    }
}

/// Cloneable queue admission only; contains no Session, Agent or file handle.
#[derive(Clone)]
pub struct SessionLeafProducer {
    shared: Arc<Shared>,
}

impl SessionLeafProducer {
    /// Nonblocking admission. Success here is a pending receipt, NOT persistence.
    /// Saturation refuses without consuming the grant; accepted grants are burned
    /// even if later cancelled, stale, failed, or their reply is lost.
    pub fn try_append(
        &self,
        grant_id: &str,
        binding: &SessionLeafBinding,
        entry_type: String,
        data: Value,
    ) -> Result<SessionLeafReceipt, SessionLeafError> {
        let bytes = payload_bytes(&entry_type, &data)?;
        let mut state = self
            .shared
            .state
            .lock()
            .expect("session leaf mutex poisoned");
        if let Some(error) = state.terminal {
            return Err(error);
        }
        let grant = state
            .grants
            .get(grant_id)
            .ok_or(SessionLeafError::StaleGrant)?;
        if grant.binding.as_ref() != binding {
            return Err(SessionLeafError::StaleBinding);
        }
        if state.pending.len() == MAX_SESSION_LEAF_REQUESTS
            || state.bytes + bytes > MAX_SESSION_LEAF_BYTES
        {
            return Err(SessionLeafError::Full);
        }
        let next = state
            .next_request
            .checked_add(1)
            .ok_or(SessionLeafError::Full)?;
        let grant = state.grants.remove(grant_id).expect("validated grant");
        let id = state.next_request;
        state.next_request = next;
        let (reply, receiver) = oneshot::channel();
        state.pending.insert(
            id,
            Pending {
                bytes,
                claimed: false,
                reply,
            },
        );
        state.bytes += bytes;
        state.queue.push_back(Request {
            id,
            grant,
            entry_type,
            data,
        });
        self.shared.ready.notify_one();
        Ok(SessionLeafReceipt {
            receiver,
            cancellation: SessionLeafCancelHandle {
                shared: Arc::clone(&self.shared),
                id,
            },
        })
    }
}

/// Cancellation authority for one admitted request, not a session mutation.
#[derive(Clone)]
pub struct SessionLeafCancelHandle {
    shared: Arc<Shared>,
    id: u64,
}

impl SessionLeafCancelHandle {
    /// Atomically cancel before claim; after claim await the original receipt.
    pub fn cancel(&self) -> SessionLeafCancellation {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("session leaf mutex poisoned");
        let Some(pending) = state.pending.get(&self.id) else {
            return SessionLeafCancellation::Settled;
        };
        if pending.claimed {
            return SessionLeafCancellation::Claimed;
        }
        state.queue.retain(|request| request.id != self.id);
        Shared::settle(&mut state, self.id, Err(SessionLeafError::Cancelled));
        SessionLeafCancellation::Prevented
    }
}

/// Pending receipt. Dropping it does NOT imply noncommit and never enables replay.
pub struct SessionLeafReceipt {
    receiver: oneshot::Receiver<Result<SessionLeafCommit, SessionLeafError>>,
    cancellation: SessionLeafCancelHandle,
}

impl SessionLeafReceipt {
    /// Obtain a cancellation handle without giving away the commit waiter.
    pub fn cancellation(&self) -> SessionLeafCancelHandle {
        self.cancellation.clone()
    }

    /// Await known commit or failure, including after cancellation lost the claim.
    pub async fn wait(self) -> Result<SessionLeafCommit, SessionLeafError> {
        self.receiver.await.unwrap_or(Err(SessionLeafError::Closed))
    }
}

/// Host-only revocation handle. Retire it on owner/process/operation changes.
#[derive(Clone)]
pub struct SessionLeafRevoker {
    shared: Arc<Shared>,
}

impl SessionLeafRevoker {
    /// Settle all unclaimed receipts; claimed work waits for its actual result.
    pub fn revoke(&self) {
        self.shared.close(SessionLeafError::Revoked);
    }
}

/// Sole consumer, serviced only by the driver owning the original mutable Session.
pub struct SessionLeafConsumer {
    shared: Arc<Shared>,
    binding: Arc<SessionLeafBinding>,
    // Each Session constructor allocates a distinct store tied to its writer.
    // This Weak is an identity witness only: no producer has a journal handle,
    // and no second writer or descriptor is opened or retained by this channel.
    incarnation: Weak<DeferredRunStore>,
}

impl SessionLeafConsumer {
    /// Create a bound activation BEFORE dispatching its hook. Host identity and
    /// namespace are checked here; an owning driver still revalidates at claim.
    pub fn new(
        session: &Session,
        binding: SessionLeafBinding,
    ) -> Result<(Self, SessionLeafProducer, SessionLeafGrant), SessionLeafError> {
        if binding.owner.session_id != session.resource_owner_key()
            || !bounded_id(&binding.owner.extension_instance_id, 128)
            || !bounded_id(&binding.operation_id, 128)
            || !is_valid_extension_metadata_namespace(&binding.namespace)
        {
            return Err(SessionLeafError::StaleBinding);
        }
        let mut consumer = Self {
            shared: Arc::new(Shared::default()),
            binding: Arc::new(binding),
            incarnation: Arc::downgrade(&session.deferred_run_store()),
        };
        let grant = consumer.issue_grant(session)?;
        let producer = SessionLeafProducer {
            shared: Arc::clone(&consumer.shared),
        };
        Ok((consumer, producer, grant))
    }

    /// Host-selected binding; immutable for this entire activation.
    pub fn binding(&self) -> &SessionLeafBinding {
        &self.binding
    }

    /// Host-only revocation, separate from the untrusted producer.
    pub fn revoker(&self) -> SessionLeafRevoker {
        SessionLeafRevoker {
            shared: Arc::clone(&self.shared),
        }
    }

    fn same_session(&self, session: &Session) -> bool {
        self.incarnation
            .ptr_eq(&Arc::downgrade(&session.deferred_run_store()))
    }

    /// Mint an additional bounded single-use grant at the actual current head.
    /// Normal append chains use the successor in the commit receipt instead.
    pub fn issue_grant(&mut self, session: &Session) -> Result<SessionLeafGrant, SessionLeafError> {
        if !self.same_session(session) {
            return Err(SessionLeafError::StaleSession);
        }
        let grant = make_grant(Arc::clone(&self.binding), session)?;
        let mut state = self
            .shared
            .state
            .lock()
            .expect("session leaf mutex poisoned");
        if let Some(error) = state.terminal {
            return Err(error);
        }
        if state.grants.len() + state.pending.len() >= MAX_SESSION_LEAF_GRANTS {
            return Err(SessionLeafError::Full);
        }
        state.grants.insert(grant.id.clone(), grant.clone());
        Ok(grant)
    }

    /// Owned wake future; does not borrow Session or the consumer while pending.
    /// Returns true for queued work, false for a closed activation.
    pub fn ready(&self) -> impl Future<Output = bool> + Send + 'static {
        let shared = Arc::clone(&self.shared);
        async move {
            loop {
                let notified = shared.ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let state = shared.state.lock().expect("session leaf mutex poisoned");
                    if !state.queue.is_empty() {
                        return true;
                    }
                    if state.terminal.is_some() {
                        return false;
                    }
                }
                notified.await;
            }
        }
    }

    /// Validate and atomically claim one request against cancellation/revocation,
    /// then append through the existing sole writer. No extension callback runs.
    /// A persistence error closes this lane and settles its remaining receipts;
    /// the driver must fail closed, not resume a mutating hook or retry the write.
    pub fn consume_next(
        &mut self,
        session: &mut Session,
        current: &SessionLeafBinding,
    ) -> Result<bool, SessionLeafError> {
        let Some(request) = self.claim_next(session, current)? else {
            return Ok(false);
        };
        self.commit(session, request)
    }

    fn claim_next(
        &self,
        session: &Session,
        current: &SessionLeafBinding,
    ) -> Result<Option<Request>, SessionLeafError> {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("session leaf mutex poisoned");
        let Some(request) = state.queue.pop_front() else {
            return Ok(None);
        };
        let failure = if current != self.binding.as_ref() {
            Some(SessionLeafError::StaleBinding)
        } else if !self.same_session(session) {
            Some(SessionLeafError::StaleSession)
        } else if session.head() != request.grant.head {
            Some(SessionLeafError::StaleHead)
        } else {
            match file_bytes(session) {
                Ok(bytes) if bytes == request.grant.file_bytes => None,
                Ok(_) => Some(SessionLeafError::StaleHead),
                Err(error) => Some(error),
            }
        };
        if let Some(error) = failure {
            Shared::settle(&mut state, request.id, Err(error));
            drop(state);
            self.shared.close(error);
            return Err(error);
        }
        state
            .pending
            .get_mut(&request.id)
            .expect("queued receipt")
            .claimed = true;
        Ok(Some(request))
    }

    /// Test the exact claim/commit linearization without timing-dependent sleeps.
    #[cfg(test)]
    pub(crate) fn consume_with_claim_for_test(
        &mut self,
        session: &mut Session,
        current: &SessionLeafBinding,
        after_claim: impl FnOnce(),
    ) -> Result<bool, SessionLeafError> {
        let Some(request) = self.claim_next(session, current)? else {
            return Ok(false);
        };
        after_claim();
        self.commit(session, request)
    }

    fn commit(
        &mut self,
        session: &mut Session,
        request: Request,
    ) -> Result<bool, SessionLeafError> {
        let result = session.append_extension_entry(
            &self.binding.namespace,
            Some(self.binding.owner.process_generation),
            &request.entry_type,
            request.data,
        );
        match result {
            Ok(entry_id) => {
                let successor = make_grant(Arc::clone(&self.binding), session);
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .expect("session leaf mutex poisoned");
                // Success is known even if post-commit grant allocation fails.
                let successor = successor.ok().filter(|_| state.terminal.is_none());
                if let Some(grant) = &successor {
                    state.grants.insert(grant.id.clone(), grant.clone());
                }
                Shared::settle(
                    &mut state,
                    request.id,
                    Ok(SessionLeafCommit {
                        head: entry_id.clone(),
                        entry_id,
                        successor,
                    }),
                );
                Ok(true)
            }
            Err(_) => {
                self.shared.close(SessionLeafError::Persistence);
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .expect("session leaf mutex poisoned");
                Shared::settle(&mut state, request.id, Err(SessionLeafError::Persistence));
                Err(SessionLeafError::Persistence)
            }
        }
    }
}

impl Drop for SessionLeafConsumer {
    fn drop(&mut self) {
        self.shared.close(SessionLeafError::Closed);
    }
}

fn bounded_id(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn file_bytes(session: &Session) -> Result<u64, SessionLeafError> {
    // Inspect the already-authorized descriptor, never Session::open or its path.
    session
        .try_clone_file()
        .and_then(|file| file.metadata())
        .map(|metadata| metadata.len())
        .map_err(|_| SessionLeafError::Persistence)
}

fn make_grant(
    binding: Arc<SessionLeafBinding>,
    session: &Session,
) -> Result<SessionLeafGrant, SessionLeafError> {
    let mut token = [0u8; 32];
    getrandom::fill(&mut token).map_err(|_| SessionLeafError::Closed)?;
    let id = token.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(SessionLeafGrant {
        id,
        binding,
        head: session.head(),
        file_bytes: file_bytes(session)?,
    })
}

fn payload_bytes(entry_type: &str, data: &Value) -> Result<usize, SessionLeafError> {
    if !bounded_id(entry_type, MAX_EXTENSION_ENTRY_TYPE_BYTES) {
        return Err(SessionLeafError::InvalidPayload);
    }
    // Match the existing private metadata envelope bounds, without widening its
    // control-character policy. These checks bound serialization work as well as
    // queued bytes; Session::append_extension_entry remains the final validator.
    let mut nodes = 2; // envelope object + entry_type string
    let mut stack = vec![(data, 1usize)];
    while let Some((value, depth)) = stack.pop() {
        if depth > 16 || nodes >= 256 {
            return Err(SessionLeafError::InvalidPayload);
        }
        nodes += 1;
        match value {
            Value::String(text)
                if text.len() > MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES
                    || text
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
            {
                return Err(SessionLeafError::InvalidPayload)
            }
            Value::Array(values) => {
                if values.len() + stack.len() + nodes > 256 {
                    return Err(SessionLeafError::InvalidPayload);
                }
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() + stack.len() + nodes > 256 {
                    return Err(SessionLeafError::InvalidPayload);
                }
                for (key, value) in values {
                    if key.len() > 256 || key.chars().any(char::is_control) {
                        return Err(SessionLeafError::InvalidPayload);
                    }
                    stack.push((value, depth + 1));
                }
            }
            _ => {}
        }
    }
    struct Size(usize);
    impl Write for Size {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0 + bytes.len() > MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES {
                return Err(std::io::Error::other("private entry bound exceeded"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[derive(serde::Serialize)]
    struct Envelope<'a> {
        entry_type: &'a str,
        data: &'a Value,
    }
    let mut size = Size(0);
    serde_json::to_writer(&mut size, &Envelope { entry_type, data })
        .map_err(|_| SessionLeafError::InvalidPayload)?;
    Ok(size.0)
}

#[cfg(test)]
mod tests;
