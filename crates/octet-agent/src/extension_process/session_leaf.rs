//! Typed producer for an explicitly host-bound private session append leaf.
//!
//! The independent reader routes an explicitly host-bound append directly to
//! the queue, bypassing foreground event broadcast, and retains each receipt
//! through its known commit/failure. No new capability is advertised. This
//! module gives the reader queue admission only, never a Session or file writer.

use serde::{Deserialize, Serialize};

mod session_operations;

#[cfg(all(test, unix))]
mod mirror_tests;

// Retained context consumers accept at most 512 KiB of complete host state.
const MAX_SESSION_MIRROR_BYTES: usize = 512 * 1024;
const MAX_SESSION_MIRROR_ENTRIES: usize = 16_384;

#[derive(Clone, PartialEq)]
pub(super) struct SessionMirror {
    pub(super) owner: ExtensionResourceOwner,
    // None is an unavailable projection, never an empty session. Remember the
    // failure so later callbacks cannot fall back to an older successful view.
    value: Option<serde_json::Value>,
}

impl SessionMirror {
    pub(super) fn rebind_generation(&mut self, generation: u64) {
        self.owner.process_generation = generation;
    }
}

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
    // Observation only, not an append grant. Keep it through connection drain
    // so an accepted reload can rebind the last host-installed immutable view.
    pub(super) mirror: StdMutex<Option<SessionMirror>>,
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
    host_snapshot: Option<serde_json::Value>,
}

impl ExtensionProcess {
    /// Install a complete read-only mirror from the actual native Session. This
    /// is independent of the invocation-scoped append grant. Projection failure
    /// replaces the cached view with unavailable state; it never retains stale
    /// entries or substitutes a successful empty session.
    pub fn set_host_state_with_session(
        &self,
        state: ExtensionHostState,
        session: &crate::Session,
    ) -> Result<(), ExtensionRuntimeError> {
        // Keep the publication on the authoritative generation through the
        // synchronous update. Reload cutover cannot copy an older snapshot
        // halfway through a refresh and then start its replacement callbacks.
        let active = read_std_lock(&self.inner.connection);
        let connection = Arc::clone(&active);
        if !session_mirror_supported(&connection) {
            drop(active);
            self.set_host_state(state);
            return Ok(());
        }
        let owner = ExtensionResourceOwner {
            session_id: session.resource_owner_key(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        };
        let projection = session_snapshot(
            session,
            &self.descriptor().manifest.name,
            MAX_SESSION_MIRROR_BYTES.min(connection.max_message_bytes()),
        )
        .and_then(|snapshot| {
            let mut host = serde_json::to_value(&state)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
            merge_session_snapshot(&mut host, &snapshot);
            validate_session_snapshot_size(&host, MAX_SESSION_MIRROR_BYTES)?;
            // Include the real notification envelope, not just its entries.
            validate_session_snapshot_size(
                &serde_json::json!({
                    "jsonrpc":"2.0", "method":methods::CONTEXT_UPDATED,
                    "params":{"resource_owner":owner,"host":host}
                }),
                connection.max_message_bytes().saturating_sub(1),
            )?;
            Ok(snapshot)
        });
        let mirror = SessionMirror {
            owner,
            value: projection.as_ref().ok().cloned(),
        };
        let (changed, retired_owner) = {
            let mut slot = lock_std_mutex(&connection.session_leaf.mirror);
            let changed = slot.as_ref() != Some(&mirror);
            let retired_owner = slot
                .as_ref()
                .filter(|previous| previous.owner.session_id != mirror.owner.session_id)
                .map(|previous| previous.owner.session_id.clone());
            *slot = Some(mirror);
            (changed, retired_owner)
        };
        self.set_host_state_on_connection(state, connection, changed, true, retired_owner);
        drop(active);
        projection.map(|_| ())
    }

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
            host_snapshot: None,
        })
    }
}

impl SessionLeafProcessLease {
    /// Attach an authoritative native session mirror before dispatch. These are
    /// native records, not Pi-file ABI records; adapters translate them locally.
    /// Snapshot failure revokes this activation without dispatching a callback.
    pub fn with_session_snapshot(
        mut self,
        session: &crate::Session,
    ) -> Result<Self, ExtensionRuntimeError> {
        if session.resource_owner_key() != self.bound.snapshot.owner.session_id {
            return Err(ExtensionRuntimeError::Protocol(
                "session snapshot owner changed".into(),
            ));
        }
        self.host_snapshot = Some(session_snapshot(
            session,
            &self.process.descriptor().manifest.name,
            self.connection.max_message_bytes(),
        )?);
        Ok(self)
    }
    /// Initial immutable grant snapshot for the owned hook request.
    pub fn grant_snapshot(&self) -> &SessionLeafGrantSnapshot {
        &self.bound.snapshot
    }

