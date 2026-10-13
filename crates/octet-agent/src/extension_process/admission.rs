//! Host request admission, child requests, progress and stderr.

use super::*;

#[cfg(test)]
#[path = "editor_checkpoint_tests.rs"]
mod editor_checkpoint_tests;

/// One admitted Wave-1 owner-scoped request awaiting a foreground projection.
pub(super) struct AdmittedExtensionRequest {
    pub(super) request_id: ExtensionRequestId,
    pub(super) generation: u64,
    pub(super) owner: ExtensionResourceOwner,
}

/// The owner-carrying shape every Wave-1 request implements: the authoritative
/// parent host request plus an optional explicit owner for deferred calls.
pub(super) trait OwnerScopedHostRequest: serde::de::DeserializeOwned {
    /// Active host request that supplies the authoritative resource owner.
    fn parent_request_id(&self) -> u64;
    /// Explicit owner sent by a caller that outlived its host request.
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        None
    }
}

pub(super) fn reject_typed_child_request(
    state: &ProtocolReadState,
    request_id: ExtensionRequestId,
    failure: ExtensionRequestFailure,
    detail: impl Into<String>,
) -> Result<(), String> {
    // A refusal can land before the request was registered (feature gate, body
    // parse) or after (bounds, owner, no consumer). Reserve the child request
    // first so one response is always deliverable and never left pending.
    if !lock_std_mutex(&state.child_requests).contains_key(&request_id) {
        let _registered = insert_child_request(state, request_id.clone(), None, None)?;
    }
    let response =
        ExtensionRequestOutcome::Failed(failure, detail.into()).into_response(request_id.clone());
    let delivery = try_queue_child_response(
        &state.child_requests,
        &request_id,
        &state.writer,
        state.max_message_bytes(),
        response,
    );
    if delivery.is_err() {
        settle_child_request(&state.child_requests, &request_id);
    }
    delivery.map(|_| ())
}

pub(super) fn require_remote_ui(state: &ProtocolReadState) -> Result<(), String> {
    let protocol = read_std_lock(&state.protocol);
    if protocol.version != EXTENSION_API_VERSION_0_4
        || !protocol.supports(EXTENSION_FEATURE_REMOTE_UI)
        || !state.remote_ui.is_bound()
    {
        return Err("remote UI requires explicitly negotiated API 0.4 and a bound frontend".into());
    }
    if state.closed.load(Ordering::Acquire) || state.draining.load(Ordering::Acquire) {
        return Err("remote UI generation is closed or draining".into());
    }
    Ok(())
}

pub(super) fn validate_remote_ui_envelope(
    object: &serde_json::Map<String, serde_json::Value>,
    request: bool,
) -> Result<(), String> {
    if object.keys().any(|key| {
        !(matches!(key.as_str(), "jsonrpc" | "method" | "params") || request && key == "id")
    }) {
        return Err("remote UI envelope contains unknown fields".into());
    }
    Ok(())
}

pub(super) fn admit_remote_ui_request<T: OwnerScopedHostRequest>(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Result<Option<(T, AdmittedExtensionRequest)>, String> {
    let id = parse_child_request_id(object, method)?;
    if let Err(detail) = require_remote_ui(state) {
        reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::UnsupportedFeature,
            detail,
        )?;
        return Ok(None);
    }
    if let Err(detail) = validate_remote_ui_envelope(object, true) {
        reject_typed_child_request(state, id, ExtensionRequestFailure::InvalidRequest, detail)?;
        return Ok(None);
    }
    let Some((request, admitted)) =
        admit_host_request::<T>(state, object, method, EXTENSION_FEATURE_REMOTE_UI, params)?
    else {
        return Ok(None);
    };
    if lock_std_mutex(&state.tombstones).contains(request.parent_request_id()) {
        refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "remote UI parent was cancelled".into(),
            ),
        )?;
        return Ok(None);
    }
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) {
        refuse_admitted_request(state, &admitted, failure)?;
        return Ok(None);
    }
    Ok(Some((request, admitted)))
}

pub(super) fn dispatch_remote_ui_request(
    state: &ProtocolReadState,
    admitted: &AdmittedExtensionRequest,
    operation: ExtensionRemoteUiOperation,
) -> Result<(), String> {
    let mut children = lock_std_mutex(&state.child_requests);
    let Some(child) = children.get_mut(&admitted.request_id) else {
        return Ok(());
    };
    let parent = (child.parent_request_id != 0).then_some(child.parent_request_id);
    let reservation = state.remote_ui.reserve(
        admitted.request_id.clone(),
        admitted.owner.clone(),
        operation.clone(),
        parent,
    );
    match reservation {
        Ok(reservation) => child.remote_ui = Some(reservation),
        Err(failure) => {
            drop(children);
            return refuse_admitted_request(state, admitted, failure);
        }
    }
    drop(children);
    dispatch_host_request_event(state, admitted, |admitted| {
        ExtensionEvent::RemoteUiRequested {
            request_id: admitted.request_id.clone(),
            generation: admitted.generation,
            owner: admitted.owner.clone(),
            operation,
        }
    })?;
    state.remote_ui.wake();
    Ok(())
}

