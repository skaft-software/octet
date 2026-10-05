//! API 0.4 immutable bulk on the existing parent/child RPC and disposition gate.
use super::*;
use crate::extension_bulk::{
    BulkError, BulkOwner, BulkParent, CommitJob, PreparedCommit, PreparedRead, ReadJob,
};
use crate::{BlobDigest, BlobRef};

pub(super) const EXTENSION_FEATURE_BULK_OBJECTS_V1: &str = "bulk_objects_v1";
const LOCAL_FILE: &str = "local-file.v1";

pub(super) fn bulk_owner(owner: &ExtensionResourceOwner) -> BulkOwner {
    BulkOwner {
        session: owner.session_id.clone(),
        extension: owner.extension_instance_id.clone(),
        generation: owner.process_generation,
    }
}
pub(super) fn bulk_parent(owner: &ExtensionResourceOwner, id: u64) -> BulkParent {
    BulkParent {
        owner: bulk_owner(owner),
        request_id: id.to_string(),
    }
}

/// Reserved references are values, never parsed from arbitrary tool text.
pub(super) fn collect_blob_refs(
    value: &serde_json::Value,
) -> Result<Vec<BlobRef>, ExtensionRuntimeError> {
    let mut pending = vec![value];
    let mut refs = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::Object(object) if object.contains_key("$blob") => {
                refs.push(
                    serde_json::from_value(value.clone())
                        .map_err(|_| resource_error("blob_unavailable"))?,
                );
            }
            serde_json::Value::Object(object) => pending.extend(object.values()),
            serde_json::Value::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    Ok(refs)
}