    /// Owned, borrow-free existing `hook/run` future on the pinned connection.
    /// Adds top-level `session_leaf` to that request. This does not invent a new
    /// hook kind; the hook must already be actually declared and implemented.
    pub async fn run_hook(
        self,
        hook: ExtensionHook,
        payload: serde_json::Value,
        context: ExtensionExecutionContext,
    ) -> Result<ExtensionHookOutput, ExtensionRuntimeError> {
        serde_json::from_value(self.run_hook_value(hook, payload, context).await?).map_err(|_| {
            ExtensionRuntimeError::Protocol("invalid private session hook response".into())
        })
    }

    async fn run_hook_value(
        self,
        hook: ExtensionHook,
        payload: serde_json::Value,
        context: ExtensionExecutionContext,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
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
        if let Some(snapshot) = &self.host_snapshot {
            let host = params["context"]["host"]
                .as_object_mut()
                .expect("execution host serializes to an object");
            host.extend(snapshot.as_object().expect("host snapshot object").clone());
        }
        validate_session_snapshot_size(&params, self.connection.max_message_bytes())?;
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

/// Owner-filtered native record. Private metadata never crosses namespaces,
/// including through compaction preparation or full session mirrors.
fn session_mirror_supported(connection: &ProcessConnection) -> bool {
    let protocol = read_std_lock(&connection.protocol);
    protocol.version == EXTENSION_API_VERSION_0_4
        && protocol.supports(EXTENSION_FEATURE_SESSION_ENTRIES)
}

fn merge_session_snapshot(host: &mut serde_json::Value, snapshot: &serde_json::Value) {
    host.as_object_mut()
        .expect("execution host is an object")
        .extend(
            snapshot
                .as_object()
                .expect("session snapshot is an object")
                .clone(),
        );
}

/// Attach only the view installed for this exact connection and native owner.
/// The private leaf path already carries a newer invocation snapshot and must
/// not have it overwritten by the last frontend boundary's cached observation.
pub(super) fn attach_session_mirror(
    connection: &ProcessConnection,
    owner: &ExtensionResourceOwner,
    host: &mut serde_json::Value,
    notification: bool,
) -> Result<(), ExtensionRuntimeError> {
    if !session_mirror_supported(connection) || host.get("session_entries").is_some() {
        return Ok(());
    }
    let slot = lock_std_mutex(&connection.session_leaf.mirror);
    let Some(mirror) = slot.as_ref() else {
        return Ok(());
    };
    if mirror.owner != *owner || owner.process_generation != connection.generation {
        return Err(ExtensionRuntimeError::Protocol(
            "session snapshot owner changed".into(),
        ));
    }
    if let Some(snapshot) = &mirror.value {
        merge_session_snapshot(host, snapshot);
    } else if notification {
        // Retained adapters merge context updates: explicit nulls invalidate
        // each getter, whereas omitted fields would leave stale data behind.
        merge_session_snapshot(
            host,
            &serde_json::json!({
                "session_entries":null, "session_branch":null,
                "session_leaf_id":null, "session_file":null,
            }),
        );
    } else {
        return Err(ExtensionRuntimeError::Protocol(
            "session snapshot unavailable; no entries truncated".into(),
        ));
    }
    Ok(())
}

pub(super) fn attach_request_session_mirror(
    connection: &ProcessConnection,
    owner: Option<&ExtensionResourceOwner>,
    params: &mut serde_json::Value,
) -> Result<(), ExtensionRuntimeError> {
    if let Some(owner) = owner {
        if let Some(host) = params
            .get_mut("context")
            .and_then(|context| context.get_mut("host"))
        {
            attach_session_mirror(connection, owner, host, false)?;
        }
    }
    Ok(())
}

fn session_snapshot(
    session: &crate::Session,
    namespace: &str,
    limit: usize,
) -> Result<serde_json::Value, ExtensionRuntimeError> {
    if session.entries().len() > MAX_SESSION_MIRROR_ENTRIES {
        return Err(ExtensionRuntimeError::Protocol(
            "session snapshot exceeds entry bound; no entries truncated".into(),
        ));
    }
    let mut entries = Vec::with_capacity(session.entries().len());
    let mut remaining = limit;
    for entry in session.entries() {
        let entry = session_entry_for_namespace(entry, namespace)?;
        remaining = remaining.saturating_sub(session_snapshot_bytes(&entry, remaining)?);
        entries.push(entry);
    }
    let branch = crate::compaction::session_operation_branch(session)
        .map_err(|_| ExtensionRuntimeError::Protocol("session snapshot unavailable".into()))?
        .iter()
        .map(|entry| session_entry_for_namespace(entry, namespace))
        .collect::<Result<Vec<_>, _>>()?;
    let snapshot = serde_json::json!({
        "session_entries":entries, "session_branch":branch,
        "session_leaf_id":session.head(), "session_file":session.path(),
    });
    validate_session_snapshot_size(&snapshot, limit)?;
    Ok(snapshot)
}

fn session_entry_for_namespace(
    entry: &crate::session::Entry,
    namespace: &str,
) -> Result<crate::session::Entry, ExtensionRuntimeError> {
    let mut entry = entry.clone();
    if let Some(metadata) = &mut entry.metadata {
        metadata
            .extension_metadata
            .retain(|owner, value| value.public || owner == namespace);
        let mut total = 0usize;
        if metadata.extension_metadata.len()
            > crate::session::MAX_EXTENSION_ENTRY_METADATA_NAMESPACES
        {
            return Err(ExtensionRuntimeError::Protocol(
                "session metadata exceeds namespace limit".into(),
            ));
        }
        for value in metadata.extension_metadata.values() {
            let bytes = serde_json::to_vec(&value.value)
                .map_err(|_| {
                    ExtensionRuntimeError::Protocol("session metadata unavailable".into())
                })?
                .len();
            total = total.saturating_add(bytes);
            if bytes > crate::session::MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES
                || total > crate::session::MAX_EXTENSION_ENTRY_METADATA_BYTES
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "session metadata exceeds durable bounds".into(),
                ));
            }
        }
    }
    Ok(entry)
}