/// Admits one API `0.2` owner-scoped request: parse, feature gate, body parse,
/// then owner resolution.
///
/// `parent_request_id` is the primary carrier: while that host request is
/// active its owner is authoritative on the wire. A real extension often acts
/// later (an HTTP callback or server event that outlives the command that
/// started it), so the request may also carry an explicit, previously issued
/// [`ExtensionResourceOwner`]. An explicit owner is admitted only when it is
/// genuine for this process generation and session; every other case is refused
/// with a typed error and no state change.
///
/// Returns `Ok(None)` when the request was already answered with a typed
/// refusal, so the caller must stop without touching state. A request whose
/// parent carries no durable session owner is refused with
/// [`ExtensionRequestFailure::NotForegroundOwner`] rather than coerced.
pub(super) fn admit_host_request<T>(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
    feature: &str,
    params: serde_json::Value,
) -> Result<Option<(T, AdmittedExtensionRequest)>, String>
where
    T: OwnerScopedHostRequest,
{
    let request_id = parse_child_request_id(object, method)?;
    if !read_std_lock(&state.protocol).supports(feature) {
        reject_typed_child_request(
            state,
            request_id,
            ExtensionRequestFailure::UnsupportedFeature,
            format!("`{method}` requires the negotiated `{feature}` feature"),
        )?;
        return Ok(None);
    }
    let is_editor_checkpoint = matches!(method, methods::COMPOSER_SET)
        && params
            .get("editor_checkpoint")
            .is_some_and(|checkpoint| !checkpoint.is_null());
    let editor_checkpoint = validated_editor_checkpoint(state, method, &params);
    let request: T = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(error) => {
            reject_typed_child_request(
                state,
                request_id,
                ExtensionRequestFailure::InvalidRequest,
                format!("invalid {method} request: {error}"),
            )?;
            return Ok(None);
        }
    };
    let parent_request_id = request.parent_request_id();
    let parent_active = {
        let pending = lock_std_mutex(&state.pending);
        if is_editor_checkpoint && lock_std_mutex(&state.tombstones).contains(parent_request_id) {
            reject_typed_child_request(
                state,
                request_id,
                ExtensionRequestFailure::NotForegroundOwner,
                "editor checkpoint parent was cancelled",
            )?;
            return Ok(None);
        }
        pending
            .get(&parent_request_id)
            .is_some_and(|pending| pending.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
    };
    let owner = if parent_active {
        let retained = register_retained_editor_checkpoint(
            state,
            &request_id,
            parent_request_id,
            request.request_resource_owner(),
            editor_checkpoint.as_ref(),
        )?;
        let registered = if let Some(registered) = retained {
            registered
        } else {
            let Some(registered) =
                register_child_request(state, request_id.clone(), Some(parent_request_id), method)?
            else {
                return Ok(None);
            };
            registered
        };
        registered.resource_owner
    } else {
        match request.request_resource_owner() {
            Some(owner) => {
                let owner = match validate_explicit_request_owner(state, owner) {
                    Ok(owner) => owner,
                    Err(failure) => {
                        reject_typed_child_request(state, request_id, failure.0, failure.1)?;
                        return Ok(None);
                    }
                };
                // The originating host request already settled, so this request
                // is answered on its own lifetime instead of a dead parent's.
                let _registered = insert_child_request(state, request_id.clone(), None, None)?;
                Some(owner)
            }
            None => {
                reject_typed_child_request(
                    state,
                    request_id,
                    ExtensionRequestFailure::NotForegroundOwner,
                    format!(
                        "`{method}` carries no live parent request and no explicit resource owner"
                    ),
                )?;
                return Ok(None);
            }
        }
    };
    let Some(owner) = owner else {
        reject_typed_child_request(
            state,
            request_id,
            ExtensionRequestFailure::NotForegroundOwner,
            format!("`{method}` requires a foreground session owner"),
        )?;
        return Ok(None);
    };
    Ok(Some((
        request,
        AdmittedExtensionRequest {
            request_id,
            generation: state.generation,
            owner,
        },
    )))
}

/// Only a bounded, negotiated editor checkpoint can outlive an active parent.
/// Typed body parsing and the normal composer gate still run in admission.
fn validated_editor_checkpoint(
    state: &ProtocolReadState,
    method: &str,
    params: &serde_json::Value,
) -> Option<ExtensionEditorCheckpoint> {
    if !matches!(method, methods::COMPOSER_SET) || require_remote_ui(state).is_err() {
        return None;
    }
    let checkpoint: ExtensionEditorCheckpoint =
        serde_json::from_value(params.get("editor_checkpoint")?.clone()).ok()?;
    checkpoint.validate().ok()?;
    bounded_plain_text_failure(
        "composer text",
        params.get("text")?.as_str()?,
        MAX_EXTENSION_COMPOSER_TEXT_BYTES,
    )
    .ok()?;
    Some(checkpoint)
}

