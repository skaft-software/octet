//! Owner-scoped API 0.4 access to the existing foreground session driver.
use super::*;

mod replacement;

#[cfg(test)]
mod tests;

/// API 0.4 owner-scoped requests to a real foreground session driver.
pub const EXTENSION_FEATURE_SESSION_CONTROL_V1: &str = "session_control_v1";

/// API 0.4 terminal, owner-fenced local compaction on a real idle consumer.
pub const EXTENSION_FEATURE_SESSION_COMPACTION_V1: &str = "session_compaction_v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionCompactionRequest {
    parent_request_id: u64,
    resource_owner: ExtensionResourceOwner,
    #[serde(default)]
    custom_instructions: Option<String>,
}

pub(super) fn dispatch_session_compaction(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let request_id = parse_child_request_id(object, "session/compact")?;
    {
        let protocol = read_std_lock(&state.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_4
            || !protocol.supports(EXTENSION_FEATURE_SESSION_CONTROL_V1)
            || !protocol.supports(EXTENSION_FEATURE_SESSION_COMPACTION_V1)
            || !state
                .session_lifecycle
                .as_ref()
                .is_some_and(|service| service.supports_compaction())
        {
            return reject_typed_child_request(state, request_id, ExtensionRequestFailure::UnsupportedFeature,
                "session/compact requires negotiated session_compaction_v1 and an opted-in idle consumer");
        }
    }
    if let Err(detail) = validate_remote_ui_envelope(object, true) {
        return reject_typed_child_request(
            state,
            request_id,
            ExtensionRequestFailure::InvalidRequest,
            detail,
        );
    }
    let request: SessionCompactionRequest = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(error) => {
            return reject_typed_child_request(
                state,
                request_id,
                ExtensionRequestFailure::InvalidRequest,
                format!("invalid session/compact request: {error}"),
            )
        }
    };
    if let Some(instructions) = &request.custom_instructions {
        if let Err((failure, detail)) =
            bounded_plain_text_failure("compaction instructions", instructions, 16 * 1024)
        {
            return reject_typed_child_request(state, request_id, failure, detail);
        }
        if instructions
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return reject_typed_child_request(
                state,
                request_id,
                ExtensionRequestFailure::InvalidRequest,
                "compaction instructions contain a control character",
            );
        }
    }
    // A synchronous Pi compact() enqueues locally, writes its parent's successful
    // reply first, THEN sends this request. Refuse live parents (especially an
    // awaited compaction hook) rather than deadlock or weaken ordinary lifetimes.
    let pending = lock_std_mutex(&state.pending);
    if pending
        .get(&request.parent_request_id)
        .is_some_and(|parent| parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
    {
        return reject_typed_child_request(state, request_id, ExtensionRequestFailure::InvalidRequest,
            "session/compact requires a settled parent; enqueue only after its successful reply, never await it inside a hook or command");
    }
    if state.closed.load(Ordering::Acquire)
        || state.draining.load(Ordering::Acquire)
        || lock_std_mutex(&state.tombstones).contains(request.parent_request_id)
    {
        return reject_typed_child_request(
            state,
            request_id,
            ExtensionRequestFailure::NotForegroundOwner,
            "session compaction parent or process was cancelled",
        );
    }
    if let Err((failure, detail)) = validate_explicit_request_owner(state, &request.resource_owner)
    {
        return reject_typed_child_request(state, request_id, failure, detail);
    }
    let registered = insert_child_request(state, request_id.clone(), None, None)?;
    drop(pending);
    queue_session_compaction_operation(
        state,
        request_id,
        request.custom_instructions,
        request.resource_owner,
        registered.response_state,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionControlRequest {
    parent_request_id: u64,
    #[serde(default)]
    resource_owner: Option<ExtensionResourceOwner>,
    #[serde(default)]
    session_id: Option<String>,
    /// Pi `fork(entryId)`.
    #[serde(default)]
    entry_id: Option<String>,
    /// Pi `fork` `position`: `before` (default) or `at`.
    #[serde(default)]
    position: Option<String>,
}
impl OwnerScopedHostRequest for SessionControlRequest {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        self.resource_owner.as_ref()
    }
}

pub(super) fn dispatch_session_control(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let Some((request, admitted)) = admit_host_request::<SessionControlRequest>(
        state,
        object,
        method,
        EXTENSION_FEATURE_SESSION_CONTROL_V1,
        params,
    )?
    else {
        return Ok(());
    };
    // Retained callbacks must still belong to this live, issued process owner.
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) {
        return refuse_admitted_request(state, &admitted, failure);
    }
    if lock_std_mutex(&state.tombstones).contains(request.parent_request_id) {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "session control parent was cancelled".into(),
            ),
        );
    }
    let operation = match (method, request.session_id) {
        ("session/wait_for_idle", None) => ExtensionSessionLifecycleOperation::WaitForIdle,
        ("session/create", None) => ExtensionSessionLifecycleOperation::Create,
        ("session/fork", None)
            if matches!(request.position.as_deref(), None | Some("before" | "at")) =>
        {
            ExtensionSessionLifecycleOperation::Fork {
                at: request.position.as_deref() == Some("at"),
                entry_id: request.entry_id,
            }
        }
        ("session/reload", None) => ExtensionSessionLifecycleOperation::Reload,
        ("session/switch", Some(session_id))
            if api_v03::parse_session_switch_params(
                serde_json::json!({"session_id": session_id}),
            )
            .is_ok() =>
        {
            ExtensionSessionLifecycleOperation::Switch { session_id }
        }
        _ => {
            return refuse_admitted_request(
                state,
                &admitted,
                (
                    ExtensionRequestFailure::InvalidRequest,
                    "invalid session control parameters".into(),
                ),
            )
        }
    };
    let response_state = {
        let children = lock_std_mutex(&state.child_requests);
        let Some(child) = children.get(&admitted.request_id) else {
            return Ok(());
        };
        Arc::clone(&child.response_state)
    };
    if matches!(operation, ExtensionSessionLifecycleOperation::Create
        | ExtensionSessionLifecycleOperation::Fork { .. }
        | ExtensionSessionLifecycleOperation::Switch { .. }) {
        replacement::queue_replacement(state, admitted, request.parent_request_id, operation, response_state)
    } else {
        queue_registered_session_lifecycle_operation(state, admitted.request_id, operation, response_state)
    }
}
