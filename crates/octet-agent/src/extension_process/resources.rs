//! Generation-local native resources. Caller disposition and execution settlement
//! are deliberately separate: a dropped waiter is not a native execution fence.
use super::*;

/// Optional API 0.4 native-resource service.
pub const EXTENSION_FEATURE_RESOURCE_REFS_V1: &str = "resource_refs_v1";
/// Optional API 0.4 operation metadata on ordinary tools.
pub const EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1: &str = "operation_descriptors_v1";
/// Finite generation-local registry capacity, including pending cleanup.
pub const MAX_RESOURCE_RECORDS: usize = 256;
/// Maximum provisional registrations by one tool call.
pub const MAX_RESOURCE_REGISTRATIONS_PER_PARENT: usize = 32;

/// Opaque host-issued identity, not serialized native state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    /// Unpredictable identity issued by the host.
    #[serde(rename = "$resource")]
    pub resource: String,
    /// Exact nominal type; never an executable class name.
    #[serde(rename = "type")]
    pub resource_type: String,
}

/// V1 permits only exclusive native execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAccess {
    /// One execution at a time, including after caller cancellation.
    Exclusive,
}

/// Fixed object-property input slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceInput {
    /// Canonical JSON Pointer into tool arguments.
    pub path: String,
    /// Exact required nominal type.
    #[serde(rename = "type")]
    pub resource_type: String,
    /// Exclusive admission; no shared or reentrant access.
    pub access: ResourceAccess,
}

/// Fixed object-property output slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOutput {
    /// Canonical JSON Pointer into structured_content.
    pub path: String,
    /// Exact exported nominal type.
    #[serde(rename = "type")]
    pub resource_type: String,
}

/// Additive metadata; invocation still uses tool/call and unchanged arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDescriptor {
    /// Stable nominal identifier, unique within the catalog.
    pub id: String,
    /// Presentation-only preferred resource input path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// All resource inputs, not just the receiver.
    #[serde(default)]
    pub resource_inputs: Vec<ResourceInput>,
    /// All resource outputs eligible for transactional activation.
    #[serde(default)]
    pub resource_outputs: Vec<ResourceOutput>,
}

/// Cleanup is independent from reference validity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCleanupStatus {
    /// Retired, awaiting bounded disposal.
    Pending,
    /// Extension acknowledged native cleanup.
    Completed,
    /// Extension reported a destructor failure.
    Failed,
    /// Transport/deadline/termination prevented a cleanup acknowledgment.
    Unknown,
}

/// A successful release always invalidates first, independently of cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceReleaseStatus {
    /// Always true for a successful release, including repeated release.
    pub retired: bool,
    /// Separately observed cleanup fact.
    pub cleanup: ResourceCleanupStatus,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceLife {
    Provisional(u64),
    Active,
    Retired,
}
struct ResourceRecord {
    reference: ResourceRef,
    owner: ExtensionResourceOwner,
    life: ResourceLife,
    pin: Option<u64>,
    cleanup: ResourceCleanupStatus,
    disposing: bool,
}
struct ResourceParent {
    owner: ExtensionResourceOwner,
    executing: bool,
    cancelled: bool,
    registrations: usize,
    bulk: bool,
}