fn register_retained_editor_checkpoint(
    state: &ProtocolReadState,
    request_id: &ExtensionRequestId,
    parent_request_id: u64,
    explicit_owner: Option<&ExtensionResourceOwner>,
    checkpoint: Option<&ExtensionEditorCheckpoint>,
) -> Result<Option<RegisteredChildRequest>, String> {
    let (Some(owner), Some(checkpoint)) = (explicit_owner, checkpoint) else {
        return Ok(None);
    };
    // Hold the ordinary registration lock so only a still-active authoritative
    // parent can grant this lifetime. This is not a later shell commit guard.
    let pending = lock_std_mutex(&state.pending);
    let authoritative = pending
        .get(&parent_request_id)
        .filter(|pending| pending.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
        .and_then(|pending| pending.resource_owner.as_ref());
    if authoritative != Some(owner)
        || validate_explicit_request_owner(state, owner).is_err()
        || state
            .remote_ui
            .with_editor_checkpoint(owner, checkpoint, || Ok(()))
            .is_err()
    {
        return Ok(None);
    }
    // The mailbox proves an acknowledged editor and exact mount identity.
    // Commit rechecks these under the shared disposition; the product also
    // validates input/checkpoint clocks, foreground and native draft revision.
    // Keep the real wire parent and authoritative owner; only this child's
    // cancellation lifetime is independent of normal successful settlement.
    let mut registered = insert_child_request(state, request_id.clone(), None, None)?;
    registered.resource_owner = Some(owner.clone());
    drop(pending);
    Ok(Some(registered))
}

/// Validates one explicit resource owner against this process generation and
/// the owners actually issued on this wire.
pub(super) fn validate_explicit_request_owner(
    state: &ProtocolReadState,
    owner: &ExtensionResourceOwner,
) -> Result<ExtensionResourceOwner, (ExtensionRequestFailure, String)> {
    let stale = || {
        (
            ExtensionRequestFailure::NotForegroundOwner,
            "resource owner is stale or foreign to this process generation".to_owned(),
        )
    };
    if owner.extension_instance_id != state.instance_id
        || owner.process_generation != state.generation
    {
        return Err(stale());
    }
    if owner.session_id.trim().is_empty()
        || owner.session_id.len() > 512
        || owner.session_id.chars().any(char::is_control)
    {
        return Err((
            ExtensionRequestFailure::NotForegroundOwner,
            "resource owner session is invalid".to_owned(),
        ));
    }
    if !lock_std_mutex(&state.issued_resource_owners).contains(owner) {
        return Err((
            ExtensionRequestFailure::NotForegroundOwner,
            "resource owner was not issued to this extension process".to_owned(),
        ));
    }
    Ok(owner.clone())
}

/// Fans one admitted request out to the foreground consumer, answering with a
/// typed refusal when no consumer is subscribed instead of hanging.
pub(super) fn dispatch_host_request_event<F>(
    state: &ProtocolReadState,
    admitted: &AdmittedExtensionRequest,
    event: F,
) -> Result<(), String>
where
    F: FnOnce(&AdmittedExtensionRequest) -> ExtensionEvent,
{
    let event = event(admitted);
    if state.events.send(event).is_err() {
        reject_typed_child_request(
            state,
            admitted.request_id.clone(),
            ExtensionRequestFailure::UnsupportedFeature,
            "no active host consumer for this request",
        )?;
    }
    Ok(())
}

/// Rejects one admitted request with a typed failure and stops the arm.
pub(super) fn refuse_admitted_request(
    state: &ProtocolReadState,
    admitted: &AdmittedExtensionRequest,
    failure: (ExtensionRequestFailure, String),
) -> Result<(), String> {
    reject_typed_child_request(state, admitted.request_id.clone(), failure.0, failure.1)
}

pub(super) fn register_unparented_api_v03_child_request(
    state: &ProtocolReadState,
    id: ExtensionRequestId,
) -> Result<RegisteredChildRequest, String> {
    if read_std_lock(&state.protocol).version != EXTENSION_API_VERSION_0_3 {
        return Err("active-session lifecycle requests require extension API 0.3".into());
    }
    // API 0.3 session lifecycle requests are intentionally host-session scoped,
    // not children of a host tool request. They retain normal child-ID and
    // exactly-once response admission so $/cancelRequest can still win safely.
    insert_child_request(state, id, None, None)
}

pub(super) fn register_child_request(
    state: &ProtocolReadState,
    id: ExtensionRequestId,
    parent_request_id: Option<u64>,
    method: &str,
) -> Result<Option<RegisteredChildRequest>, String> {
    if read_std_lock(&state.protocol).version == EXTENSION_API_VERSION_0_1 {
        return insert_child_request(state, id, None, None).map(Some);
    }
    let parent = parent_request_id
        .ok_or_else(|| format!("{method} requires parent_request_id in API 0.2"))?;
    let pending = lock_std_mutex(&state.pending);
    let Some(parent_request) = pending
        .get(&parent)
        .filter(|pending| pending.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
    else {
        reserve_child_request_id(state, &id)?;
        drop(pending);
        // Parent settlement can legitimately win the wire race with an
        // extension-originated child request. Consume the child ID for this
        // generation and terminalize it when possible; this is not a fatal
        // framing or protocol violation.
        let _ = queue_writer_value(
            &state.writer,
            &state.frame_limit,
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{
                    "code":JSON_RPC_REQUEST_CANCELLED,
                    "message":"parent request is no longer active",
                    "data":{"parent_request_id":parent},
                },
            }),
        );
        return Ok(None);
    };
    let progress = parent_request.child_interaction_progress.clone();
    let resource_owner = parent_request.resource_owner.clone();
    let mut registered = insert_child_request(state, id, Some(parent), progress)?;
    registered.resource_owner = resource_owner;
    drop(pending);
    Ok(Some(registered))
}

