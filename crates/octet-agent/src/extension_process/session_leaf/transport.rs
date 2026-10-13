//! Session-repair.v2: counted immutable documents, parent-bound chunk handles.
//!
//! The encoded representation is the native view. One Arc has one reservation,
//! retained until both current publication and every invocation have released it.
use super::*;
use base64::Engine as _;
use serde_json::{json, Value};

mod dispatch;
mod history;
pub(in crate::extension_process) use dispatch::attach;
pub(in crate::extension_process) use dispatch::{publish, stage, PublicationPump, Staged};
pub(super) mod receipt;
mod wire;
pub use wire::SessionSnapshotProfile;
pub(in crate::extension_process) use wire::{Descriptor, Preparation};

pub(crate) const OWNER_ROUTES: &str = "session_owner_routes_v1";
pub(crate) const SNAPSHOT_TRANSPORT: &str = "session_snapshot_transport_v1";
pub(in crate::extension_process) const PREPARE: &str = "session/snapshot/prepare";
const READ: &str = "session/snapshot/read";
const RELEASE: &str = "session/snapshot/release";
const PROJECTION_BEGIN: &str = "session/projection/begin";
const PROJECTION_CHUNK: &str = "session/projection/chunk";
const PROJECTION_COMMIT: &str = "session/projection/commit";
const MAX_SAFE: u64 = 9_007_199_254_740_991;

fn unavailable() -> ExtensionRuntimeError {
    ExtensionRuntimeError::Protocol("session transport unavailable; no history truncated".into())
}

#[derive(Default)]
struct Accounting {
    bytes: usize,
    records: usize,
    views: usize,
    projection_bytes: usize,
    projections: usize,
}
struct Reservation {
    accounting: Arc<StdMutex<Accounting>>,
    bytes: usize,
    records: usize,
    projection: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut a = lock_std_mutex(&self.accounting);
        if self.projection {
            a.projection_bytes -= self.bytes;
            a.projections -= 1;
        } else {
            a.bytes -= self.bytes;
            a.records -= self.records;
            a.views -= 1;
        }
    }
}

pub(in crate::extension_process) struct View {
    // Declaration order is intentional: bytes are dropped before the quota.
    bytes: Vec<u8>,
    pub(in crate::extension_process) descriptor: Descriptor,
    _reservation: Reservation,
}
struct Transfer {
    parent: u64,
    view: Arc<View>,
    offset: usize,
}
struct Projection {
    parent: u64,
    owner: ExtensionResourceOwner,
    bytes: Vec<u8>,
    expected_bytes: usize,
    sha256: String,
    committed: bool,
    _reservation: Reservation,
}
#[derive(Default)]
pub(in crate::extension_process) struct Store {
    accounting: Arc<StdMutex<Accounting>>,
    revision: u64,
    pub(in crate::extension_process) current: HashMap<ExtensionResourceOwner, Option<Arc<View>>>,
    transfers: HashMap<String, Transfer>,
    pub(super) parents: HashMap<u64, (ExtensionResourceOwner, Option<Preparation>)>,
    projections: HashMap<String, Projection>,
}

impl Store {
    #[cfg(test)]
    pub(in crate::extension_process) fn transfer_offset(&self, parent: u64) -> Option<usize> {
        self.transfers
            .values()
            .find(|t| t.parent == parent)
            .map(|t| t.offset)
    }

    #[cfg(test)]
    pub(in crate::extension_process) fn in_flight(&self, parent: u64) -> usize {
        self.transfers
            .values()
            .filter(|t| t.parent == parent)
            .count()
    }

    #[cfg(test)]
    pub(in crate::extension_process) fn retained(&self) -> (usize, usize, usize) {
        let accounting = lock_std_mutex(&self.accounting);
        (accounting.bytes, accounting.records, accounting.views)
    }