#[derive(Default)]
pub(super) struct ResourceRegistry {
    pub(super) bulk: Option<crate::BulkStorage>,
    issued_owners: IssuedResourceOwners,
    #[cfg(test)]
    pub(super) before_result_admission: Option<Arc<ReferenceTestBarrier>>,
    #[cfg(test)]
    pub(super) before_bulk_copy: Option<Arc<ReferenceTestBarrier>>,
    #[cfg(test)]
    pub(super) bulk_copy_hook: Option<(ExtensionRequestId, crate::extension_bulk::CopyTestHook)>,
    records: HashMap<String, ResourceRecord>,
    parents: HashMap<u64, ResourceParent>,
    pub(super) retirement_epoch: u64,
    closed: bool,
    next_token: u64,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct ReferenceTestBarrier {
    pub(super) entered: Notify,
    pub(super) proceed: Notify,
}
#[cfg(test)]
impl ReferenceTestBarrier {
    pub(super) async fn pause(&self) {
        self.entered.notify_one();
        self.proceed.notified().await;
    }
}

pub(super) type Resources = Arc<StdMutex<ResourceRegistry>>;

pub(super) fn resource_error(code: &str) -> ExtensionRuntimeError {
    ExtensionRuntimeError::Remote {
        code: -32000,
        message: code.to_owned(),
        data: Some(serde_json::json!({"code": code})),
    }
}

pub(super) fn validate_nominal(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}
fn validate_ref(reference: &ResourceRef) -> Result<(), ExtensionRuntimeError> {
    if reference.resource.is_empty()
        || reference.resource.len() > 128
        || !reference.resource.is_ascii()
        || !validate_nominal(&reference.resource_type)
    {
        return Err(resource_error("resource_unavailable"));
    }
    Ok(())
}

impl ResourceRegistry {
    pub(super) fn with_bulk(
        bulk: Option<crate::BulkStorage>,
        issued_owners: IssuedResourceOwners,
    ) -> Self {
        Self {
            bulk,
            issued_owners,
            ..Self::default()
        }
    }

    pub(super) fn bulk_parent(
        &self,
        id: u64,
    ) -> Result<crate::extension_bulk::BulkParent, ExtensionRuntimeError> {
        let parent = self
            .parents
            .get(&id)
            .filter(|p| !self.closed && !p.cancelled && p.bulk)
            .ok_or_else(|| resource_error("blob_unavailable"))?;
        Ok(bulk_parent(&parent.owner, id))
    }

    fn record(
        &self,
        owner: &ExtensionResourceOwner,
        reference: &ResourceRef,
        provisional_parent: Option<u64>,
        retired: bool,
    ) -> Result<&ResourceRecord, ExtensionRuntimeError> {
        validate_ref(reference)?;
        let record = self
            .records
            .get(&reference.resource)
            .filter(|r| {
                !self.closed
                    && r.owner == *owner
                    && (r.life == ResourceLife::Active
                        || (retired && r.life == ResourceLife::Retired)
                        || provisional_parent
                            .is_some_and(|id| r.life == ResourceLife::Provisional(id)))
            })
            .ok_or_else(|| resource_error("resource_unavailable"))?;
        if record.reference.resource_type != reference.resource_type {
            return Err(resource_error("resource_type_mismatch"));
        }
        Ok(record)
    }

    pub(super) fn admit(
        &mut self,
        id: u64,
        owner: ExtensionResourceOwner,
        epoch: u64,
        inputs: &[(ResourceRef, String)],
        bulk_inputs: Option<&[crate::BlobRef]>,
        definition: &ToolDefinition,
        arguments: &serde_json::Value,
    ) -> Result<(), ExtensionRuntimeError> {
        if self.closed || self.retirement_epoch != epoch {
            return Err(resource_error("resource_unavailable"));
        }
        if self.parents.len() >= MAX_RESOURCE_RECORDS {
            return Err(resource_error("quota_exceeded"));
        }
        // Authenticate every token before comparing its client-supplied slot type.
        for (reference, _) in inputs {
            self.record(&owner, reference, None, false)?;
        }
        for (reference, nominal) in inputs {
            if reference.resource_type != *nominal {
                return Err(resource_error("resource_type_mismatch"));
            }
            if self.records[&reference.resource].pin.is_some() {
                return Err(resource_error("resource_busy"));
            }
        }
        if let Some(operation) = &definition.operation {
            validate_structured_content(&definition.parameters, arguments)
                .map_err(ExtensionRuntimeError::Protocol)?;
            resource_values(
                arguments,
                operation
                    .resource_inputs
                    .iter()
                    .map(|s| (s.path.as_str(), s.resource_type.as_str())),
            )?;
        }
        if let Some(inputs) = bulk_inputs {
            let parent = bulk_parent(&owner, id);
            let mut store = self.bulk.as_ref().expect("negotiated bulk store").lock();
            store.begin_parent(&parent);
            if let Err(error) = store.validate_outputs(&parent, inputs) {
                store.retire_parent(&parent);
                return Err(resource_error(error.code()));
            }
        }
        for (reference, _) in inputs {
            self.records.get_mut(&reference.resource).unwrap().pin = Some(id);
        }
        self.parents.insert(
            id,
            ResourceParent {
                owner,
                executing: true,
                cancelled: false,
                registrations: 0,
                bulk: bulk_inputs.is_some(),
            },
        );
        Ok(())
    }