pub(super) fn reserve_child_request_id(
    state: &ProtocolReadState,
    id: &ExtensionRequestId,
) -> Result<(), String> {
    let mut seen = lock_std_mutex(&state.seen_child_request_ids);
    if seen.len() >= MAX_EXTENSION_CHILD_REQUEST_IDS_PER_GENERATION {
        return Err(format!(
            "extension-originated request ID limit {MAX_EXTENSION_CHILD_REQUEST_IDS_PER_GENERATION} exceeded"
        ));
    }
    if !seen.insert(id.clone()) {
        return Err("reused extension-originated request id".into());
    }
    Ok(())
}

pub(super) fn insert_child_request(
    state: &ProtocolReadState,
    id: ExtensionRequestId,
    parent_request_id: Option<u64>,
    progress: Option<ToolProgressSink>,
) -> Result<RegisteredChildRequest, String> {
    let parent = parent_request_id.unwrap_or(0);
    let mut children = lock_std_mutex(&state.child_requests);
    if children.len() >= MAX_CHILD_REQUESTS {
        return Err(format!(
            "extension-originated request limit {MAX_CHILD_REQUESTS} exceeded"
        ));
    }
    reserve_child_request_id(state, &id)?;
    let response_state = Arc::new(ChildResponseState {
        state: AtomicU8::new(CHILD_ACTIVE),
        changed: Notify::new(),
        cancel_on_response_abort: StdMutex::new(None),
        composition_cancellation: StdMutex::new(None),
        session_leaf_cancel: StdMutex::new(None),
    });
    match children.entry(id) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(ChildRequest {
                exec_cancelled: false,
                parent_request_id: parent,
                response_state: Arc::clone(&response_state),
                policy_intent: None,
                remote_ui: None,
            });
        }
        std::collections::hash_map::Entry::Occupied(_) => {
            unreachable!("seen request IDs make an occupied child entry impossible");
        }
    }
    Ok(RegisteredChildRequest {
        parent_request_id,
        progress,
        resource_owner: None,
        response_state,
    })
}

pub(super) fn settle_child_request(
    child_requests: &ChildRequests,
    id: &ExtensionRequestId,
) -> bool {
    let child = lock_std_mutex(child_requests).remove(id);
    if let Some(child) = child {
        cancel_composition_work(&child.response_state);
        child
            .response_state
            .state
            .store(CHILD_SETTLED, Ordering::Release);
        child.response_state.changed.notify_waiters();
        true
    } else {
        false
    }
}