pub(super) fn validate_bulk_schema(
    schema: &serde_json::Value,
) -> Result<(), ExtensionRuntimeError> {
    fn closed(schema: &serde_json::Value, fields: &[&str]) -> bool {
        schema["type"] == "object"
            && schema["additionalProperties"] == false
            && schema["properties"].as_object().is_some_and(|p| {
                p.len() == fields.len() && fields.iter().all(|f| p.contains_key(*f))
            })
            && schema["required"].as_array().is_some_and(|r| {
                r.len() == fields.len() && fields.iter().all(|f| r.contains(&serde_json::json!(f)))
            })
    }
    let mut pending = vec![schema];
    while let Some(node) = pending.pop() {
        if node["properties"]
            .as_object()
            .is_some_and(|p| p.contains_key("$blob"))
        {
            let props = &node["properties"];
            let digest = &props["digest"];
            let algorithm = &digest["properties"]["algorithm"];
            let nominal = (algorithm.get("const").is_some() || algorithm.get("enum").is_some())
                && algorithm.get("const").is_none_or(|v| v == "sha256")
                && algorithm
                    .get("enum")
                    .is_none_or(|v| v == &serde_json::json!(["sha256"]));
            if !closed(node, &["$blob", "bytes", "digest", "media_type"])
                || props["$blob"]["type"] != "string"
                || props["bytes"]["type"] != "integer"
                || props["media_type"]["type"] != "string"
                || !closed(digest, &["algorithm", "value"])
                || algorithm["type"] != "string"
                || !nominal
                || digest["properties"]["value"]["type"] != "string"
            {
                return Err(ExtensionRuntimeError::Protocol(
                    "BlobRef schema must be the exact closed sha256 descriptor".into(),
                ));
            }
        }
        match node {
            serde_json::Value::Object(object) => pending.extend(object.values()),
            serde_json::Value::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    Ok(())
}

/// The reserved local-file namespace is transport-only, including after release.
/// Inspect strings for leakage, but never interpret them as reference authority.
pub(super) fn validate_no_bulk_locators(
    value: &serde_json::Value,
    transfer: &Path,
) -> Result<(), ExtensionRuntimeError> {
    let root = transfer.to_string_lossy();
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::String(text) => {
                let locator = text.split("octet-transfer-").skip(1).any(|tail| {
                    tail.as_bytes()
                        .get(..48)
                        .is_some_and(|token| token.iter().all(u8::is_ascii_hexdigit))
                });
                if text.contains(root.as_ref()) || locator {
                    return Err(ExtensionRuntimeError::Protocol(
                        "bulk transport locators are not domain results".into(),
                    ));
                }
            }
            serde_json::Value::Object(object) => {
                if object.get("profile").is_some_and(|p| p == LOCAL_FILE)
                    || object.contains_key("transfer_directory")
                {
                    return Err(ExtensionRuntimeError::Protocol(
                        "bulk transport context is not a domain result".into(),
                    ));
                }
                pending.extend(object.values());
            }
            serde_json::Value::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn diagnostic_blob_ids(
    connection: &ProcessConnection,
    owner: Option<&str>,
    metadata: &serde_json::Value,
) -> Result<Vec<(String, Option<u64>)>, ExtensionRuntimeError> {
    use crate::extension_diagnostics::{AttachmentKind, Diagnostic, Source, METADATA_KEY};
    let Some(diagnostics) = metadata.get(METADATA_KEY) else {
        return Ok(Vec::new());
    };
    let diagnostics: Vec<Diagnostic> = serde_json::from_value(diagnostics.clone())
        .map_err(|_| ExtensionRuntimeError::Protocol("invalid diagnostics".into()))?;
    let mut blobs = Vec::new();
    let artifact = |id: &str| -> Result<(), ExtensionRuntimeError> {
        let owner = owner.ok_or_else(|| resource_error("blob_unavailable"))?;
        connection
            .artifact_store
            .resolve_artifact_for_owner(connection.generation, owner, id)
            .map(|_| ())
            .map_err(|_| resource_error("blob_unavailable"))
    };
    for diagnostic in diagnostics {
        for location in diagnostic
            .primary
            .iter()
            .chain(diagnostic.related.iter().map(|r| &r.location))
            .chain(
                diagnostic
                    .fixes
                    .iter()
                    .flat_map(|f| f.edits.iter().map(|e| &e.location)),
            )
        {
            match &location.source {
                Source::Blob { id } => blobs.push((id.clone(), Some(location.span.end_byte))),
                Source::Artifact { id } => artifact(id)?,
                Source::Workspace { .. } => {}
            }
        }
        for attachment in diagnostic.attachments {
            match attachment.kind {
                AttachmentKind::Blob => blobs.push((attachment.id, None)),
                AttachmentKind::Artifact => artifact(&attachment.id)?,
            }
        }
    }
    Ok(blobs)
}

/// Both validations precede either publication; cancellation/retirement uses
/// this same registry guard. The storage guard also excludes session disposal.
pub(super) fn admit_reference_outputs(
    registry: &mut ResourceRegistry,
    id: u64,
    resources: &[ResourceRef],
    blobs: Option<&[BlobRef]>,
    diagnostics: &[(String, Option<u64>)],
    is_error: bool,
) -> Result<(), ExtensionRuntimeError> {
    registry.validate_outputs(id, resources)?;
    if let Some(blobs) = blobs {
        let parent = registry.bulk_parent(id)?;
        let storage = registry.bulk.clone().expect("admitted bulk store");
        let mut store = storage.lock();
        store
            .validate_outputs(&parent, blobs)
            .map_err(|e| resource_error(e.code()))?;
        for (id, end_byte) in diagnostics {
            let reference = if !is_error {
                blobs.iter().find(|b| &b.id == id).cloned()
            } else {
                None
            };
            let reference = reference
                .map(Ok)
                .unwrap_or_else(|| store.reference_for_session(&parent.owner.session, id))
                .map_err(|e| resource_error(e.code()))?;
            if end_byte.is_some_and(|end| end > reference.bytes) {
                return Err(resource_error("blob_unavailable"));
            }
        }
        if !is_error {
            store
                .admit_parent(&parent, blobs)
                .map_err(|e| resource_error(e.code()))?;
            registry.commit_validated(id, resources);
            return Ok(());
        }
    }
    if is_error {
        registry.cancel_parent(id);
    } else {
        registry.commit_validated(id, resources);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "params")]
enum BulkOperation {
    #[serde(rename = "bulk/write")]
    Write(WriteRequest),
    #[serde(rename = "bulk/commit")]
    Commit(CommitRequest),
    #[serde(rename = "bulk/read")]
    Read(ReadRequest),
    #[serde(rename = "bulk/release")]
    Release(ReleaseRequest),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    parent_request_id: u64,
    profile: String,
    capacity: u64,
    media_type: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitRequest {
    parent_request_id: u64,
    ticket: String,
    bytes: u64,
    digest: BlobDigest,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    parent_request_id: u64,
    profile: String,
    blob: BlobRef,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    parent_request_id: u64,
    id: String,
}
impl BulkOperation {
    fn parent_id(&self) -> u64 {
        match self {
            Self::Write(r) => r.parent_request_id,
            Self::Commit(r) => r.parent_request_id,
            Self::Read(r) => r.parent_request_id,
            Self::Release(r) => r.parent_request_id,
        }
    }
}
enum Work {
    Commit(CommitJob),
    Read(ReadJob),
    Done(serde_json::Value, Option<String>),
}
enum Prepared {
    Commit(PreparedCommit),
    Read(PreparedRead),
    Done(serde_json::Value, Option<String>),
}
impl Work {
    fn run(self) -> Result<Prepared, BulkError> {
        match self {
            Self::Commit(job) => job.run().map(Prepared::Commit),
            Self::Read(job) => job.run().map(Prepared::Read),
            Self::Done(value, grant) => Ok(Prepared::Done(value, grant)),
        }
    }
}
fn refusal(id: &ExtensionRequestId, code: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":code,"data":{"code":code}}})
}
fn refuse(state: &ProtocolReadState, id: ExtensionRequestId, code: &str) -> Result<(), String> {
    insert_child_request(state, id.clone(), None, None)?;
    try_queue_child_response(
        &state.child_requests,
        &id,
        &state.writer,
        state.max_message_bytes(),
        refusal(&id, code),
    )
    .map(|_| ())
}
struct ChildGuard {
    children: ChildRequests,
    id: ExtensionRequestId,
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        settle_child_request(&self.children, &self.id);
    }
}

pub(super) fn dispatch_bulk_request(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, method)?;
    {
        let protocol = read_std_lock(&state.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_4
            || !protocol.supports(EXTENSION_FEATURE_BULK_OBJECTS_V1)
        {
            return refuse(state, id, "unsupported_feature");
        }
    }
    let operation: BulkOperation =
        match serde_json::from_value(serde_json::json!({"method":method,"params":params})) {
            Ok(operation) => operation,
            Err(_) => return refuse(state, id, "blob_unavailable"),
        };
    let parent_id = operation.parent_id();
    let (registered, owner) = {
        let pending = lock_std_mutex(&state.pending);
        let owner = pending
            .get(&parent_id)
            .filter(|p| {
                p.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                    && p.tool_call_policy_digest.is_some()
            })
            .and_then(|p| p.resource_owner.as_ref())
            .filter(|o| {
                o.extension_instance_id == state.instance_id
                    && o.process_generation == state.generation
            });
        let Some(owner) = owner else {
            drop(pending);
            return refuse(state, id, "blob_unavailable");
        };
        (
            insert_child_request(state, id.clone(), Some(parent_id), None)?,
            bulk_owner(owner),
        )
    };
    // Reuse the existing child cancellation token/response CAS, not a second lane.
    let cancellation = CancellationToken::default();
    *lock_std_mutex(&registered.response_state.composition_cancellation) =
        Some(cancellation.clone());
    let worker = match Arc::clone(&state.child_work_slots).try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            return try_queue_child_response(
                &state.child_requests,
                &id,
                &state.writer,
                state.max_message_bytes(),
                refusal(&id, "quota_exceeded"),
            )
            .map(|_| ())
        }
    };
    let resources = Arc::clone(&state.resources);
    let children = Arc::clone(&state.child_requests);
    let writer = state.writer.clone();
    let max_message_bytes = state.max_message_bytes();
    let response_state = registered.response_state;
    let events = state.events.clone();
    tokio::spawn(async move {
        let _child = ChildGuard {
            children: Arc::clone(&children),
            id: id.clone(),
        };
        let prepared = {
            let registry = lock_std_mutex(&resources);
            let result = (|| -> Result<Work, BulkError> {
                if cancellation.is_cancelled()
                    || response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE
                    || !registry.execution_pending(parent_id)
                {
                    return Err(BulkError::Unavailable);
                }
                let parent = registry
                    .bulk_parent(parent_id)
                    .map_err(|_| BulkError::Unavailable)?;
                let mut store = registry
                    .bulk
                    .as_ref()
                    .ok_or(BulkError::UnsupportedFeature)?
                    .lock();
                match operation {
                    BulkOperation::Write(r) => {
                        if r.profile != LOCAL_FILE {
                            return Err(BulkError::UnsupportedFeature);
                        }
                        let ticket = store.write(&parent, r.capacity, &r.media_type)?;
                        Ok(Work::Done(serde_json::json!(ticket), Some(ticket.ticket)))
                    }
                    BulkOperation::Commit(r) => store
                        .prepare_commit(&parent, &r.ticket, r.bytes, &r.digest)
                        .map(Work::Commit),
                    BulkOperation::Read(r) => {
                        if r.profile != LOCAL_FILE {
                            return Err(BulkError::UnsupportedFeature);
                        }
                        store.prepare_read(&parent.owner, &r.blob).map(Work::Read)
                    }
                    BulkOperation::Release(r) => {
                        store.release(&parent.owner, &r.id)?;
                        Ok(Work::Done(serde_json::json!({"released":true}), None))
                    }
                }
            })();
            result
        };
        #[cfg(test)]
        {
            let barrier = {
                if matches!(&prepared, Ok(Work::Commit(_) | Work::Read(_))) {
                    lock_std_mutex(&resources).before_bulk_copy.take()
                } else {
                    None
                }
            };
            if let Some(barrier) = barrier {
                barrier.pause().await;
            }
        }
        #[cfg(test)]
        let copy_hook = {
            let mut registry = lock_std_mutex(&resources);
            if matches!(&prepared, Ok(Work::Commit(_)))
                && registry
                    .bulk_copy_hook
                    .as_ref()
                    .is_some_and(|(request, _)| request == &id)
            {
                registry.bulk_copy_hook.take().map(|(_, hook)| hook)
            } else {
                None
            }
        };
        // Keep the slot until actual blocking work settles, even if caller/child
        // cancellation already won. Dropped job guards reclaim partial files.
        let prepared = match prepared {
            Ok(work) => tokio::task::spawn_blocking(move || {
                let _worker = worker;
                #[cfg(test)]
                {
                    crate::extension_bulk::with_copy_test_hook(copy_hook, || work.run())
                }
                #[cfg(not(test))]
                {
                    work.run()
                }
            })
            .await
            .unwrap_or(Err(BulkError::StorageUnavailable)),
            Err(error) => Err(error),
        };
        let registry = lock_std_mutex(&resources);
        let Some(storage) = &registry.bulk else {
            return;
        };
        let mut store = storage.lock();
        let live = !cancellation.is_cancelled()
            && response_state.state.load(Ordering::Acquire) == CHILD_ACTIVE
            && registry.execution_pending(parent_id)
            && registry.bulk_parent(parent_id).is_ok();
        let result = match prepared {
            Ok(Prepared::Done(value, grant)) => {
                if !live {
                    if let Some(grant) = grant {
                        let _ = store.release(&owner, &grant);
                    }
                    return;
                }
                Ok((value, grant))
            }
            Ok(prepared) if live => match prepared {
                Prepared::Commit(prepared) => store
                    .finish_commit(prepared)
                    .map(|value| (serde_json::json!(value), None)),
                Prepared::Read(prepared) => store.finish_read(prepared).map(|value| {
                    let grant = value.lease.clone();
                    (serde_json::json!(value), Some(grant))
                }),
                Prepared::Done(..) => unreachable!(),
            },
            Ok(_) => return,
            Err(error) if live => Err(error),
            Err(_) => return,
        };
        let (response, grant) = match result {
            Ok((value, grant)) => (
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":value}),
                grant,
            ),
            Err(error) => (refusal(&id, error.code()), None),
        };
        let delivery =
            try_queue_child_response(&children, &id, &writer, max_message_bytes, response);
        if !matches!(delivery, Ok(ChildResponseAdmission::Queued)) {
            if let Some(grant) = grant {
                let _ = store.release(&owner, &grant);
            }
        }
        if delivery.is_err() {
            let _ = events.send(ExtensionEvent::Diagnostic {
                message: "bulk child response delivery failed".into(),
            });
        }
    });
    Ok(())
}
