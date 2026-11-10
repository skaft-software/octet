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
            Self::Session(receiver) => receiver
                .await
                .unwrap_or(Err(ExtensionSessionLifecycleError::Unavailable))
                .map(|id| serde_json::json!({"session_id":id})),
            Self::Setup(receiver) => receiver
                .await
                .map_err(|_| ExtensionSessionLifecycleError::Unavailable)?
                .map_err(ExtensionSessionLifecycleError::Refused),
        }
    }
}

impl ExtensionProcess {
    /// Project the real mirror for this pinned setup process and live owner.
    pub fn replacement_context(
        &self,
        owner: &ExtensionResourceOwner,
    ) -> Result<serde_json::Value, ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let context = self.current_context_for_resource_owner(owner.session_id.clone());
        if context.resource_owner.as_ref() != Some(owner) {
            return Err(ExtensionRuntimeError::Protocol(
                "session setup process generation changed".into(),
            ));
        }
        let mut context = serde_json::to_value(context)
            .map_err(|e| ExtensionRuntimeError::Protocol(e.to_string()))?;
        if session_leaf::owner_routes_enabled(&read_std_lock(&connection.protocol)) {
            session_leaf::merge_routed_session_facts(&connection, owner, &mut context["host"])?;
        } else {
            session_leaf::attach_session_mirror(&connection, owner, &mut context["host"], false)?;
        }
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
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        Some(&self.resource_owner)
    }
}

pub(super) fn dispatch_setup(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let Some((request, admitted)) = admit_host_request::<SetupRequest>(
        state,
        object,
        "session/setup",
        EXTENSION_FEATURE_SESSION_CONTROL_V1,
        params,
    )?
    else {
        return Ok(());
    };
    // Setup is never a retained grant: every write must still have this live
    // command parent, the admitted process owner, and the native setup boundary.
    let live = lock_std_mutex(&state.pending)
        .get(&request.parent_request_id)
        .is_some_and(|parent| {
            parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                && parent.resource_owner.as_ref() == Some(&admitted.owner)
        });
    if !live
        || request.resource_owner != admitted.owner
        || lock_std_mutex(&state.tombstones).contains(request.parent_request_id)
    {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "session setup requires its live replacement command parent".into(),
            ),
        );
    }
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) {
        return refuse_admitted_request(state, &admitted, failure);
    }
    let completes = matches!(request.mutation, SetupMutation::Complete);
    let mutation = serde_json::to_value(request.mutation).map_err(|e| e.to_string())?;
    if serde_json::to_vec(&mutation)
        .map_err(|e| e.to_string())?
        .len()
        > 32768
    {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::InvalidRequest,
                "session setup mutation exceeds 32KiB".into(),
            ),
        );
    }
    let response_state = {
        let children = lock_std_mutex(&state.child_requests);
        let Some(child) = children.get(&admitted.request_id) else {
            return Ok(());
        };
        Arc::clone(&child.response_state)
    };
    let worker = state.child_work_slots.clone().try_acquire_owned();
    let submission = match (worker.as_ref(), state.session_lifecycle.as_ref()) {
        (Ok(_), Some(service)) => {
            service.try_submit_setup(ExtensionSessionLifecycleOperation::Setup {
                parent_request_id: request.parent_request_id,
                owner: admitted.owner.clone(),
                namespace: state.extension_identity.name.clone(),
                mutation,
            })
        }
        (Ok(_), None) => Err(SessionLifecycleSubmitError::Unavailable),
        (Err(_), _) => Err(SessionLifecycleSubmitError::Full),
    };
    let writer = state.writer.clone();
    let frame_limit = Arc::clone(&state.frame_limit);
    let children = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let pending = Arc::clone(&state.pending);
    let issued = Arc::clone(&state.issued_resource_owners);
    let mailbox = Arc::clone(&state.session_leaf);
    let routed = session_leaf::owner_routes_enabled(&read_std_lock(&state.protocol));
    let closed = Arc::clone(&state.closed);
    let draining = Arc::clone(&state.draining);
    tokio::spawn(async move {
        let id = admitted.request_id.clone();
        let response = match submission {
            Ok(receiver) => {
                let result = tokio::select! {
                    biased;
                    _ = child_response_settled(Arc::clone(&response_state)) => return,
                    result = receiver => result.unwrap_or(Err("session setup driver unavailable".into())),
                };
                match result {
                    Ok(mut result) if completes && routed => {
                        // Setup writes advanced the real replacement history.
                        // Re-pin the live parent to the publication that holds
                        // them (its receipt head) and lease that exact view.
                        let head = result["context"]["host"]["session_leaf_id"].clone();
                        let lease = Some(frame_limit.max_message_bytes());
                        let repin = async {
                            loop {
                                let changed =
                                    session_leaf::transport::publication_changed(&mailbox);
                                let current = lock_std_mutex(&mailbox.mirror)
                                    .as_ref()
                                    .and_then(|mirror| mirror.leaf_id());
                                if current.as_ref() == Some(&head) {
                                    if let Some(leased) = rebind_parent(
                                        (&pending, &issued),
                                        &mailbox,
                                        &children,
                                        &admitted,
                                        routed,
                                        (&closed, &draining),
                                        lease,
                                    )? {
                                        break Ok::<_, String>(leased);
                                    }
                                }
                                changed.await;
                            }
                        };
                        let leased = tokio::select! {
                            biased;
                            _ = child_response_settled(response_state) => return,
                            leased = repin => leased,
                        };
                        match leased {
                            Ok(leased) => {
                                if let Some(descriptor) = leased {
                                    result["session_snapshot"] = descriptor;
                                }
                                Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
                            }
                            Err(detail) => Ok(serde_json::json!({
                                "jsonrpc":"2.0", "id":id,
                                "error":ExtensionRequestFailure::NotForegroundOwner.error_object(&detail),
                            })),
                        }
                    }
                    Ok(result) => Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})),
                    Err(detail) => session_lifecycle_error(&id, "internal_error", &detail),
                }
            }
            Err(error) => session_lifecycle_submit_error(&id, error),
        };
        if let Ok(response) = response {
            deliver_session_lifecycle_response(
                children,
                id,
                writer,
                frame_limit,
                health,
                events,
                response,
            )
            .await;
        } else {
            settle_child_request(&children, &id);
        }
        drop(worker);
    });
    Ok(())
}

