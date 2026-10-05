//! Reading protocol output and queueing agent-session, child and secret operations.

use super::*;

pub(super) struct ProtocolReadState {
    pub(super) pending: PendingRequests,
    pub(super) resources: Resources,
    pub(super) resource_cleanup_changed: Arc<Notify>,
    pub(super) issued_resource_owners: IssuedResourceOwners,
    pub(super) session_leaf: Arc<session_leaf::SessionLeafMailbox>,
    pub(super) remote_ui: Arc<RemoteUiMailbox>,
    pub(super) pending_changed: Arc<Notify>,
    pub(super) closed: Arc<AtomicBool>,
    pub(super) draining: Arc<AtomicBool>,
    pub(super) events: broadcast::Sender<ExtensionEvent>,
    pub(super) presentation_rate: StdMutex<PresentationUpdateRate>,
    pub(super) presentation_updates: Option<watch::Sender<Option<PresentationDispatch>>>,
    pub(super) presentation_sequence: AtomicU64,
    pub(super) generation: u64,
    pub(super) instance_id: String,
    pub(super) frame_limit: Arc<ProtocolFrameLimit>,
    pub(super) declared: ManifestContributions,
    pub(super) writer: mpsc::Sender<WriterFrame>,
    pub(super) child_requests: ChildRequests,
    pub(super) seen_child_request_ids: StdMutex<HashSet<ExtensionRequestId>>,
    pub(super) child_work_slots: Arc<Semaphore>,
    pub(super) tombstones: Arc<StdMutex<RequestTombstones>>,
    pub(super) protocol: Arc<StdRwLock<ExtensionNegotiatedProtocol>>,
    pub(super) api_v03_contract: Arc<StdRwLock<Option<api_v03::NegotiatedContract>>>,
    pub(super) provider_registry: Option<Arc<ExtensionProviderRegistry>>,
    pub(super) provider_owner: ExtensionProviderOwner,
    pub(super) provider_streams: ProviderStreams,
    pub(super) tool_catalog: Arc<StdRwLock<Vec<ToolDefinition>>>,
    pub(super) catalog_updates: mpsc::Sender<CatalogUpdateRequest>,
    pub(super) delegation_service: Arc<StdRwLock<Option<ExtensionDelegationService>>>,
    pub(super) session_lifecycle: Option<ExtensionSessionLifecycleService>,
    pub(super) event_bus: Option<Arc<ExtensionEventBus>>,
    pub(super) approval_store: Arc<ExtensionApprovalStore>,
    pub(super) secret_broker: Option<Arc<dyn ExtensionSecretBroker>>,
    pub(super) extension_identity: ExtensionIdentity,
    pub(super) allowed_secrets: Arc<BTreeSet<String>>,
    pub(super) health: Arc<StdRwLock<ConnectionHealth>>,
    pub(super) artifact_store: ArtifactStore,
    pub(super) child: Option<Arc<Mutex<Child>>>,
    pub(super) termination: Option<ProcessTerminationHandle>,
}

impl ProtocolReadState {
    pub(super) fn max_message_bytes(&self) -> usize {
        self.frame_limit.max_message_bytes()
    }
}

pub(super) enum AgentSessionOperation {
    Models {
        query: Option<String>,
        limit: usize,
    },
    Spawn {
        task_name: String,
        profile: Option<String>,
        fingerprint: Option<String>,
        message: String,
        idempotency_key: String,
        policy: Box<ExtensionAgentSessionPolicy>,
    },
    Message {
        target: String,
        message: String,
    },
    FollowUp {
        target: String,
        message: String,
    },
    List,
    Wait {
        timeout: Duration,
    },
    Interrupt {
        target: String,
    },
    Events {
        target: String,
        after_sequence: u64,
        timeout: Duration,
    },
    Stop {
        target: String,
    },
}