pub(super) async fn child_response_settled(state: Arc<ChildResponseState>) {
    loop {
        let changed = state.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if state.state.load(Ordering::Acquire) != CHILD_ACTIVE {
            return;
        }
        changed.await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildResponseAdmission {
    Queued,
    AlreadySettled,
}

pub(super) fn rollback_undelivered_artifact(
    store: &ArtifactStore,
    generation: u64,
    artifact_id: Option<&ArtifactId>,
    delivery: &Result<ChildResponseAdmission, String>,
) {
    if !matches!(delivery, Ok(ChildResponseAdmission::Queued)) {
        if let Some(artifact_id) = artifact_id {
            let _ = store.remove_artifact(generation, artifact_id);
        }
    }
}

// API 0.3 rejects deferred extension-originated requests before this legacy
// response path.
pub(super) fn try_queue_child_response(
    child_requests: &ChildRequests,
    id: &ExtensionRequestId,
    writer: &mpsc::Sender<WriterFrame>,
    max_message_bytes: usize,
    value: serde_json::Value,
) -> Result<ChildResponseAdmission, String> {
    let line = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    try_queue_child_response_line(child_requests, id, writer, max_message_bytes, line)
}

pub(super) fn try_queue_child_response_line(
    child_requests: &ChildRequests,
    id: &ExtensionRequestId,
    writer: &mpsc::Sender<WriterFrame>,
    max_message_bytes: usize,
    line: Vec<u8>,
) -> Result<ChildResponseAdmission, String> {
    // A secret lookup can reach either early-settled branch below. Keep the
    // serialized response zeroizing until writer ownership is established.
    let mut line = ZeroizingBytes(line);
    let response_state = {
        let children = lock_std_mutex(child_requests);
        let Some(child) = children.get(id) else {
            return Ok(ChildResponseAdmission::AlreadySettled);
        };
        Arc::clone(&child.response_state)
    };
    if response_state
        .state
        .compare_exchange(
            CHILD_ACTIVE,
            CHILD_RESPONDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return Ok(ChildResponseAdmission::AlreadySettled);
    }
    match queue_writer_line(writer, max_message_bytes, std::mem::take(&mut line.0)) {
        Ok(()) => {
            response_state.state.store(CHILD_SETTLED, Ordering::Release);
            let mut children = lock_std_mutex(child_requests);
            if children
                .get(id)
                .is_some_and(|child| Arc::ptr_eq(&child.response_state, &response_state))
            {
                children.remove(id);
            }
            response_state.changed.notify_waiters();
            Ok(ChildResponseAdmission::Queued)
        }
        Err(error) => {
            // Do not restore a dead parent's child if queue admission loses
            // a race with cancellation. Match the async response claim path.
            drop(ChildResponseClaim {
                child_requests: Arc::clone(child_requests),
                id: id.clone(),
                response_state,
                admitted: false,
                abort_cancel: None,
            });
            Err(error)
        }
    }
}

pub(super) fn cancel_children_from_reader(
    state: &ProtocolReadState,
    parent_request_id: u64,
    reason: &str,
) {
    let child_ids = cancel_active_children(&state.child_requests, parent_request_id, reason);
    for id in child_ids {
        let _ = queue_writer_value(
            &state.writer,
            &state.frame_limit,
            serde_json::json!({
                "jsonrpc":"2.0",
                "method":methods::CANCEL_REQUEST,
                "params":{"id":id,"reason":reason},
            }),
        );
    }
}

pub(super) fn cancel_active_children(
    child_requests: &ChildRequests,
    parent_request_id: u64,
    reason: &str,
) -> Vec<ExtensionRequestId> {
    let mut children = lock_std_mutex(child_requests);
    let matching = children
        .iter()
        .filter_map(|(id, child)| {
            (child.parent_request_id == parent_request_id)
                .then_some((id.clone(), Arc::clone(&child.response_state)))
        })
        .collect::<Vec<_>>();
    let mut settled = Vec::new();
    for (id, response_state) in matching {
        if let Some(cancel) = lock_std_mutex(&response_state.session_leaf_cancel).as_ref() {
            // The leaf receipt owns the definitive terminal response, including
            // when cancellation loses to the actual session commit claim.
            cancel.cancel();
            continue;
        }
        match response_state.state.load(Ordering::Acquire) {
            CHILD_ACTIVE
                if response_state
                    .state
                    .compare_exchange(
                        CHILD_ACTIVE,
                        CHILD_SETTLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok() =>
            {
                cancel_composition_work(&response_state);
                settled.push((id, response_state));
            }
            CHILD_RESPONDING => {
                *lock_std_mutex(&response_state.cancel_on_response_abort) = Some(reason.to_owned());
            }
            _ => {}
        }
    }
    for (id, response_state) in &settled {
        if children
            .get(id)
            .is_some_and(|child| Arc::ptr_eq(&child.response_state, response_state))
        {
            children.remove(id);
        }
        response_state.changed.notify_waiters();
    }
    settled.into_iter().map(|(id, _)| id).collect()
}

pub(super) fn dispatch_progress(
    state: &ProtocolReadState,
    notification: ExtensionProgressNotification,
) -> Result<(), String> {
    let (sink, owner, method) = {
        let mut pending = lock_std_mutex(&state.pending);
        let Some(request) = pending.get_mut(&notification.request_id) else {
            let _ = state.events.send(ExtensionEvent::Diagnostic {
                message: format!(
                    "ignored progress for inactive request {}",
                    notification.request_id
                ),
            });
            return Ok(());
        };
        if request
            .last_progress_sequence
            .is_some_and(|previous| notification.sequence <= previous)
        {
            let _ = state.events.send(ExtensionEvent::Diagnostic {
                message: format!(
                    "ignored non-monotonic progress sequence {} for request {}",
                    notification.sequence, notification.request_id
                ),
            });
            return Ok(());
        }
        request.last_progress_sequence = Some(notification.sequence);
        (
            request.progress.clone(),
            request.resource_owner.clone(),
            request.method.clone(),
        )
    };
    let Some(sink) = sink else {
        return Ok(());
    };
    match notification.event {
        ExtensionProgressEvent::PartialResult { result } => {
            require_feature(state, EXTENSION_FEATURE_CONTENT_PARTS)?;
            if method != methods::TOOL_CALL {
                return Err("partial results require an active tool/call".into());
            }
            let output = decode_progress_output(
                state,
                owner.as_ref().map(|owner| owner.session_id.as_str()),
                result,
            )
            .map_err(|error| error.to_string())?;
            sink.send_one(crate::tool::ToolProgress::PartialResult(Arc::new(output)));
        }
        ExtensionProgressEvent::Status {
            mut message,
            current,
            total,
            unit,
        } => {
            if current.is_some() || total.is_some() || unit.is_some() {
                use std::fmt::Write as _;
                message.push_str(" [");
                match (current, total) {
                    (Some(current), Some(total)) => {
                        let _ = write!(message, "{current}/{total}");
                    }
                    (Some(current), None) => {
                        let _ = write!(message, "{current}");
                    }
                    (None, Some(total)) => {
                        let _ = write!(message, "total {total}");
                    }
                    (None, None) => {}
                }
                if let Some(unit) = unit {
                    if current.is_some() || total.is_some() {
                        message.push(' ');
                    }
                    message.push_str(&unit);
                }
                message.push(']');
            }
            sink.status(message);
        }
        ExtensionProgressEvent::Output {
            stream,
            encoding,
            data,
        } => {
            let bytes = match encoding {
                ExtensionProgressEncoding::Utf8 => data.into_bytes(),
                ExtensionProgressEncoding::Base64 => base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|error| format!("invalid base64 progress output: {error}"))?,
            };
            sink.output(
                match stream {
                    ExtensionProgressStream::Stdout => OutputStream::Stdout,
                    ExtensionProgressStream::Stderr => OutputStream::Stderr,
                },
                bytes,
            );
        }
        ExtensionProgressEvent::Decoration { label, detail } => {
            require_feature(state, EXTENSION_FEATURE_PROGRESS_DECORATION)?;
            let decoration = ToolProgressDecoration::new(label, detail)
                .ok_or_else(|| "invalid bounded progress decoration".to_owned())?;
            sink.send_one(crate::tool::ToolProgress::Decoration(decoration));
        }
    }
    Ok(())
}

pub(super) fn artifact_publication(
    request: ArtifactPublishRequest,
) -> Result<ArtifactPublication, String> {
    let source = match (request.data, request.path) {
        (Some(data), None) if data.encoding == ExtensionProgressEncoding::Base64 => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.data)
                .map_err(|error| format!("invalid base64 artifact data: {error}"))?;
            ArtifactSource::Inline(bytes.into())
        }
        (Some(_), None) => return Err("inline artifact encoding must be base64".into()),
        (None, Some(path)) => ArtifactSource::ScratchPath(path),
        _ => return Err("artifact publication requires exactly one of data or path".into()),
    };
    Ok(ArtifactPublication {
        source,
        mime_type: request.mime_type,
        size: request.size,
        sha256: request.sha256,
    })
}