    pub(in crate::extension_process) fn next_revision(
        &mut self,
    ) -> Result<u64, ExtensionRuntimeError> {
        self.revision = self
            .revision
            .checked_add(1)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or_else(unavailable)?;
        Ok(self.revision)
    }
    fn reserve(
        &self,
        profile: &SessionSnapshotProfile,
        bytes: usize,
        records: usize,
        projection: bool,
    ) -> Result<Reservation, ExtensionRuntimeError> {
        let mut a = lock_std_mutex(&self.accounting);
        if projection {
            if bytes == 0
                || bytes > profile.projection_bytes
                || a.projection_bytes
                    .checked_add(bytes)
                    .is_none_or(|v| v > profile.projection_bytes)
                || a.projections >= profile.projections
            {
                return Err(unavailable());
            }
            a.projection_bytes += bytes;
            a.projections += 1;
        } else {
            if bytes == 0
                || bytes > profile.snapshot_bytes
                || records > profile.view_entries
                || a.bytes
                    .checked_add(bytes)
                    .is_none_or(|v| v > profile.generation_bytes)
                || a.records
                    .checked_add(records)
                    .is_none_or(|v| v > profile.generation_entries)
                || a.views >= profile.owner_views
            {
                return Err(unavailable());
            }
            a.bytes += bytes;
            a.records += records;
            a.views += 1;
        }
        Ok(Reservation {
            accounting: Arc::clone(&self.accounting),
            bytes,
            records,
            projection,
        })
    }

    /// Encode one complete history document at an already reserved revision.
    /// The caller allocates the revision first so a newest-wins barrier can
    /// never advertise a revision newer than the document it announces.
    pub(in crate::extension_process) fn history(
        &mut self,
        session: &crate::Session,
        namespace: &str,
        owner: ExtensionResourceOwner,
        profile: &SessionSnapshotProfile,
        preparation: Option<Preparation>,
        revision: u64,
    ) -> Result<Arc<View>, ExtensionRuntimeError> {
        history::encode(
            self,
            session,
            namespace,
            owner,
            profile,
            preparation,
            revision,
        )
    }

    pub(in crate::extension_process) fn invocation(
        &mut self,
        payload: &Value,
        history: &View,
        profile: &SessionSnapshotProfile,
    ) -> Result<Arc<View>, ExtensionRuntimeError> {
        // Canonical request/projection admission remains 64MiB, independently of
        // history capacity. No private transport frame cap becomes a request cap.
        let bytes = session_snapshot_bytes(payload, profile.projection_bytes)?;
        let reservation = self.reserve(profile, bytes, 0, false)?;
        let mut encoded = Vec::with_capacity(bytes);
        serde_json::to_writer(&mut encoded, payload).map_err(|_| unavailable())?;
        let descriptor = Descriptor {
            kind: "invocation".into(),
            bytes,
            sha256: hex_digest(&encoded),
            entry_count: 0,
            branch_count: 0,
            ..history.descriptor.clone()
        };
        Ok(Arc::new(View {
            bytes: encoded,
            descriptor,
            _reservation: reservation,
        }))
    }

    pub(in crate::extension_process) fn transfer(
        &mut self,
        parent: u64,
        view: Arc<View>,
        profile: &SessionSnapshotProfile,
    ) -> Result<Descriptor, ExtensionRuntimeError> {
        if self.transfers.len() >= profile.transfers {
            return Err(unavailable());
        }
        let id = new_token()?;
        let descriptor = Descriptor {
            transfer_id: id.clone(),
            ..view.descriptor.clone()
        };
        self.transfers.insert(
            id,
            Transfer {
                parent,
                view,
                offset: 0,
            },
        );
        Ok(descriptor)
    }

    pub(in crate::extension_process) fn settle(&mut self, parent: u64) {
        self.parents.remove(&parent);
        self.transfers.retain(|_, t| t.parent != parent);
        self.projections.retain(|_, p| p.parent != parent);
    }
    pub(in crate::extension_process) fn clear(&mut self) {
        self.transfers.clear();
        self.projections.clear();
        self.current.clear();
        self.parents.clear();
    }

    fn read(
        &mut self,
        request: wire::Read,
        owner: &ExtensionResourceOwner,
        profile: &SessionSnapshotProfile,
    ) -> Result<Value, ExtensionRuntimeError> {
        let t = self
            .transfers
            .get_mut(&request.transfer_id)
            .ok_or_else(unavailable)?;
        if t.parent != request.parent_request_id
            || &t.view.descriptor.owner != owner
            || request.offset != t.offset
            || request.max_bytes == 0
            || request.max_bytes > profile.chunk_bytes
            || t.offset >= t.view.bytes.len()
        {
            return Err(unavailable());
        }
        let end = t
            .offset
            .saturating_add(request.max_bytes)
            .min(t.view.bytes.len());
        let data = base64::engine::general_purpose::STANDARD.encode(&t.view.bytes[t.offset..end]);
        t.offset = end;
        Ok(
            json!({"transfer_id":request.transfer_id,"offset":request.offset,"data":data,"next_offset":end,"eof":end == t.view.bytes.len()}),
        )
    }