pub(super) async fn execute_agent_session_operation(
    service: ExtensionDelegationService,
    resource_owner: String,
    operation: AgentSessionOperation,
    cancellation: CancellationToken,
) -> Result<serde_json::Value, String> {
    match operation {
        AgentSessionOperation::Spawn {
            task_name,
            profile,
            fingerprint,
            message,
            idempotency_key,
            policy,
        } => service.spawn(
            &resource_owner,
            ExtensionDelegationSpawnRequest {
                task_name,
                profile,
                fingerprint,
                message,
                idempotency_key,
                policy: *policy,
            },
        ),
        AgentSessionOperation::Message { target, message } => {
            service
                .send_message(&resource_owner, &target, message)
                .await
        }
        AgentSessionOperation::FollowUp { target, message } => {
            service.follow_up(&resource_owner, &target, message).await
        }
        AgentSessionOperation::Models { query, limit } => {
            service.models(&resource_owner, query.as_deref(), limit)
        }
        AgentSessionOperation::List => service.list(&resource_owner),
        AgentSessionOperation::Wait { timeout } => {
            service.wait(&resource_owner, timeout, &cancellation).await
        }
        AgentSessionOperation::Interrupt { target } => {
            service.interrupt(&resource_owner, &target).await
        }
        AgentSessionOperation::Events {
            target,
            after_sequence,
            timeout,
        } => {
            service
                .events(
                    &resource_owner,
                    &target,
                    after_sequence,
                    timeout,
                    &cancellation,
                )
                .await
        }
        AgentSessionOperation::Stop { target } => service.stop(&resource_owner, &target),
    }
}

pub(super) fn queue_agent_session_operation(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    parent_request_id: u64,
    method: &'static str,
    operation: AgentSessionOperation,
    explicit_owner: Option<ExtensionResourceOwner>,
) -> Result<(), String> {
    let Some(registered) = register_agent_session_request(
        state,
        request_id.clone(),
        parent_request_id,
        method,
        explicit_owner,
    )?
    else {
        return Ok(());
    };
    let response_state = registered.response_state;
    let resource_owner = registered.resource_owner;
    let issued_resource_owners = Arc::clone(&state.issued_resource_owners);
    let closed = Arc::clone(&state.closed);
    let draining = Arc::clone(&state.draining);
    let owner_changed = Arc::clone(&state.pending_changed);
    let service = read_std_lock(&state.delegation_service).clone();
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            let delivery = try_queue_child_response(
                &state.child_requests,
                &request_id,
                &state.writer,
                state.max_message_bytes(),
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":request_id,
                    "error":{
                        "code":-32000,
                        "message":format!("extension child worker limit {MAX_CHILD_WORKERS} exceeded"),
                    },
                }),
            );
            if delivery.is_err() {
                settle_child_request(&state.child_requests, &request_id);
            }
            return Ok(());
        }
    };
    let writer = state.writer.clone();
    let child_requests = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let max_message_bytes = state.max_message_bytes();
    tokio::spawn(async move {
        let cancellation = CancellationToken::default();
        let result = if let (Some(service), Some(resource_owner)) = (service, resource_owner) {
            if closed.load(Ordering::Acquire)
                || draining.load(Ordering::Acquire)
                || !lock_std_mutex(&issued_resource_owners).contains(&resource_owner)
            {
                Err("child session owner retired before dispatch".to_owned())
            } else {
                let retired = async {
                    loop {
                        let changed = owner_changed.notified();
                        tokio::pin!(changed);
                        changed.as_mut().enable();
                        if closed.load(Ordering::Acquire)
                            || draining.load(Ordering::Acquire)
                            || !lock_std_mutex(&issued_resource_owners).contains(&resource_owner)
                        {
                            return;
                        }
                        changed.await;
                    }
                };
                tokio::select! {
                    biased;
                    _ = retired => {
                        cancellation.cancel();
                        Err("child session owner retired during dispatch".to_owned())
                    }
                    _ = child_response_settled(Arc::clone(&response_state)) => {
                        cancellation.cancel();
                        drop(worker);
                        return;
                    }
                    result = execute_agent_session_operation(
                        service,
                        resource_owner.session_id.clone(),
                        operation,
                        cancellation.clone(),
                    ) => result,
                }
            }
        } else {
            Err("agent session service is not bound to this host-owned resource owner".to_owned())
        };
        let response = match result {
            Ok(result) => serde_json::json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "result":result,
            }),
            Err(message) => serde_json::json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "error":{"code":-32002,"message":message},
            }),
        };
        let delivery = try_queue_child_response(
            &child_requests,
            &request_id,
            &writer,
            max_message_bytes,
            response,
        );
        if let Err(message) = delivery {
            update_health(
                &health,
                ExtensionHealthState::Degraded,
                Some(message.clone()),
            );
            let _ = events.send(ExtensionEvent::Diagnostic { message });
            settle_child_request(&child_requests, &request_id);
        }
        drop(worker);
    });
    Ok(())
}

