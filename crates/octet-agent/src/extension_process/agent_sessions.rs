//! Optional retained child calls reuse issued owners, never caller authority.
use super::*;

pub(super) fn register_agent_session_request(
    state: &ProtocolReadState,
    id: ExtensionRequestId,
    parent_request_id: u64,
    method: &str,
    explicit_owner: Option<ExtensionResourceOwner>,
) -> Result<Option<RegisteredChildRequest>, String> {
    let Some(owner) = explicit_owner else {
        return register_child_request(state, id, Some(parent_request_id), method);
    };
    let supported = {
        let protocol = read_std_lock(&state.protocol);
        protocol.version == EXTENSION_API_VERSION_0_4
            && protocol.supports(EXTENSION_FEATURE_AGENT_SESSION_LIFETIME_V1)
    };
    if !supported {
        reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::UnsupportedFeature,
            "explicit child owner requires API 0.4 agent_session_lifetime_v1",
        )?;
        return Ok(None);
    }
    if let Err((failure, detail)) = validate_explicit_request_owner(state, &owner) {
        reject_typed_child_request(state, id, failure, detail)?;
        return Ok(None);
    }
    let pending = lock_std_mutex(&state.pending);
    if lock_std_mutex(&state.tombstones).contains(parent_request_id) {
        drop(pending);
        reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::NotForegroundOwner,
            "child session parent was cancelled",
        )?;
        return Ok(None);
    }
    if let Some(parent) = pending
        .get(&parent_request_id)
        .filter(|parent| parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
    {
        if parent.resource_owner.as_ref() != Some(&owner) {
            drop(pending);
            reject_typed_child_request(
                state,
                id,
                ExtensionRequestFailure::NotForegroundOwner,
                "explicit child owner does not match the active parent",
            )?;
            return Ok(None);
        }
        // An active parent retains ordinary child cancellation/settlement.
        // The lifetime feature is not the editor's narrow checkpoint exception.
        drop(pending);
        return register_child_request(state, id, Some(parent_request_id), method);
    }
    // This negotiated service authenticates new calls with an issued session
    // owner, not a fabricated live parent. Cancelling an individual request is
    // not session-wide revocation; retirement invalidates the issued owner.
    // The service still checks its principal/owner and owned target at dispatch.
    let mut registered = insert_child_request(state, id, None, None)?;
    registered.resource_owner = Some(owner);
    drop(pending);
    Ok(Some(registered))
}