pub(super) fn queue_writer_value(
    writer: &mpsc::Sender<WriterFrame>,
    frame_limit: &ProtocolFrameLimit,
    value: serde_json::Value,
) -> Result<(), String> {
    let line = if frame_limit.api_v03 {
        api_v03::parse_json_rpc_envelope(value.clone()).map_err(|error| error.to_string())?;
        api_v03::canonical_frame(&value, frame_limit.max_frame_bytes())
            .map_err(|error| error.to_string())?
            .into_bytes()
    } else {
        serde_json::to_vec(&value).map_err(|error| error.to_string())?
    };
    queue_writer_line(writer, frame_limit.max_message_bytes(), line)
}

pub(super) fn queue_writer_line(
    writer: &mpsc::Sender<WriterFrame>,
    max_message_bytes: usize,
    mut line: Vec<u8>,
) -> Result<(), String> {
    line.push(b'\n');
    if line.len() > max_message_bytes {
        line.fill(0);
        return Err(format!("writer frame exceeded {max_message_bytes} bytes"));
    }
    writer
        .try_send(WriterFrame {
            line,
            state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
            completion: None,
            bus_delivery: None,
        })
        .map_err(|error| format!("bounded extension writer rejected frame: {error}"))
}

#[derive(Deserialize)]
pub(super) struct RpcErrorObject {
    pub(super) code: i64,
    pub(super) message: String,
    #[serde(default)]
    pub(super) data: Option<serde_json::Value>,
}

pub(super) async fn read_extension_stderr<R>(
    stderr: R,
    events: broadcast::Sender<ExtensionEvent>,
    max_message_bytes: usize,
) where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stderr);
    let mut bytes = vec![0_u8; max_message_bytes.clamp(1, 8192)];
    let mut buffered = Vec::new();
    let deliver = |line: &str| {
        // An adapter's own startup phase line is off-screen attribution, not a
        // diagnostic: while the host traces startup, forward it verbatim so a
        // harness sees host and child phases in one stream. Every other line
        // keeps the ordinary bounded diagnostic path.
        if forward_child_line(line) {
            return;
        }
        let _ = events.send(ExtensionEvent::Diagnostic {
            message: format!("extension stderr: {line}"),
        });
    };
    loop {
        let count = match reader.read(&mut bytes).await {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                let _ = events.send(ExtensionEvent::Diagnostic {
                    message: format!("extension stderr read failed: {error}"),
                });
                break;
            }
        };
        for byte in &bytes[..count] {
            if *byte == b'\n' || buffered.len() >= max_message_bytes.saturating_sub(1) {
                if !buffered.is_empty() {
                    deliver(&String::from_utf8_lossy(&buffered));
                    buffered.clear();
                }
            } else if *byte != b'\r' {
                buffered.push(*byte);
            }
        }
    }
    if !buffered.is_empty() {
        deliver(&String::from_utf8_lossy(&buffered));
    }
}

pub(super) fn fail_all_pending(
    pending: &PendingRequests,
    pending_changed: &Notify,
    error: PendingError,
) {
    let mut pending = lock_std_mutex(pending);
    for (_, request) in pending.drain() {
        request.terminal.store(REQUEST_COMPLETED, Ordering::Release);
        let _ = request.sender.send(Err(error.clone()));
    }
    drop(pending);
    pending_changed.notify_waiters();
}