/// Use the existing lifecycle queue and child-worker slot, retaining both native
/// cancellation and accounting settlement even after the reverse caller leaves.
pub(super) fn queue_session_compaction_operation(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    instructions: Option<String>,
    owner: ExtensionResourceOwner,
    parent_request_id: u64,
    callback: bool,
    response_state: Arc<ChildResponseState>,
) -> Result<(), String> {
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            return reject_typed_child_request(
                state,
                request_id,
                ExtensionRequestFailure::BoundsExceeded,
                "session compaction worker limit exceeded",
            )
        }
    };
    let authority = SessionCompactionAuthority {
        parent_request_id,
        callback,
        owner,
        issued: Arc::clone(&state.issued_resource_owners),
        closed: Arc::clone(&state.closed),
        draining: Arc::clone(&state.draining),
        response: Arc::clone(&response_state),
    };
    let service = state
        .session_lifecycle
        .as_ref()
        .expect("compaction admission checked its consumer");
    let cancellation = CancellationToken::default();
    let driver = Arc::clone(&service.state);
    let epoch = driver.epoch.load(Ordering::Acquire);
    let mut receiver = match service.try_submit_compaction(
        instructions,
        cancellation.clone(),
        authority.clone(),
    ) {
        Ok(receiver) => receiver,
        Err(error) => {
            return reject_typed_child_request(
                state,
                request_id,
                match error {
                    SessionLifecycleSubmitError::Full => ExtensionRequestFailure::BoundsExceeded,
                    SessionLifecycleSubmitError::Unavailable => {
                        ExtensionRequestFailure::NotForegroundOwner
                    }
                },
                match error {
                    SessionLifecycleSubmitError::Full => "session lifecycle queue is full",
                    SessionLifecycleSubmitError::Unavailable => {
                        "session compaction idle consumer is unavailable"
                    }
                },
            )
        }
    };
    let owner_changed = Arc::clone(&state.pending_changed);
    let writer = state.writer.clone();
    let child_requests = Arc::clone(&state.child_requests);
    let max_message_bytes = state.max_message_bytes();
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    tokio::spawn(async move {
        let retired = async {
            // Some legacy generation teardown paths clear child maps without a
            // waiter notification. Keep revocation bounded on this same worker.
            let mut tick = tokio::time::interval(Duration::from_millis(25));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let owner_change = owner_changed.notified();
                let driver_change = driver.changed.notified();
                tokio::pin!(owner_change, driver_change);
                owner_change.as_mut().enable();
                driver_change.as_mut().enable();
                if !authority.is_current()
                    || !driver.active.load(Ordering::Acquire)
                    || driver.epoch.load(Ordering::Acquire) != epoch
                {
                    return;
                }
                tokio::select! {
                    _ = owner_change => {},
                    _ = driver_change => {},
                    _ = tick.tick() => {},
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = child_response_settled(response_state) => {
                cancellation.cancel();
                // Never abandon a borrowed Agent summary before it accounts for
                // completed/uncertain provider work. The same permit stays held.
                let _ = receiver.await;
                return;
            }
            _ = retired => {
                cancellation.cancel();
                receiver.await
            }
            result = &mut receiver => result,
        };
        let result = result.unwrap_or_else(|_| {
            Err("session compaction consumer was lost; do not replay ambiguous work".into())
        });
        let result = match result {
            Ok(committed) if !authority.is_current()
                || !driver.active.load(Ordering::Acquire)
                || driver.epoch.load(Ordering::Acquire) != epoch => Err(format!("session compaction committed entry {} but its owner retired; retained, do not retry", committed.entry_id)),
            other => other,
        };
        let committed_id = result.as_ref().ok().map(|result| result.entry_id.clone());
        let response = match result {
            Ok(result) => serde_json::json!({"jsonrpc":"2.0","id":request_id,"result":result}),
            Err(mut message) => {
                // Error details are bounded too; do not invent a successful result.
                truncate_utf8(&mut message, MAX_LIFECYCLE_REASON_BYTES);
                serde_json::json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32603,"message":message}})
            }
        };
        let delivery = try_queue_child_response(
            &child_requests,
            &request_id,
            &writer,
            max_message_bytes,
            response,
        );
        if let Err(message) = delivery {
            // A real summary can exceed a small negotiated frame. Refuse, naming
            // the retained checkpoint, rather than truncating the summary or ACKing.
            let response = serde_json::json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32603,"message":match committed_id {
                Some(id) => format!("session compaction committed entry {id} but response delivery failed; retained, do not retry"),
                None => "session compaction response delivery failed; do not replay ambiguous work".into(),
            }}});
            if try_queue_child_response(
                &child_requests,
                &request_id,
                &writer,
                max_message_bytes,
                response,
            )
            .is_err()
            {
                settle_child_request(&child_requests, &request_id);
            }
            update_health(
                &health,
                ExtensionHealthState::Degraded,
                Some(message.clone()),
            );
            let _ = events.send(ExtensionEvent::Diagnostic { message });
        }
        drop(worker);
    });
    Ok(())
}

