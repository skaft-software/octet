//! A command's transport lifetime survives its own session replacement. Only
//! this admitted command parent is rebound; old retained contexts stay retired.
use super::*;

enum ReplacementReceiver {
    Session(oneshot::Receiver<Result<String, ExtensionSessionLifecycleError>>),
    Setup(oneshot::Receiver<Result<serde_json::Value, String>>),
}
impl ReplacementReceiver {
    async fn receive(self) -> Result<serde_json::Value, ExtensionSessionLifecycleError> {
        match self {
            Self::Session(receiver) => receiver.await.unwrap_or(Err(ExtensionSessionLifecycleError::Unavailable))
                .map(|id| serde_json::json!({"session_id":id})),
            Self::Setup(receiver) => receiver.await.map_err(|_| ExtensionSessionLifecycleError::Unavailable)?
                .map_err(|_| ExtensionSessionLifecycleError::Failed),
        }
    }
}

impl ExtensionProcess {
    /// Project the real mirror for this pinned setup process and live owner.
    pub fn replacement_context(&self, owner: &ExtensionResourceOwner) -> Result<serde_json::Value, ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let context = self.current_context_for_resource_owner(owner.session_id.clone());
        if context.resource_owner.as_ref() != Some(owner) {
            return Err(ExtensionRuntimeError::Protocol("session setup process generation changed".into()));
        }
        let mut context = serde_json::to_value(context).map_err(|e| ExtensionRuntimeError::Protocol(e.to_string()))?;
        session_leaf::attach_session_mirror(&connection, owner, &mut context["host"], false)?;
        Ok(context)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SetupMutation {
    Append { entry: serde_json::Value },
    Branch { entry_id: Option<String> },
    Complete,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    parent_request_id: u64,
    resource_owner: ExtensionResourceOwner,
    mutation: SetupMutation,
}
impl OwnerScopedHostRequest for SetupRequest {
    fn parent_request_id(&self) -> u64 { self.parent_request_id }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> { Some(&self.resource_owner) }
}

pub(super) fn dispatch_setup(state: &ProtocolReadState, object: &serde_json::Map<String, serde_json::Value>, params: serde_json::Value) -> Result<(), String> {
    let Some((request, admitted)) = admit_host_request::<SetupRequest>(state, object, "session/setup", EXTENSION_FEATURE_SESSION_CONTROL_V1, params)? else { return Ok(()); };
    // Setup is never a retained grant: every write must still have this live
    // command parent, the admitted process owner, and the native setup boundary.
    let live = lock_std_mutex(&state.pending).get(&request.parent_request_id).is_some_and(|parent|
        parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE && parent.resource_owner.as_ref() == Some(&admitted.owner));
    if !live || request.resource_owner != admitted.owner || lock_std_mutex(&state.tombstones).contains(request.parent_request_id) {
        return refuse_admitted_request(state, &admitted, (ExtensionRequestFailure::NotForegroundOwner, "session setup requires its live replacement command parent".into()));
    }
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) { return refuse_admitted_request(state, &admitted, failure); }
    let mutation = serde_json::to_value(request.mutation).map_err(|e| e.to_string())?;
    if serde_json::to_vec(&mutation).map_err(|e| e.to_string())?.len() > 32768 {
        return refuse_admitted_request(state, &admitted, (ExtensionRequestFailure::InvalidRequest, "session setup mutation exceeds 32KiB".into()));
    }
    let response_state = {
        let children = lock_std_mutex(&state.child_requests);
        let Some(child) = children.get(&admitted.request_id) else { return Ok(()); };
        Arc::clone(&child.response_state)
    };
    let worker = state.child_work_slots.clone().try_acquire_owned();
    let submission = match (worker.as_ref(), state.session_lifecycle.as_ref()) {
        (Ok(_), Some(service)) => service.try_submit_setup(ExtensionSessionLifecycleOperation::Setup {
            parent_request_id: request.parent_request_id, owner: admitted.owner.clone(),
            namespace: state.extension_identity.name.clone(), mutation,
        }),
        (Ok(_), None) => Err(SessionLifecycleSubmitError::Unavailable),
        (Err(_), _) => Err(SessionLifecycleSubmitError::Full),
    };
    let writer = state.writer.clone(); let frame_limit = Arc::clone(&state.frame_limit);
    let children = Arc::clone(&state.child_requests); let health = Arc::clone(&state.health); let events = state.events.clone();
    tokio::spawn(async move {
        let id = admitted.request_id;
        let response = match submission {
            Ok(receiver) => {
                let result = tokio::select! {
                    biased;
                    _ = child_response_settled(response_state) => return,
                    result = receiver => result.unwrap_or(Err("session setup driver unavailable".into())),
                };
                match result {
                    Ok(result) => Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})),
                    Err(detail) => api_v03_session_lifecycle_error(&id, "internal_error", &detail),
                }
            }
            Err(error) => api_v03_session_lifecycle_submit_error(&id, error),
        };
        if let Ok(response) = response {
            deliver_api_v03_session_lifecycle_response(children, id, writer, frame_limit, health, events, response).await;
        } else { settle_child_request(&children, &id); }
        drop(worker);
    });
    Ok(())
}

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
    let setup = matches!(&operation, ExtensionSessionLifecycleOperation::CreateWithOptions { setup_parent: Some(_), .. });
    let submission = match (worker.as_ref(), state.session_lifecycle.as_ref()) {
        (Ok(_), Some(service)) if setup => service.try_submit_setup(operation).map(ReplacementReceiver::Setup),
        (Ok(_), Some(service)) => service.try_submit(operation).map(ReplacementReceiver::Session),
        (Ok(_), None) => Err(SessionLifecycleSubmitError::Unavailable),
        (Err(_), _) => Err(SessionLifecycleSubmitError::Full),
    };
    tokio::spawn(async move {
        let id = admitted.request_id;
        let response = match submission {
            Ok(receiver) => {
                let result = tokio::select! {
                    biased;
                    _ = child_response_settled(response_state) => return,
                    result = receiver.receive() => result,
                };
                match result {
                    Ok(result) if result["cancelled"] == true => Ok(serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result})),
                    Ok(result) => match rebind_parent(&pending, &issued, &mirror, parent_id, &admitted.owner) {
                        Ok(()) => Ok(serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result})),
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