fn rebind_parent(
    bindings: (&PendingRequests, &IssuedResourceOwners),
    mailbox: &Arc<session_leaf::SessionLeafMailbox>,
    children: &ChildRequests,
    admitted: &AdmittedExtensionRequest,
    routed: bool,
    process: (&AtomicBool, &AtomicBool),
    lease: Option<usize>,
) -> Result<Option<Option<serde_json::Value>>, String> {
    let mut pending = lock_std_mutex(bindings.0);
    let mut issued = lock_std_mutex(bindings.1);
    let children = lock_std_mutex(children);
    let child = children
        .get(&admitted.request_id)
        .filter(|child| child.response_state.state.load(Ordering::Acquire) == CHILD_ACTIVE)
        .ok_or("replacement command child is no longer live")?;
    let parent_id = child.parent_request_id;
    let parent = pending
        .get_mut(&parent_id)
        .filter(|parent| parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
        .ok_or("replacement command parent is no longer live")?;
    if parent.resource_owner.as_ref() != Some(&admitted.owner)
        || process.0.load(Ordering::Acquire)
        || process.1.load(Ordering::Acquire)
    {
        return Err("replacement command parent owner changed".into());
    }
    let mut store = lock_std_mutex(&mailbox.transport);
    let mirror = lock_std_mutex(&mailbox.mirror);
    let mirror = mirror
        .as_ref()
        .ok_or("replacement session owner unavailable")?;
    let owner = &mirror.owner;
    if owner.extension_instance_id != admitted.owner.extension_instance_id
        || owner.process_generation != admitted.owner.process_generation
    {
        return Err("replacement command owner belongs to a different process".into());
    }
    if routed
        && !store
            .rebind_parent(parent_id, &admitted.owner, mirror, mailbox)
            .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    // The rebound parent receives a counted lease of exactly its pinned view,
    // so its first append receipt is validated against the same revision.
    let leased = match lease.filter(|_| routed) {
        Some(frame) => {
            let profile = mailbox.profile(frame).map_err(|error| error.to_string())?;
            let descriptor = store
                .lease_parent(parent_id, &profile)
                .map_err(|error| error.to_string())?;
            Some(serde_json::to_value(descriptor).map_err(|error| error.to_string())?)
        }
        None => None,
    };
    // Admission of subsequent requests still derives authority from the live
    // parent. Do not weaken it to accept any explicit owner supplied by Node.
    parent.resource_owner = Some(owner.clone());
    issued.insert(owner.clone());
    Ok(Some(leased))
}

pub(super) fn queue_replacement(
    state: &ProtocolReadState,
    admitted: AdmittedExtensionRequest,
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
    let routed = session_leaf::owner_routes_enabled(&read_std_lock(&state.protocol));
    let closed = Arc::clone(&state.closed);
    let draining = Arc::clone(&state.draining);
    let worker = state.child_work_slots.clone().try_acquire_owned();
    let setup = matches!(
        &operation,
        ExtensionSessionLifecycleOperation::CreateWithOptions {
            setup_parent: Some(_),
            ..
        }
    );
    let submission = match (worker.as_ref(), state.session_lifecycle.as_ref()) {
        (Ok(_), Some(service)) if setup => service
            .try_submit_setup(operation)
            .map(ReplacementReceiver::Setup),
        (Ok(_), Some(service)) => service
            .try_submit(operation)
            .map(ReplacementReceiver::Session),
        (Ok(_), None) => Err(SessionLifecycleSubmitError::Unavailable),
        (Err(_), _) => Err(SessionLifecycleSubmitError::Full),
    };
    tokio::spawn(async move {
        let id = admitted.request_id.clone();
        let response = match submission {
            Ok(receiver) => {
                let result = tokio::select! {
                    biased;
                    _ = child_response_settled(Arc::clone(&response_state)) => return,
                    result = receiver.receive() => result,
                };
                match result {
                    Ok(result) if result["cancelled"] == true => {
                        Ok(serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result}))
                    }
                    Ok(mut result) => {
                        // Setup parents are leased again at setup completion,
                        // after their writes; ordinary replacements lease now.
                        let lease = (!setup).then(|| frame_limit.max_message_bytes());
                        let rebind = async {
                            loop {
                                let changed = session_leaf::transport::publication_changed(&mirror);
                                if let Some(leased) = rebind_parent(
                                    (&pending, &issued),
                                    &mirror,
                                    &children,
                                    &admitted,
                                    routed,
                                    (&closed, &draining),
                                    lease,
                                )? {
                                    break Ok(leased);
                                }
                                changed.await;
                            }
                        };
                        let rebound: Result<Option<serde_json::Value>, String> = tokio::select! {
                            biased;
                            _ = child_response_settled(response_state) => return,
                            result = rebind => result,
                        };
                        match rebound {
                            Ok(leased) => {
                                if let Some(descriptor) = leased {
                                    result["session_snapshot"] = descriptor;
                                }
                                Ok(serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result}))
                            }
                            // This is the owner-scoped API 0.4 contract, not an
                            // API 0.3 error semantic. Always send its real refusal.
                            Err(detail) => Ok(serde_json::json!({
                                "jsonrpc":"2.0", "id":id,
                                "error":ExtensionRequestFailure::NotForegroundOwner.error_object(&detail),
                            })),
                        }
                    }
                    Err(ExtensionSessionLifecycleError::Cancelled) => Ok(serde_json::json!({
                        "jsonrpc":"2.0", "id":id, "result":{"cancelled":true}
                    })),
                    Err(ExtensionSessionLifecycleError::Unavailable) => session_lifecycle_error(
                        &id,
                        "internal_error",
                        "active-session lifecycle service is unavailable",
                    ),
                    Err(ExtensionSessionLifecycleError::Refused(reason)) => Ok(serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32000, "message": reason},
                    })),
                    Err(ExtensionSessionLifecycleError::Failed) => session_lifecycle_error(
                        &id,
                        "internal_error",
                        "active-session lifecycle operation failed",
                    ),
                }
            }
            Err(error) => session_lifecycle_submit_error(&id, error),
        };
        // API 0.4 typed errors must not pass through the canonical API 0.3
        // error table. Its ordinary bounded writer still atomically claims the
        // child response, so cancellation cannot produce a second terminal.
        let delivery = response.and_then(|response| {
            try_queue_child_response(
                &children,
                &id,
                &writer,
                frame_limit.max_message_bytes(),
                response,
            )
        });
        if let Err(message) = delivery {
            update_health(
                &health,
                ExtensionHealthState::Degraded,
                Some(message.clone()),
            );
            let _ = events.send(ExtensionEvent::Diagnostic { message });
            settle_child_request(&children, &id);
        }
        drop(worker);
    });
    Ok(())
}