pub(super) fn api_v03_session_lifecycle_success(
    request_id: &ExtensionRequestId,
    session_id: String,
) -> Result<serde_json::Value, String> {
    let result = api_v03::SessionLifecycleResult { session_id };
    let result = serde_json::to_value(result).map_err(|error| error.to_string())?;
    api_v03::parse_session_lifecycle_result(result.clone()).map_err(|error| error.to_string())?;
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": result,
    });
    api_v03::parse_json_rpc_envelope(response.clone()).map_err(|error| error.to_string())?;
    Ok(response)
}

pub(super) fn api_v03_session_lifecycle_error(
    request_id: &ExtensionRequestId,
    semantic: &str,
    reason: &str,
) -> Result<serde_json::Value, String> {
    let error = api_v03::error_object(semantic, Some(serde_json::json!({ "reason": reason })))
        .map_err(|error| error.to_string())?;
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "error": error,
    });
    api_v03::parse_json_rpc_envelope(response.clone()).map_err(|error| error.to_string())?;
    Ok(response)
}

pub(super) fn api_v03_session_lifecycle_submit_error(
    request_id: &ExtensionRequestId,
    error: SessionLifecycleSubmitError,
) -> Result<serde_json::Value, String> {
    match error {
        SessionLifecycleSubmitError::Unavailable => api_v03_session_lifecycle_error(
            request_id,
            "internal_error",
            "active-session lifecycle service is unavailable",
        ),
        SessionLifecycleSubmitError::Full => api_v03_session_lifecycle_error(
            request_id,
            "resource_exhausted",
            "active-session lifecycle queue is full",
        ),
    }
}

/// Canonically serialize and atomically admit one API 0.3 child response. This
/// mirrors the connection-owned response path instead of using the legacy
/// best-effort writer queue, so cancellation cannot race a second terminal
/// response into the stream.
pub(super) async fn queue_api_v03_child_response(
    child_requests: &ChildRequests,
    request_id: &ExtensionRequestId,
    writer: &mpsc::Sender<WriterFrame>,
    frame_limit: &Arc<ProtocolFrameLimit>,
    response: serde_json::Value,
) -> Result<ChildResponseAdmission, String> {
    api_v03::parse_json_rpc_envelope(response.clone()).map_err(|error| error.to_string())?;
    let mut line = api_v03::canonical_json(&response)
        .map_err(|error| error.to_string())?
        .into_bytes();
    line.push(b'\n');
    if !frame_limit.accepts_message_bytes(line.len()) {
        line.fill(0);
        return Err(format!(
            "API 0.3 session lifecycle response exceeded {} bytes",
            frame_limit.max_message_bytes()
        ));
    }
    let line = ZeroizingBytes(line);
    loop {
        let response_state = {
            let children = lock_std_mutex(child_requests);
            let Some(child) = children.get(request_id) else {
                return Ok(ChildResponseAdmission::AlreadySettled);
            };
            Arc::clone(&child.response_state)
        };
        match response_state.state.compare_exchange(
            CHILD_ACTIVE,
            CHILD_RESPONDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                let mut claim = ChildResponseClaim {
                    child_requests: Arc::clone(child_requests),
                    id: request_id.clone(),
                    response_state,
                    admitted: false,
                    // This path is API 0.3-only. A response-admission failure
                    // resets the child to active; its owner settles it rather
                    // than emitting a noncanonical best-effort cancellation.
                    abort_cancel: None,
                };
                let (completed, completion) = oneshot::channel();
                let admission = writer.send(WriterFrame {
                    line: line.0.clone(),
                    state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                    completion: Some(completed),
                    bus_delivery: None,
                });
                tokio::pin!(admission);
                tokio::select! {
                    biased;
                    _ = host_shutdown_requested() => {
                        return Err("host is shutting down".into());
                    }
                    result = tokio::time::timeout(CONFIRMATION_RESPONSE_TIMEOUT, &mut admission) => {
                        result
                            .map_err(|_| "API 0.3 session lifecycle response admission timed out".to_owned())?
                            .map_err(|_| "extension writer closed".to_owned())?;
                    }
                }
                // Writer admission is the sole terminal outcome boundary.
                claim.mark_admitted();
                tokio::select! {
                    biased;
                    _ = host_shutdown_requested() => {
                        return Err("host is shutting down".into());
                    }
                    result = tokio::time::timeout(CONFIRMATION_RESPONSE_TIMEOUT, completion) => {
                        result
                            .map_err(|_| "API 0.3 session lifecycle response write timed out".to_owned())?
                            .map_err(|_| "extension writer closed".to_owned())?
                            .map_err(|error| pending_error(error, "request").to_string())?;
                    }
                };
                return Ok(ChildResponseAdmission::Queued);
            }
            Err(CHILD_RESPONDING) => {
                let changed = response_state.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if response_state.state.load(Ordering::Acquire) == CHILD_RESPONDING {
                    changed.await;
                }
            }
            Err(CHILD_SETTLED) => return Ok(ChildResponseAdmission::AlreadySettled),
            Err(_) => unreachable!("child response state has a fixed representation"),
        }
    }
}