    pub(super) fn register(
        &mut self,
        id: u64,
        resource_type: String,
    ) -> Result<ResourceRef, ExtensionRuntimeError> {
        if !validate_nominal(&resource_type) {
            return Err(resource_error("resource_type_mismatch"));
        }
        let parent = self
            .parents
            .get(&id)
            .filter(|p| !self.closed && p.executing && !p.cancelled)
            .ok_or_else(|| resource_error("resource_unavailable"))?;
        if parent.registrations >= MAX_RESOURCE_REGISTRATIONS_PER_PARENT {
            return Err(resource_error("quota_exceeded"));
        }
        if self.records.len() >= MAX_RESOURCE_RECORDS {
            self.records.retain(|_, r| {
                r.life != ResourceLife::Retired || r.cleanup == ResourceCleanupStatus::Pending
            });
        }
        if self.records.len() >= MAX_RESOURCE_RECORDS {
            return Err(resource_error("quota_exceeded"));
        }
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| resource_error("resource_unavailable"))?;
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or_else(|| resource_error("quota_exceeded"))?;
        let token = format!(
            "r{}{:016x}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            self.next_token
        );
        let reference = ResourceRef {
            resource: token.clone(),
            resource_type,
        };
        let parent = self.parents.get_mut(&id).unwrap();
        parent.registrations += 1;
        self.records.insert(
            token,
            ResourceRecord {
                reference: reference.clone(),
                owner: parent.owner.clone(),
                life: ResourceLife::Provisional(id),
                pin: Some(id),
                cleanup: ResourceCleanupStatus::Pending,
                disposing: false,
            },
        );
        Ok(reference)
    }

    /// Caller disposition only: never frees native execution pins.
    pub(super) fn cancel_parent(&mut self, id: u64) -> bool {
        let existed = self.parents.contains_key(&id);
        if let Some(parent) = self.parents.get_mut(&id) {
            parent.cancelled = true;
            if parent.bulk {
                self.bulk
                    .as_ref()
                    .expect("admitted bulk store")
                    .lock()
                    .retire_parent(&bulk_parent(&parent.owner, id));
            }
        }
        for record in self.records.values_mut() {
            if record.life == ResourceLife::Provisional(id) {
                record.life = ResourceLife::Retired;
            }
        }
        if self.parents.get(&id).is_some_and(|p| !p.executing) {
            self.parents.remove(&id);
        }
        existed
    }

    /// Actual terminal reply (even for a dropped waiter), or provably unstarted frame.
    pub(super) fn settle_execution(&mut self, id: u64) -> bool {
        let existed = self.parents.contains_key(&id);
        for record in self.records.values_mut() {
            if record.pin == Some(id) {
                record.pin = None;
            }
        }
        if let Some(parent) = self.parents.get_mut(&id) {
            parent.executing = false;
        }
        if self.parents.get(&id).is_some_and(|p| p.cancelled) {
            self.parents.remove(&id);
        }
        existed
    }

