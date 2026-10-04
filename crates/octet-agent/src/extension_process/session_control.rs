//! Owner-scoped API 0.4 access to the existing foreground session driver.
use super::*;

#[cfg(test)]
mod tests;

/// API 0.4 owner-scoped requests to a real foreground session driver.
pub const EXTENSION_FEATURE_SESSION_CONTROL_V1: &str = "session_control_v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionControlRequest {
    parent_request_id: u64,
    #[serde(default)]
    resource_owner: Option<ExtensionResourceOwner>,
    #[serde(default)]
    session_id: Option<String>,
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
        ("session/fork", None) => ExtensionSessionLifecycleOperation::Fork,
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
    queue_registered_session_lifecycle_operation(
        state,
        admitted.request_id,
        operation,
        response_state,
    )
}