pub(super) async fn deliver_api_v03_session_lifecycle_response(
    child_requests: ChildRequests,
    request_id: ExtensionRequestId,
    writer: mpsc::Sender<WriterFrame>,
    frame_limit: Arc<ProtocolFrameLimit>,
    health: Arc<StdRwLock<ConnectionHealth>>,
    events: broadcast::Sender<ExtensionEvent>,
    response: serde_json::Value,
) {
    if let Err(message) = queue_api_v03_child_response(
        &child_requests,
        &request_id,
        &writer,
        &frame_limit,
        response,
    )
    .await
    {
        update_health(
            &health,
            ExtensionHealthState::Degraded,
            Some(message.clone()),
        );
        let _ = events.send(ExtensionEvent::Diagnostic { message });
        settle_child_request(&child_requests, &request_id);
    }
}

pub(super) fn queue_api_v03_session_lifecycle_operation(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    operation: ExtensionSessionLifecycleOperation,
) -> Result<(), String> {
    let registered = register_unparented_api_v03_child_request(state, request_id.clone())?;
    queue_registered_session_lifecycle_operation(
        state,
        request_id,
        operation,
        registered.response_state,
    )
}

pub(super) fn queue_registered_session_lifecycle_operation(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    operation: ExtensionSessionLifecycleOperation,
    response_state: Arc<ChildResponseState>,
) -> Result<(), String> {
    let writer = state.writer.clone();
    let frame_limit = Arc::clone(&state.frame_limit);
    let child_requests = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let worker = state.child_work_slots.clone().try_acquire_owned();
    let submission = match worker.as_ref() {
        Ok(_) => state
            .session_lifecycle
            .as_ref()
            .map(|service| service.try_submit(operation))
            .unwrap_or(Err(SessionLifecycleSubmitError::Unavailable)),
        Err(_) => Err(SessionLifecycleSubmitError::Full),
    };

    let Ok(worker) = worker else {
        let response =
            api_v03_session_lifecycle_submit_error(&request_id, SessionLifecycleSubmitError::Full)?;
        tokio::spawn(deliver_api_v03_session_lifecycle_response(
            child_requests,
            request_id,
            writer,
            frame_limit,
            health,
            events,
            response,
        ));
        return Ok(());
    };

    tokio::spawn(async move {
        let response = match submission {
            Ok(receiver) => {
                let result = tokio::select! {
                    biased;
                    _ = child_response_settled(response_state) => return,
                    result = receiver => result.unwrap_or(Err(ExtensionSessionLifecycleError::Unavailable)),
                };
                match result {
                    Ok(session_id) => api_v03_session_lifecycle_success(&request_id, session_id),
                    Err(ExtensionSessionLifecycleError::Cancelled) => Ok(serde_json::json!({
                        "jsonrpc":"2.0", "id":request_id, "result":{"cancelled":true}
                    })),
                    Err(ExtensionSessionLifecycleError::Unavailable) => {
                        api_v03_session_lifecycle_error(
                            &request_id,
                            "internal_error",
                            "active-session lifecycle service is unavailable",
                        )
                    }
                    Err(ExtensionSessionLifecycleError::Failed) => api_v03_session_lifecycle_error(
                        &request_id,
                        "internal_error",
                        "active-session lifecycle operation failed",
                    ),
                }
            }
            Err(error) => api_v03_session_lifecycle_submit_error(&request_id, error),
        };
        match response {
            Ok(response) => {
                deliver_api_v03_session_lifecycle_response(
                    child_requests,
                    request_id,
                    writer,
                    frame_limit,
                    health,
                    events,
                    response,
                )
                .await;
            }
            Err(message) => {
                update_health(
                    &health,
                    ExtensionHealthState::Degraded,
                    Some(message.clone()),
                );
                let _ = events.send(ExtensionEvent::Diagnostic { message });
                settle_child_request(&child_requests, &request_id);
            }
        }
        drop(worker);
    });
    Ok(())
}

