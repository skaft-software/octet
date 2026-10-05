//! A command's transport lifetime survives its own session replacement. Only
//! this admitted command parent is rebound; old retained contexts stay retired.
use super::*;

fn rebind_parent(
    pending: &PendingRequests,
    issued: &IssuedResourceOwners,
    mirror: &Arc<session_leaf::SessionLeafMailbox>,
    parent_id: u64,
    previous: &ExtensionResourceOwner,
) -> Result<(), String> {
    let owner = lock_std_mutex(&mirror.mirror)
        .as_ref()
        .map(|mirror| mirror.owner.clone())
        .ok_or("replacement session snapshot unavailable")?;
    if owner.extension_instance_id != previous.extension_instance_id
        || owner.process_generation != previous.process_generation
    {
        return Err("replacement command owner belongs to a different process".into());
    }
    let mut pending = lock_std_mutex(pending);
    let parent = pending.get_mut(&parent_id)
        .filter(|parent| parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
        .ok_or("replacement command parent is no longer live")?;
    if parent.resource_owner.as_ref() != Some(previous) {
        return Err("replacement command parent owner changed".into());
    }
    // Admission of subsequent requests still derives authority from the live
    // parent. Do not weaken it to accept any explicit owner supplied by Node.
    parent.resource_owner = Some(owner.clone());
    lock_std_mutex(issued).insert(owner);
    Ok(())
}

pub(super) fn queue_replacement(
    state: &ProtocolReadState,
    admitted: AdmittedExtensionRequest,
    parent_id: u64,
    operation: ExtensionSessionLifecycleOperation,
    response_state: Arc<ChildResponseState>,
) -> Result<(), String> {
    let writer = state.writer.clone();
    let frame_limit = Arc::clone(&state.frame_limit);
    let children = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let pending = Arc::clone(&state.pending);
    let issued = Arc::clone(&state.issued_resource_owners);
    let mirror = Arc::clone(&state.session_leaf);
    let worker = state.child_work_slots.clone().try_acquire_owned();
    let submission = match worker.as_ref() {
        Ok(_) => state.session_lifecycle.as_ref()
            .map(|service| service.try_submit(operation))
            .unwrap_or(Err(SessionLifecycleSubmitError::Unavailable)),
        Err(_) => Err(SessionLifecycleSubmitError::Full),
    };
    tokio::spawn(async move {
        let id = admitted.request_id;
        let response = match submission {
            Ok(receiver) => {
                let result = tokio::select! {
                    biased;
                    _ = child_response_settled(response_state) => return,
                    result = receiver => result.unwrap_or(Err(ExtensionSessionLifecycleError::Unavailable)),
                };
                match result {
                    Ok(session_id) => match rebind_parent(&pending, &issued, &mirror, parent_id, &admitted.owner) {
                        Ok(()) => api_v03_session_lifecycle_success(&id, session_id),
                        Err(detail) => api_v03_session_lifecycle_error(&id, "not_foreground_owner", &detail),
                    },
                    Err(ExtensionSessionLifecycleError::Cancelled) => Ok(serde_json::json!({
                        "jsonrpc":"2.0", "id":id, "result":{"cancelled":true}
                    })),
                    Err(ExtensionSessionLifecycleError::Unavailable) => api_v03_session_lifecycle_error(
                        &id, "internal_error", "active-session lifecycle service is unavailable"),
                    Err(ExtensionSessionLifecycleError::Failed) => api_v03_session_lifecycle_error(
                        &id, "internal_error", "active-session lifecycle operation failed"),
                }
            }
            Err(error) => api_v03_session_lifecycle_submit_error(&id, error),
        };
        match response {
            Ok(response) => deliver_api_v03_session_lifecycle_response(
                children, id, writer, frame_limit, health, events, response).await,
            Err(message) => {
                update_health(&health, ExtensionHealthState::Degraded, Some(message.clone()));
                let _ = events.send(ExtensionEvent::Diagnostic { message });
                settle_child_request(&children, &id);
            }
        }
        drop(worker);
    });
    Ok(())
}