    pub(super) fn validate_outputs(
        &self,
        id: u64,
        outputs: &[ResourceRef],
    ) -> Result<(), ExtensionRuntimeError> {
        let parent = self
            .parents
            .get(&id)
            .filter(|p| !self.closed && !p.cancelled)
            .ok_or_else(|| resource_error("resource_unavailable"))?;
        for reference in outputs {
            self.record(&parent.owner, reference, Some(id), false)?;
        }
        Ok(())
    }

    /// Infallible after resource AND blob validation under this same gate.
    pub(super) fn commit_validated(&mut self, id: u64, outputs: &[ResourceRef]) {
        let exported = outputs
            .iter()
            .map(|r| r.resource.as_str())
            .collect::<HashSet<_>>();
        for record in self.records.values_mut() {
            if record.life == ResourceLife::Provisional(id) {
                record.life = if exported.contains(record.reference.resource.as_str()) {
                    ResourceLife::Active
                } else {
                    ResourceLife::Retired
                };
            }
        }
        self.parents.remove(&id);
    }

    fn release(
        &mut self,
        owner: &ExtensionResourceOwner,
        reference: &ResourceRef,
    ) -> Result<ResourceReleaseStatus, ExtensionRuntimeError> {
        let record = self.record(owner, reference, None, true)?;
        if record.pin.is_some() {
            return Err(resource_error("resource_busy"));
        }
        let record = self.records.get_mut(&reference.resource).unwrap();
        record.life = ResourceLife::Retired;
        Ok(ResourceReleaseStatus {
            retired: true,
            cleanup: record.cleanup,
        })
    }

    pub(super) fn retire_owner(&mut self, session_id: &str) {
        if let Some(storage) = &self.bulk {
            let mut store = storage.lock();
            for owner in lock_std_mutex(&self.issued_owners)
                .iter()
                .filter(|o| o.session_id == session_id)
            {
                store.retire_generation(&bulk_owner(owner));
            }
        }
        self.retirement_epoch = self.retirement_epoch.saturating_add(1);
        for record in self.records.values_mut() {
            if record.owner.session_id == session_id {
                record.life = ResourceLife::Retired;
            }
        }
        let parents = self
            .parents
            .iter()
            .filter_map(|(id, p)| (p.owner.session_id == session_id).then_some(*id))
            .collect::<Vec<_>>();
        for id in parents {
            self.cancel_parent(id);
        }
    }

    #[cfg(test)]
    pub(super) fn cleanup_status(&self, resource: &ResourceRef) -> Option<ResourceCleanupStatus> {
        self.records.get(&resource.resource).map(|r| r.cleanup)
    }

    pub(super) fn execution_pending(&self, id: u64) -> bool {
        self.parents.get(&id).is_some_and(|p| p.executing)
    }

    pub(super) fn has_executions(&self) -> bool {
        self.parents.values().any(|p| p.executing)
    }

    pub(super) fn retire_generation(&mut self) {
        if let Some(storage) = &self.bulk {
            let mut store = storage.lock();
            for owner in lock_std_mutex(&self.issued_owners).iter() {
                store.retire_generation(&bulk_owner(owner));
            }
        }
        self.closed = true;
        for record in self.records.values_mut() {
            record.life = ResourceLife::Retired;
        }
        for parent in self.parents.values_mut() {
            parent.cancelled = true;
        }
    }

    pub(super) fn terminate_generation(&mut self) {
        self.retire_generation();
        self.parents.clear();
        for record in self.records.values_mut() {
            record.pin = None;
            if record.cleanup == ResourceCleanupStatus::Pending {
                record.cleanup = ResourceCleanupStatus::Unknown;
            }
        }
    }

    fn take_disposal(&mut self) -> Vec<ResourceRef> {
        if self.closed {
            return Vec::new();
        }
        self.records
            .values_mut()
            .filter(|r| {
                r.life == ResourceLife::Retired
                    && r.pin.is_none()
                    && r.cleanup == ResourceCleanupStatus::Pending
                    && !r.disposing
            })
            .take(MAX_RESOURCE_REGISTRATIONS_PER_PARENT)
            .map(|r| {
                r.disposing = true;
                r.reference.clone()
            })
            .collect()
    }
}