#[derive(Serialize)]
pub(super) struct SecretResult<'a> {
    pub(super) value: &'a str,
}

pub(super) fn queue_secret_lookup(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    request: ExtensionSecretGetRequest,
) -> Result<(), String> {
    let Some(registered) = register_child_request(
        state,
        request_id.clone(),
        Some(request.parent_request_id),
        methods::SECRET_GET,
    )?
    else {
        return Ok(());
    };
    if request.name.len() > MAX_EXTENSION_SECRET_NAME_BYTES
        || !state.allowed_secrets.contains(&request.name)
    {
        try_queue_child_response(
            &state.child_requests,
            &request_id,
            &state.writer,
            state.max_message_bytes(),
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "error":{"code":-32602,"message":"secret name is not declared by this extension"},
            }),
        )?;
        return Ok(());
    }
    let Some(resource_owner) = registered.resource_owner else {
        try_queue_child_response(
            &state.child_requests,
            &request_id,
            &state.writer,
            state.max_message_bytes(),
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "error":{
                    "code":-32002,
                    "message":"secret lookup requires a host-owned session context",
                },
            }),
        )?;
        return Ok(());
    };
    let Some(broker) = state.secret_broker.clone() else {
        try_queue_child_response(
            &state.child_requests,
            &request_id,
            &state.writer,
            state.max_message_bytes(),
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":request_id,
                "error":{"code":-32002,"message":"secret service is unavailable"},
            }),
        )?;
        return Ok(());
    };
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            try_queue_child_response(
                &state.child_requests,
                &request_id,
                &state.writer,
                state.max_message_bytes(),
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":request_id,
                    "error":{
                        "code":-32000,
                        "message":format!("extension child worker limit {MAX_CHILD_WORKERS} exceeded"),
                    },
                }),
            )?;
            return Ok(());
        }
    };
    let lookup = ExtensionSecretRequest {
        extension: state.extension_identity.clone(),
        resource_owner,
        parent_request_id: request.parent_request_id,
        name: request.name,
    };
    let response_state = registered.response_state;
    let child_requests = Arc::clone(&state.child_requests);
    let writer = state.writer.clone();
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let max_message_bytes = state.max_message_bytes();
    tokio::spawn(async move {
        let result = tokio::select! {
            result = broker.get_secret(lookup) => Some(result),
            _ = child_response_settled(response_state) => None,
        };
        let Some(result) = result else {
            drop(worker);
            return;
        };
        let delivery = match result {
            Ok(Some(secret)) => {
                let result = SecretResult {
                    value: secret.as_str(),
                };
                let line = serde_json::to_vec(&ChildSuccessResponse {
                    jsonrpc: "2.0",
                    id: &request_id,
                    result: &result,
                })
                .map_err(|error| error.to_string());
                match line {
                    Ok(line) => try_queue_child_response_line(
                        &child_requests,
                        &request_id,
                        &writer,
                        max_message_bytes,
                        line,
                    ),
                    Err(error) => Err(error),
                }
            }
            Ok(None) => try_queue_child_response(
                &child_requests,
                &request_id,
                &writer,
                max_message_bytes,
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":request_id,
                    "error":{"code":-32004,"message":"secret is unavailable"},
                }),
            ),
            Err(_) => {
                let _ = events.send(ExtensionEvent::Diagnostic {
                    message: "configured extension secret broker failed a lookup".into(),
                });
                try_queue_child_response(
                    &child_requests,
                    &request_id,
                    &writer,
                    max_message_bytes,
                    serde_json::json!({
                        "jsonrpc":"2.0",
                        "id":request_id,
                        "error":{"code":-32004,"message":"secret is unavailable"},
                    }),
                )
            }
        };
        if let Err(message) = delivery {
            update_health(
                &health,
                ExtensionHealthState::Degraded,
                Some(message.clone()),
            );
            let _ = events.send(ExtensionEvent::Diagnostic { message });
            settle_child_request(&child_requests, &request_id);
        }
        drop(worker);
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn read_protocol_stdout<R>(
    mut stdout: R,
    pending: PendingRequests,
    resources: Resources,
    resource_cleanup_changed: Arc<Notify>,
    issued_resource_owners: IssuedResourceOwners,
    session_leaf: Arc<session_leaf::SessionLeafMailbox>,
    remote_ui: Arc<RemoteUiMailbox>,
    pending_changed: Arc<Notify>,
    closed: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    events: broadcast::Sender<ExtensionEvent>,
    presentation_updates: watch::Sender<Option<PresentationDispatch>>,
    generation: u64,
    instance_id: String,
    frame_limit: Arc<ProtocolFrameLimit>,
    declared: ManifestContributions,
    writer: mpsc::Sender<WriterFrame>,
    child_requests: ChildRequests,
    child_work_slots: Arc<Semaphore>,
    tombstones: Arc<StdMutex<RequestTombstones>>,
    protocol: Arc<StdRwLock<ExtensionNegotiatedProtocol>>,
    api_v03_contract: Arc<StdRwLock<Option<api_v03::NegotiatedContract>>>,
    provider_registry: Option<Arc<ExtensionProviderRegistry>>,
    provider_owner: ExtensionProviderOwner,
    provider_streams: ProviderStreams,
    tool_catalog: Arc<StdRwLock<Vec<ToolDefinition>>>,
    catalog_updates: mpsc::Sender<CatalogUpdateRequest>,
    delegation_service: Arc<StdRwLock<Option<ExtensionDelegationService>>>,
    session_lifecycle: Option<ExtensionSessionLifecycleService>,
    event_bus: Option<Arc<ExtensionEventBus>>,
    approval_store: Arc<ExtensionApprovalStore>,
    secret_broker: Option<Arc<dyn ExtensionSecretBroker>>,
    extension_identity: ExtensionIdentity,
    allowed_secrets: Arc<BTreeSet<String>>,
    health: Arc<StdRwLock<ConnectionHealth>>,
    initialization_complete: Arc<AtomicBool>,
    initialization_changed: Arc<Notify>,
    artifact_store: ArtifactStore,
    child: Option<Arc<Mutex<Child>>>,
    termination: Option<ProcessTerminationHandle>,
) where
    R: AsyncRead + Unpin,
{
    let state = ProtocolReadState {
        pending,
        resources,
        resource_cleanup_changed,
        issued_resource_owners,
        session_leaf,
        remote_ui,
        pending_changed,
        closed,
        draining,
        events,
        presentation_rate: StdMutex::new(PresentationUpdateRate::default()),
        presentation_updates: Some(presentation_updates),
        presentation_sequence: AtomicU64::new(0),
        generation,
        instance_id,
        frame_limit,
        declared,
        writer,
        child_requests,
        seen_child_request_ids: StdMutex::new(HashSet::new()),
        child_work_slots,
        tombstones,
        protocol,
        api_v03_contract,
        provider_registry,
        provider_owner,
        provider_streams,
        tool_catalog,
        catalog_updates,
        delegation_service,
        session_lifecycle,
        event_bus,
        approval_store,
        secret_broker,
        extension_identity,
        allowed_secrets,
        health,
        artifact_store,
        child,
        termination,
    };
    let mut bus_attached = false;
    let mut read_buffer = [0_u8; 8192];
    let mut line = Vec::new();
    let result = 'stream: loop {
        let count = match stdout.read(&mut read_buffer).await {
            Ok(0) => {
                if line.is_empty() {
                    break 'stream Ok(());
                }
                break 'stream Err("stdout ended with an unterminated JSON message".into());
            }
            Ok(count) => count,
            Err(error) => break 'stream Err(error.to_string()),
        };
        for byte in &read_buffer[..count] {
            if *byte == b'\n' {
                let is_api_v03 =
                    read_std_lock(&state.protocol).version == EXTENSION_API_VERSION_0_3;
                if line.last() == Some(&b'\r') {
                    if is_api_v03 {
                        break 'stream Err("API 0.3 frames must end with exactly one LF".into());
                    }
                    line.pop();
                }
                if line.is_empty() {
                    if is_api_v03 {
                        break 'stream Err(
                            "API 0.3 frames must contain one canonical JSON value per LF delimiter"
                                .into(),
                        );
                    }
                    continue;
                }
                if let Err(error) = handle_protocol_line(&line, &state) {
                    break 'stream Err(error);
                }
                line.fill(0);
                line.clear();
                while !initialization_complete.load(Ordering::Acquire) {
                    let changed = initialization_changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if initialization_complete.load(Ordering::Acquire) {
                        break;
                    }
                    changed.await;
                }
                if !bus_attached {
                    if let Some(bus) = &state.event_bus {
                        if let Err(error) = bus.attach(&state) {
                            break 'stream Err(error.to_string());
                        }
                    }
                    bus_attached = true;
                }
            } else {
                line.push(*byte);
                if line.len() >= state.max_message_bytes() {
                    break 'stream Err(format!(
                        "stdout message exceeded {} bytes",
                        state.max_message_bytes()
                    ));
                }
            }
        }
    };

    state.closed.store(true, Ordering::Release);
    lock_std_mutex(&state.resources).retire_generation();
    state.resource_cleanup_changed.notify_one();
    state.session_leaf.clear();
    state.remote_ui.clear();
    lock_std_mutex(&state.issued_resource_owners).clear();
    lock_std_mutex(&state.child_requests).clear();
    if let Some(bus) = &state.event_bus {
        bus.remove(&state.instance_id, state.generation);
    }
    if let Some(registry) = &state.provider_registry {
        registry.remove_owner(&state.provider_owner);
    }
    lock_std_mutex(&state.provider_streams).clear();
    let message = match result {
        Ok(()) => "extension stdout closed".to_owned(),
        Err(message) => {
            let _ = state.events.send(ExtensionEvent::Diagnostic {
                message: message.clone(),
            });
            message
        }
    };
    let health_state = if state.draining.load(Ordering::Acquire) {
        ExtensionHealthState::Stopped
    } else {
        ExtensionHealthState::Crashed
    };
    update_health(
        &state.health,
        health_state,
        (health_state == ExtensionHealthState::Crashed).then(|| message.clone()),
    );
    fail_all_pending(
        &state.pending,
        &state.pending_changed,
        PendingError::Closed(message),
    );
    if health_state == ExtensionHealthState::Crashed {
        if let (Some(child), Some(termination)) = (state.child, state.termination) {
            reap_failed_extension(child, termination).await;
            lock_std_mutex(&state.resources).terminate_generation();
        }
    }
}