    fn release(
        &mut self,
        request: wire::Handle,
        owner: &ExtensionResourceOwner,
    ) -> Result<Value, ExtensionRuntimeError> {
        let t = self
            .transfers
            .get(&request.transfer_id)
            .ok_or_else(unavailable)?;
        if t.parent != request.parent_request_id || &t.view.descriptor.owner != owner {
            return Err(unavailable());
        }
        self.transfers.remove(&request.transfer_id);
        Ok(json!({"released":true}))
    }

    fn projection_begin(
        &mut self,
        r: wire::ProjectionBegin,
        owner: &ExtensionResourceOwner,
        profile: &SessionSnapshotProfile,
    ) -> Result<Value, ExtensionRuntimeError> {
        if !wire::valid_hash(&r.sha256) {
            return Err(unavailable());
        }
        let reservation = self.reserve(profile, r.bytes, 0, true)?;
        let id = new_token()?;
        self.projections.insert(
            id.clone(),
            Projection {
                parent: r.parent_request_id,
                owner: owner.clone(),
                bytes: Vec::with_capacity(r.bytes),
                expected_bytes: r.bytes,
                sha256: r.sha256,
                committed: false,
                _reservation: reservation,
            },
        );
        Ok(json!({"transfer_id":id,"chunk_bytes":profile.chunk_bytes}))
    }

    fn projection_chunk(
        &mut self,
        r: wire::ProjectionChunk,
        owner: &ExtensionResourceOwner,
        profile: &SessionSnapshotProfile,
    ) -> Result<Value, ExtensionRuntimeError> {
        let p = self
            .projections
            .get_mut(&r.transfer_id)
            .ok_or_else(unavailable)?;
        if p.parent != r.parent_request_id
            || &p.owner != owner
            || p.committed
            || r.offset != p.bytes.len()
            || r.data.len() > profile.chunk_bytes.div_ceil(3) * 4
        {
            return Err(unavailable());
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&r.data)
            .map_err(|_| unavailable())?;
        if decoded.is_empty()
            || decoded.len() > profile.chunk_bytes
            || base64::engine::general_purpose::STANDARD.encode(&decoded) != r.data
            || decoded.len() > p.expected_bytes.saturating_sub(p.bytes.len())
        {
            return Err(unavailable());
        }
        p.bytes.extend_from_slice(&decoded);
        Ok(json!({"next_offset":p.bytes.len()}))
    }

    fn projection_commit(
        &mut self,
        r: wire::Handle,
        owner: &ExtensionResourceOwner,
    ) -> Result<Value, ExtensionRuntimeError> {
        let p = self
            .projections
            .get_mut(&r.transfer_id)
            .ok_or_else(unavailable)?;
        if p.parent != r.parent_request_id
            || &p.owner != owner
            || p.committed
            || p.bytes.len() != p.expected_bytes
            || hex_digest(&p.bytes) != p.sha256
            || std::str::from_utf8(&p.bytes).is_err()
        {
            return Err(unavailable());
        }
        p.committed = true;
        Ok(json!({"transfer_id":r.transfer_id,"bytes":p.bytes.len(),"sha256":p.sha256}))
    }

    pub(in crate::extension_process) fn resolve_projection(
        &mut self,
        parent: u64,
        result: &mut Value,
    ) -> Result<(), ExtensionRuntimeError> {
        let Some(reference) = result.get_mut("provider_context_transfer").map(Value::take) else {
            return Ok(());
        };
        if result.get("provider_context").is_some() {
            return Err(unavailable());
        }
        let reference: wire::ProjectionResult =
            serde_json::from_value(reference).map_err(|_| unavailable())?;
        let p = self
            .projections
            .remove(&reference.transfer_id)
            .ok_or_else(unavailable)?;
        if p.parent != parent
            || !p.committed
            || p.bytes.len() != reference.bytes
            || p.sha256 != reference.sha256
        {
            return Err(unavailable());
        }
        // Decode while the exact encoded reservation is held. The authoritative
        // Agent still performs identity, loadout, replay, budget and 64MiB checks.
        let value: Value = serde_json::from_slice(&p.bytes).map_err(|_| unavailable())?;
        result
            .as_object_mut()
            .ok_or_else(unavailable)?
            .remove("provider_context_transfer");
        result["provider_context"] = value;
        Ok(())
    }
}