/// Per-call validated result travels beside the existing JSON-RPC exchange.
/// It is only populated after complete validation and atomic reference admission.
pub(super) struct ToolResultAdmission {
    pub(super) definition: ToolDefinition,
    pub(super) policy: Option<DynamicToolRegistration>,
    pub(super) output: StdMutex<Option<ToolCallOutput>>,
}

impl ExtensionProcess {
    /// Validate without granting execution, projecting schemas or exposing foreign metadata.
    pub fn lookup_resource(
        &self,
        session_id: &str,
        resource: &ResourceRef,
    ) -> Result<ExtensionResourceOwner, ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let owner = self.native_resource_owner(&connection, session_id);
        lock_std_mutex(&connection.resources).record(&owner, resource, None, false)?;
        if !connection_is_usable(&connection) || connection.draining.load(Ordering::Acquire) {
            return Err(resource_error("resource_unavailable"));
        }
        Ok(owner)
    }

    /// Immediate owner invalidation, independent of frontend and hook subscriptions.
    pub fn retire_resource_owner(&self, session_id: &str) {
        let connection = read_std_lock(&self.inner.connection).clone();
        // Bulk/resource retirement still needs the issued tuples to invalidate
        // their backing stores. Revoke reverse-request issuance afterwards.
        lock_std_mutex(&connection.resources).retire_owner(session_id);
        lock_std_mutex(&connection.issued_resource_owners)
            .retain(|owner| owner.session_id != session_id);
        // Retained child calls share the same issued-owner lifetime as other
        // reverse requests. Wake in-flight waits and stop only this owner's
        // worker trees; requesting shutdown is not a settlement receipt.
        connection.pending_changed.notify_waiters();
        if let Some(service) = read_std_lock(&self.inner.delegation_service).clone() {
            service.shutdown_owner(session_id);
        }
        connection.resource_cleanup_changed.notify_one();
        let _ = connection.events.send(ExtensionEvent::Diagnostic {
            message: "resource owner retired; existing executions draining".into(),
        });
    }

    /// Lifecycle release invalidates before acknowledgment; it never waits on a pin.
    pub fn release_resource(
        &self,
        session_id: &str,
        resource: &ResourceRef,
    ) -> Result<ResourceReleaseStatus, ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let owner = self.native_resource_owner(&connection, session_id);
        let result = lock_std_mutex(&connection.resources).release(&owner, resource);
        connection.resource_cleanup_changed.notify_one();
        let _ = connection.events.send(ExtensionEvent::Diagnostic {
            message: if result.is_ok() {
                "resource retired; cleanup independently pending"
            } else {
                "resource release rejected"
            }
            .into(),
        });
        result
    }

    fn native_resource_owner(
        &self,
        connection: &ProcessConnection,
        session_id: &str,
    ) -> ExtensionResourceOwner {
        ExtensionResourceOwner {
            session_id: session_id.to_owned(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        }
    }
}