pub(super) fn pending_error(error: PendingError, method: &str) -> ExtensionRuntimeError {
    match error {
        PendingError::Closed(message) => ExtensionRuntimeError::Closed(message),
        PendingError::Protocol(message) => ExtensionRuntimeError::Protocol(message),
        PendingError::Cancelled(reason) => ExtensionRuntimeError::Cancelled {
            method: method.to_owned(),
            reason,
        },
        PendingError::Remote {
            code,
            message,
            data,
        } => ExtensionRuntimeError::Remote {
            code,
            message,
            data,
        },
    }
}

pub(super) fn read_std_lock<T>(lock: &StdRwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn write_std_lock<T>(lock: &StdRwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn lock_std_mutex<T>(lock: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn require_declared(declared: bool, contribution: &str) -> Result<(), String> {
    if declared {
        Ok(())
    } else {
        Err(format!(
            "extension emitted undeclared {contribution} capability"
        ))
    }
}

#[cfg(unix)]
pub(super) fn extension_process_group_id(child: &Child) -> u64 {
    child.id().map(u64::from).unwrap_or(0)
}

/// Termination stub for targets that have no group signalling primitive.
/// It is only ever reached from the non-Unix arm of
/// [`terminate_registered_process_group`], which is itself `cfg(not(windows))`,
/// so this stub exists for exactly one platform shape: neither Unix nor Windows.
/// Windows is covered by job objects; Unix by `#[cfg(unix)]` signalling.
#[cfg(all(not(unix), not(windows)))]
pub(super) fn kill_process_group(_process_group_id: u64) {}

pub(super) fn validate_shortcut_definitions(
    definitions: &[ShortcutDefinition],
) -> Result<(), String> {
    if definitions.len() > MAX_EXTENSION_SHORTCUTS {
        return Err(format!(
            "shortcut catalog contains {} entries; limit is {MAX_EXTENSION_SHORTCUTS}",
            definitions.len()
        ));
    }
    let mut names = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for shortcut in definitions {
        validate_identifier("shortcut", &shortcut.name, true).map_err(|error| error.to_string())?;
        if shortcut.key.is_empty()
            || shortcut.key.len() > MAX_EXTENSION_SHORTCUT_KEY_BYTES
            || shortcut.key.trim() != shortcut.key
            || shortcut.key.chars().any(char::is_control)
        {
            return Err(format!("invalid shortcut key `{}`", shortcut.key));
        }
        if shortcut.description.trim().is_empty()
            || shortcut.description.len() > MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES
            || shortcut.description.chars().any(char::is_control)
        {
            return Err(format!(
                "shortcut `{}` has an invalid description",
                shortcut.name
            ));
        }
        if !names.insert(shortcut.name.as_str()) {
            return Err(format!("duplicate shortcut definition `{}`", shortcut.name));
        }
        if !keys.insert(shortcut.key.to_ascii_lowercase()) {
            return Err(format!("duplicate shortcut key `{}`", shortcut.key));
        }
    }
    Ok(())
}

pub(super) fn validate_identifiers(
    kind: &str,
    values: &[String],
    extended: bool,
) -> Result<(), ExtensionRuntimeError> {
    for value in values {
        validate_identifier(kind, value, extended)?;
    }
    validate_unique(kind, values)
}

pub(super) fn validate_identifier(
    kind: &str,
    value: &str,
    extended: bool,
) -> Result<(), ExtensionRuntimeError> {
    let mut characters = value.chars();
    let first = characters.next();
    let first_valid = if extended {
        first.is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
    } else {
        first.is_some_and(|character| character.is_ascii_lowercase())
    };
    let rest_valid = characters.all(|character| {
        if extended {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        } else {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }
    });
    if value.len() > 64 || !first_valid || !rest_valid {
        return Err(ExtensionRuntimeError::InvalidManifest(format!(
            "invalid {kind} identifier `{value}`"
        )));
    }
    Ok(())
}

pub(super) fn validate_unique<T>(kind: &str, values: &[T]) -> Result<(), ExtensionRuntimeError>
where
    T: Ord + std::fmt::Debug,
{
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(ExtensionRuntimeError::InvalidManifest(format!(
                "duplicate {kind} `{value:?}`"
            )));
        }
    }
    Ok(())
}

/// Validates one value against a manifest-declared CLI flag type and bound.
pub fn validate_extension_flag_value(
    flag: &ExtensionFlag,
    value: &serde_json::Value,
) -> Result<(), ExtensionRuntimeError> {
    match flag.kind {
        ExtensionFlagType::Boolean if value.is_boolean() => Ok(()),
        ExtensionFlagType::String => match value.as_str() {
            Some(value) if value.len() <= MAX_EXTENSION_FLAG_STRING_BYTES => Ok(()),
            Some(_) => Err(ExtensionRuntimeError::InvalidManifest(format!(
                "extension flag `{}` string value exceeds {MAX_EXTENSION_FLAG_STRING_BYTES} bytes",
                flag.name
            ))),
            None => Err(ExtensionRuntimeError::InvalidManifest(format!(
                "extension flag `{}` value must be a string",
                flag.name
            ))),
        },
        ExtensionFlagType::Integer => match value.as_i64() {
            Some(value) if value.unsigned_abs() <= api_v03::MAX_PORTABLE_JSON_INTEGER as u64 => {
                Ok(())
            }
            Some(_) => Err(ExtensionRuntimeError::InvalidManifest(format!(
                "extension flag `{}` integer value exceeds the portable JSON range",
                flag.name
            ))),
            None => Err(ExtensionRuntimeError::InvalidManifest(format!(
                "extension flag `{}` value must be an integer",
                flag.name
            ))),
        },
        ExtensionFlagType::Boolean => Err(ExtensionRuntimeError::InvalidManifest(format!(
            "extension flag `{}` value must be a boolean",
            flag.name
        ))),
    }
}

pub(super) fn validate_extension_flags(
    flags: &[ExtensionFlag],
) -> Result<(), ExtensionRuntimeError> {
    if flags.len() > MAX_EXTENSION_FLAGS {
        return Err(ExtensionRuntimeError::InvalidManifest(format!(
            "extension declares {} CLI flags; limit is {MAX_EXTENSION_FLAGS}",
            flags.len()
        )));
    }
    let mut names = BTreeSet::new();
    for flag in flags {
        validate_identifier("CLI flag", &flag.name, false)?;
        if !names.insert(&flag.name) {
            return Err(ExtensionRuntimeError::InvalidManifest(format!(
                "duplicate CLI flag `{}`",
                flag.name
            )));
        }
        if let Some(description) = &flag.description {
            if description.is_empty()
                || description.len() > MAX_EXTENSION_FLAG_DESCRIPTION_BYTES
                || description.chars().any(char::is_control)
            {
                return Err(ExtensionRuntimeError::InvalidManifest(format!(
                    "invalid CLI flag `{}` description",
                    flag.name
                )));
            }
        }
        validate_extension_flag_value(flag, &flag.default)?;
    }
    Ok(())
}

pub(super) fn resolve_extension_flag_values(
    manifest: &ExtensionManifest,
    supplied: &BTreeMap<String, serde_json::Value>,
) -> Result<BTreeMap<String, serde_json::Value>, ExtensionRuntimeError> {
    let declared = manifest
        .contributes
        .flags
        .iter()
        .map(|flag| (flag.name.as_str(), flag))
        .collect::<BTreeMap<_, _>>();
    for name in supplied.keys() {
        if !declared.contains_key(name.as_str()) {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "extension `{}` was given undeclared CLI flag `{name}`",
                manifest.name
            )));
        }
    }
    declared
        .into_iter()
        .map(|(name, flag)| {
            let value = supplied
                .get(name)
                .cloned()
                .unwrap_or_else(|| flag.default.clone());
            validate_extension_flag_value(flag, &value)?;
            Ok((name.to_owned(), value))
        })
        .collect()
}