/// The legacy-shaped session facts an admitted setup/replacement receipt
/// carries: same records as the counted document, branch expanded to entries.
/// One representation of history; only the delivery channel differs.
pub(in crate::extension_process) fn host_facts(
    view: &View,
) -> Result<Value, ExtensionRuntimeError> {
    let document: Value = serde_json::from_slice(&view.bytes).map_err(|_| unavailable())?;
    let entries = document.get("entries").cloned().ok_or_else(unavailable)?;
    let mut by_id = HashMap::new();
    for entry in entries.as_array().into_iter().flatten() {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            by_id.insert(id.to_owned(), entry.clone());
        }
    }
    let branch = document
        .get("branch_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().and_then(|id| by_id.get(id)).cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let header = document.get("header").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "session_entries": entries,
        "session_branch": branch,
        "session_leaf_id": document.get("head").cloned().unwrap_or(Value::Null),
        "session_file": document.get("file").cloned().unwrap_or(Value::Null),
        // An absent native header is an explicit null, never an empty object.
        "session_header": if header.as_object().is_some_and(|object| object.is_empty()) { Value::Null } else { header },
        "session_labels": document.get("labels").cloned().unwrap_or(Value::Null),
        "session_view_revision": view.descriptor.view_revision,
    }))
}

fn new_token() -> Result<String, ExtensionRuntimeError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| unavailable())?;
    Ok(hex_digest(&bytes))
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Handles are not authority. Admission supplies an ACTIVE numeric parent, and
/// each handle must match that exact parent and its complete host-issued owner.
pub(in crate::extension_process) fn dispatch(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, Value>,
    method: &str,
    params: Value,
) -> Result<bool, String> {
    if !matches!(
        method,
        READ | RELEASE | PROJECTION_BEGIN | PROJECTION_CHUNK | PROJECTION_COMMIT
    ) {
        return Ok(false);
    }
    let Some((request, admitted)) = admit_host_request::<wire::Parent>(
        state,
        object,
        method,
        SNAPSHOT_TRANSPORT,
        json!({"parent_request_id":params.get("parent_request_id")}),
    )?
    else {
        return Ok(true);
    };
    let pending = lock_std_mutex(&state.pending);
    let active = pending.get(&request.parent_request_id).filter(|p| {
        p.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
            && p.resource_owner.as_ref() == Some(&admitted.owner)
    });
    let protocol = read_std_lock(&state.protocol);
    let result = if active.is_none()
        || !owner_routes_enabled(&protocol)
        || validate_remote_ui_envelope(object, true).is_err()
    {
        Err(unavailable())
    } else {
        let profile = state
            .session_leaf
            .profile(state.max_message_bytes())
            .map_err(|e| e.to_string())?;
        let mut store = lock_std_mutex(&state.session_leaf.transport);
        macro_rules! parse {
            ($ty:ty) => {
                serde_json::from_value::<$ty>(params).map_err(|_| unavailable())
            };
        }
        match method {
            READ => parse!(wire::Read).and_then(|r| store.read(r, &admitted.owner, &profile)),
            RELEASE => parse!(wire::Handle).and_then(|r| store.release(r, &admitted.owner)),
            PROJECTION_BEGIN if active.is_some_and(|p| p.method == methods::HOOK_RUN) => {
                parse!(wire::ProjectionBegin)
                    .and_then(|r| store.projection_begin(r, &admitted.owner, &profile))
            }
            PROJECTION_CHUNK => parse!(wire::ProjectionChunk)
                .and_then(|r| store.projection_chunk(r, &admitted.owner, &profile)),
            PROJECTION_COMMIT => {
                parse!(wire::Handle).and_then(|r| store.projection_commit(r, &admitted.owner))
            }
            _ => Err(unavailable()),
        }
    };
    let response = match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":admitted.request_id,"result":result}),
        Err(_) => {
            json!({"jsonrpc":"2.0","id":admitted.request_id,"error":{"code":-32602,"message":"session transport unavailable"}})
        }
    };
    try_queue_child_response(
        &state.child_requests,
        &admitted.request_id,
        &state.writer,
        state.max_message_bytes(),
        response,
    )?;
    Ok(true)
}