pub(super) fn presentation_update_owner(
    state: &ProtocolReadState,
    request: &PresentationUpdateRequest,
) -> Result<Option<ExtensionResourceOwner>, String> {
    match (&request.parent_request_id, &request.resource_owner) {
        (Some(_), Some(_)) => {
            Err("presentation update cannot combine parent_request_id and resource_owner".into())
        }
        (Some(parent_request_id), None) => lock_std_mutex(&state.pending)
            .get(parent_request_id)
            .map(|pending| pending.resource_owner.clone())
            .ok_or_else(|| {
                "presentation update references a stale or unknown host request".to_owned()
            }),
        (None, Some(owner)) => {
            if owner.extension_instance_id != state.instance_id
                || owner.process_generation != state.generation
            {
                return Err("presentation update resource owner is stale or foreign".into());
            }
            if owner.session_id.trim().is_empty()
                || owner.session_id.len() > 512
                || owner.session_id.chars().any(char::is_control)
            {
                return Err("presentation update resource owner is invalid".into());
            }
            if !lock_std_mutex(&state.issued_resource_owners).contains(owner) {
                return Err("presentation update resource owner is stale or foreign".into());
            }
            Ok(Some(owner.clone()))
        }
        (None, None) => Ok(None),
    }
}

pub(super) fn queue_api_v03_unknown_method(
    state: &ProtocolReadState,
    id: serde_json::Value,
    method: &str,
) -> Result<(), String> {
    let error = api_v03::error_object(
        "unknown_method",
        Some(serde_json::json!({"method": method})),
    )
    .map_err(|error| error.to_string())?;
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    });
    queue_writer_value(&state.writer, &state.frame_limit, response)
}