/// Projects host-resolved CLI flag values into the API `0.2` initialize
/// payload, bounding every field on the way out.
///
/// The host is the sender here, so a flag value that cannot be projected fails
/// the initialize with a typed protocol error instead of being truncated or
/// silently dropped. API `0.1` and API `0.3` never use this projection: `0.1`
/// keeps its frozen payload byte-for-byte and `0.3` carries its own
/// `flag_values` field.
pub(super) fn projected_initialize_flag_values(
    api_version: &str,
    flag_values: &BTreeMap<String, serde_json::Value>,
) -> Result<Option<Vec<api_v03::InitializeFlagValue>>, ExtensionRuntimeError> {
    if !uses_api_0_2_capabilities(api_version) {
        return Ok(None);
    }
    if flag_values.len() > MAX_EXTENSION_FLAGS {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "extension flag projection carries {} flags; limit is {MAX_EXTENSION_FLAGS}",
            flag_values.len()
        )));
    }
    let mut projected = Vec::with_capacity(flag_values.len());
    for (name, value) in flag_values {
        if name.is_empty()
            || name.len() > MAX_EXTENSION_FLAG_DESCRIPTION_BYTES
            || name.chars().any(char::is_control)
        {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "extension flag name cannot be projected into API 0.2 initialize: `{name}`"
            )));
        }
        match value {
            serde_json::Value::Bool(_) => {}
            serde_json::Value::String(text) if text.len() <= MAX_EXTENSION_FLAG_STRING_BYTES => {}
            serde_json::Value::String(_) => {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "extension flag `{name}` string value exceeds {MAX_EXTENSION_FLAG_STRING_BYTES} bytes"
                )));
            }
            serde_json::Value::Number(number)
                if number.as_i64().is_some_and(|value| {
                    value.unsigned_abs() <= api_v03::MAX_PORTABLE_JSON_INTEGER as u64
                }) => {}
            _ => {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "extension flag `{name}` value cannot be projected into API 0.2 initialize"
                )));
            }
        }
        projected.push(api_v03::InitializeFlagValue {
            name: name.clone(),
            value: value.clone(),
        });
    }
    Ok(Some(projected))
}

pub(super) fn valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}