/// One bounded cleanup lane per generation, reusing the existing writer and RPC.
pub(super) async fn run_resource_cleanup(
    connection: Weak<ProcessConnection>,
    changed: Arc<Notify>,
) {
    loop {
        changed.notified().await;
        let Some(connection) = connection.upgrade() else {
            return;
        };
        loop {
            let resources = lock_std_mutex(&connection.resources).take_disposal();
            if resources.is_empty() {
                break;
            }
            let result = connection
                .request_during_shutdown(
                    "resource/dispose",
                    serde_json::json!({"resources":resources,"reason":"retired"}),
                    connection.shutdown_timeout,
                )
                .await;
            let statuses = result
                .as_ref()
                .ok()
                .and_then(|v| v.get("results"))
                .and_then(|v| v.as_array());
            let unknown = {
                let mut registry = lock_std_mutex(&connection.resources);
                for reference in &resources {
                    let status = statuses
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|e| e.get("resource") == Some(&serde_json::json!(reference)))
                        })
                        .and_then(|e| e.get("status"))
                        .and_then(|v| v.as_str());
                    let cleanup = match status {
                        Some("completed") => ResourceCleanupStatus::Completed,
                        Some("failed") => ResourceCleanupStatus::Failed,
                        _ => ResourceCleanupStatus::Unknown,
                    };
                    if let Some(record) = registry.records.get_mut(&reference.resource) {
                        record.cleanup = cleanup;
                    }
                }
                statuses.is_none()
                    || resources.iter().any(|r| {
                        registry
                            .records
                            .get(&r.resource)
                            .is_some_and(|r| r.cleanup == ResourceCleanupStatus::Unknown)
                    })
            };
            let _ = connection.events.send(ExtensionEvent::Diagnostic {
                message: format!(
                    "resource cleanup settled: count={}, unknown={unknown}",
                    resources.len()
                ),
            });
            if unknown {
                connection.terminate().await;
                return;
            }
        }
        if connection.closed.load(Ordering::Acquire) {
            return;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRequest {
    parent_request_id: u64,
    #[serde(rename = "type")]
    resource_type: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    parent_request_id: u64,
    resource: ResourceRef,
}

pub(super) fn dispatch_resource_request(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, method)?;
    let result = (|| -> Result<serde_json::Value, ExtensionRuntimeError> {
        if read_std_lock(&state.protocol).version != EXTENSION_API_VERSION_0_4
            || !read_std_lock(&state.protocol).supports(EXTENSION_FEATURE_RESOURCE_REFS_V1)
        {
            return Err(resource_error("unsupported_feature"));
        }
        let parent_id = params
            .get("parent_request_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| resource_error("resource_unavailable"))?;
        // Pending + registry is the common lock order with caller cancellation.
        let pending = lock_std_mutex(&state.pending);
        let parent = pending
            .get(&parent_id)
            .filter(|p| {
                p.tool_call_policy_digest.is_some()
                    && p.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
            })
            .ok_or_else(|| resource_error("resource_unavailable"))?;
        let owner = parent
            .resource_owner
            .as_ref()
            .ok_or_else(|| resource_error("resource_unavailable"))?;
        insert_child_request(state, id.clone(), Some(parent_id), None)
            .map_err(ExtensionRuntimeError::Protocol)?;
        let mut registry = lock_std_mutex(&state.resources);
        match method {
            "resource/register" => {
                let request: RegisterRequest = serde_json::from_value(params)
                    .map_err(|_| resource_error("resource_unavailable"))?;
                serde_json::to_value(
                    registry.register(request.parent_request_id, request.resource_type)?,
                )
                .map_err(|e| ExtensionRuntimeError::Protocol(e.to_string()))
            }
            _ => {
                let request: ReleaseRequest = serde_json::from_value(params)
                    .map_err(|_| resource_error("resource_unavailable"))?;
                debug_assert_eq!(request.parent_request_id, parent_id);
                serde_json::to_value(registry.release(owner, &request.resource)?)
                    .map_err(|e| ExtensionRuntimeError::Protocol(e.to_string()))
            }
        }
    })();
    // Refusals use the same bounded child-response path as successful replies.
    if !lock_std_mutex(&state.child_requests).contains_key(&id) {
        insert_child_request(state, id.clone(), None, None)?;
    }
    let response = match result {
        Ok(value) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":value}),
        Err(ExtensionRuntimeError::Remote {
            code,
            message,
            data,
        }) => {
            serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message,"data":data}})
        }
        Err(_) => {
            serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"invalid resource request","data":{"code":"resource_unavailable"}}})
        }
    };
    try_queue_child_response(
        &state.child_requests,
        &id,
        &state.writer,
        state.max_message_bytes(),
        response,
    )?;
    state.resource_cleanup_changed.notify_one();
    Ok(())
}