fn validate_session_snapshot_size(
    value: &impl Serialize,
    limit: usize,
) -> Result<(), ExtensionRuntimeError> {
    session_snapshot_bytes(value, limit).map(|_| ())
}

fn session_snapshot_bytes(
    value: &impl Serialize,
    limit: usize,
) -> Result<usize, ExtensionRuntimeError> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("session snapshot exceeds wire bound"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, value).map_err(|_| {
        ExtensionRuntimeError::Protocol(
            "session snapshot exceeds wire bound; no entries truncated".into(),
        )
    })?;
    Ok(count.bytes)
}

#[cfg(test)]
mod session_snapshot_tests {
    use super::*;
    use crate::session::{
        Entry, EntryId, EntryMetadata, EntryValue, ExtensionEntryMetadata,
        ExtensionMetadataProvenance,
    };

    fn entry() -> Entry {
        let mut metadata = EntryMetadata::default();
        for (namespace, public) in [
            ("owner.one", false),
            ("owner.two", false),
            ("owner.public", true),
        ] {
            metadata.extension_metadata.insert(
                namespace.into(),
                ExtensionEntryMetadata {
                    public,
                    value: serde_json::json!({"data":format!("private:{namespace}")}),
                    provenance: ExtensionMetadataProvenance {
                        extension: namespace.into(),
                        process_generation: Some(1),
                    },
                },
            );
        }
        Entry {
            id: EntryId("001".into()),
            parent: None,
            timestamp_unix_ms: None,
            value: EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            metadata: Some(metadata),
        }
    }

    #[test]
    fn snapshot_retains_only_receivers_private_metadata_and_public_values() {
        let original = entry();
        let projected = session_entry_for_namespace(&original, "owner.one").unwrap();
        let values = &projected.metadata.as_ref().unwrap().extension_metadata;
        assert!(values.contains_key("owner.one"));
        assert!(values.contains_key("owner.public"));
        assert!(!values.contains_key("owner.two"));
        assert!(
            original
                .metadata
                .as_ref()
                .unwrap()
                .extension_metadata
                .contains_key("owner.two")
        );
        assert!(
            !serde_json::to_string(&projected)
                .unwrap()
                .contains("private:owner.two")
        );
    }

    #[test]
    fn oversized_visible_metadata_and_wire_snapshots_refuse_without_truncation() {
        let mut oversized = entry();
        oversized
            .metadata
            .as_mut()
            .unwrap()
            .extension_metadata
            .get_mut("owner.one")
            .unwrap()
            .value = serde_json::Value::String(
            "x".repeat(crate::session::MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES),
        );
        assert!(session_entry_for_namespace(&oversized, "owner.one").is_err());
        let value = serde_json::json!({"session_entries":[entry(),entry()]});
        assert!(validate_session_snapshot_size(&value, 8).is_err());
        assert!(validate_session_snapshot_size(&value, 16 * 1024).is_ok());
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
