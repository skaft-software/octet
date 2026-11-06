use super::*;
use crate::tool_inference::ToolInferenceRequest;
use std::future::Future;
use std::task::Poll;

pub(super) const FEATURE: &str = "nested_inference_v1";
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InferenceParams {
    parent_request_id: u64,
    inference_id: String,
    model: ModelSelector,
    request: InferenceContext,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSelector {
    provider: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InferenceContext {
    messages: Vec<octet_ai::Message>,
    system: Option<String>,
    max_output_tokens: Option<u64>,
    temperature: Option<f32>,
    reasoning: Option<String>,
}

fn inference_request(params: InferenceParams) -> Result<ToolInferenceRequest, String> {
    if !identifier(&params.inference_id)
        || !identifier(&params.model.provider)
        || !identifier(&params.model.id)
    {
        return Err("inference identifiers must be bounded nonempty identifiers".into());
    }
    if params.request.messages.len() > 4096
        || params.request.max_output_tokens == Some(0)
        || params
            .request
            .temperature
            .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
    {
        return Err("invalid inference context limits".into());
    }
    let reasoning = match params.request.reasoning.as_deref() {
        None | Some("off") => octet_ai::ReasoningConfig::Off,
        Some(value @ ("minimal" | "low" | "medium" | "high" | "xhigh" | "max")) => {
            let effort = serde_json::from_value(serde_json::json!(value))
                .map_err(|_| "invalid reasoning effort")?;
            octet_ai::ReasoningConfig::Effort(effort)
        }
        _ => return Err("unsupported inference reasoning effort".into()),
    };
    Ok(ToolInferenceRequest {
        provider: params.model.provider,
        model: params.model.id,
        request: octet_ai::Request {
            system: params.request.system,
            messages: params.request.messages,
            tools: Vec::new(),
            tool_choice: octet_ai::ToolChoice::None,
            max_output_tokens: params.request.max_output_tokens,
            temperature: params.request.temperature,
            stop: Vec::new(),
            reasoning,
            reasoning_mode: Default::default(),
            responses: None,
            output_format: Default::default(),
            output_modalities: Default::default(),
            compatibility: Default::default(),
            cache_retention: Default::default(),
            session_id: None,
        },
    })
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
}

fn error(id: &ExtensionRequestId, code: i64, message: &'static str) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

fn refuse(
    state: &ProtocolReadState,
    id: ExtensionRequestId,
    code: i64,
    message: &'static str,
) -> Result<(), String> {
    insert_child_request(state, id.clone(), None, None)?;
    let delivery = try_queue_child_response(
        &state.child_requests,
        &id,
        &state.writer,
        state.max_message_bytes(),
        error(&id, code, message),
    );
    if delivery.is_err() {
        settle_child_request(&state.child_requests, &id);
    }
    delivery.map(|_| ())
}

struct Parent {
    id: u64,
    terminal: Arc<AtomicU8>,
    pending: PendingRequests,
    changed: Arc<Notify>,
    closed: Arc<AtomicBool>,
}

impl Parent {
    fn live(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && lock_std_mutex(&self.pending)
                .get(&self.id)
                .is_some_and(|binding| {
                    Arc::ptr_eq(&binding.terminal, &self.terminal)
                        && binding.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                })
    }

    async fn settled(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.live() {
                return;
            }
            changed.await;
        }
    }
}

struct ChildGuard {
    children: ChildRequests,
    id: ExtensionRequestId,
    cancel: CancellationToken,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        settle_child_request(&self.children, &self.id);
    }
}

pub(super) fn cancel(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    if object.contains_key("id") {
        return Err("inference cancellation must be a notification".into());
    }
    require_feature(state, FEATURE)?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Cancel {
        id: ExtensionRequestId,
    }
    let request: Cancel =
        serde_json::from_value(params).map_err(|_| "invalid inference cancellation")?;
    let child = lock_std_mutex(&state.child_requests)
        .get(&request.id)
        .map(|child| (child.parent_request_id, Arc::clone(&child.response_state)));
    if let Some((parent_id, response)) = child {
        let parent_is_live = lock_std_mutex(&state.pending)
            .get(&parent_id)
            .is_some_and(|parent| {
                parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                    && parent.tool_call_policy_digest.is_some()
                    && !state.closed.load(Ordering::Acquire)
                    && parent.resource_owner.as_ref().is_some_and(|owner| {
                        owner.extension_instance_id == state.instance_id
                            && owner.process_generation == state.generation
                            && lock_std_mutex(&state.issued_resource_owners).contains(owner)
                    })
            });
        if parent_is_live
            && response.state.load(Ordering::Acquire) == CHILD_ACTIVE
            && lock_std_mutex(&response.inference_id).is_some()
        {
            if let Some(cancel) = lock_std_mutex(&response.composition_cancellation).as_ref() {
                cancel.cancel();
            }
        }
    }
    Ok(())
}

pub(super) fn dispatch(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, "provider/infer")?;
    {
        let protocol = read_std_lock(&state.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_4 || !protocol.supports(FEATURE) {
            return refuse(
                state,
                id,
                -32601,
                "nested inference requires negotiated API 0.4 nested_inference_v1",
            );
        }
    }
    if serde_json::to_vec(&params)
        .map_err(|_| "invalid inference request")?
        .len()
        > MAX_REQUEST_BYTES
    {
        return refuse(
            state,
            id,
            -32602,
            "inference request exceeds its byte limit",
        );
    }
    let params: InferenceParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(_) => return refuse(state, id, -32602, "invalid inference request"),
    };
    let parent_id = params.parent_request_id;
    let inference_id = params.inference_id.clone();
    let request = match inference_request(params) {
        Ok(request) => request,
        Err(_) => return refuse(state, id, -32602, "invalid inference request"),
    };
    let (registered, service, parent) = {
        let pending = lock_std_mutex(&state.pending);
        let binding = pending
            .get(&parent_id)
            .filter(|parent| {
                parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                    && parent.tool_call_policy_digest.is_some()
                    && !state.closed.load(Ordering::Acquire)
                    && parent.resource_owner.as_ref().is_some_and(|owner| {
                        owner.extension_instance_id == state.instance_id
                            && owner.process_generation == state.generation
                            && lock_std_mutex(&state.issued_resource_owners).contains(owner)
                    })
            })
            .and_then(|parent| {
                Some((
                    parent,
                    parent
                        .child_interaction_progress
                        .as_ref()?
                        .inference_service()?,
                ))
            });
        let Some((binding, service)) = binding else {
            drop(pending);
            return refuse(
                state,
                id,
                -32002,
                "inference requires a live model-tool parent and native inference service",
            );
        };
        let registered = insert_child_request(state, id.clone(), Some(parent_id), None)?;
        let parent = Parent {
            id: parent_id,
            terminal: Arc::clone(&binding.terminal),
            pending: Arc::clone(&state.pending),
            changed: Arc::clone(&state.pending_changed),
            closed: Arc::clone(&state.closed),
        };
        (registered, service, parent)
    };
    let cancellation = CancellationToken::default();
    *lock_std_mutex(&registered.response_state.inference_id) = Some(inference_id.clone());
    *lock_std_mutex(&registered.response_state.composition_cancellation) =
        Some(cancellation.clone());
    if registered.response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE || !parent.live() {
        cancellation.cancel();
        settle_child_request(&state.child_requests, &id);
        return Ok(());
    }
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            let delivery = try_queue_child_response(
                &state.child_requests,
                &id,
                &state.writer,
                state.max_message_bytes(),
                error(&id, -32002, "inference worker limit exceeded"),
            );
            if delivery.is_err() {
                settle_child_request(&state.child_requests, &id);
            }
            return delivery.map(|_| ());
        }
    };
    let response_state = registered.response_state;
    let writer = state.writer.clone();
    let children = Arc::clone(&state.child_requests);
    let max_message_bytes = state.max_message_bytes();
    tokio::spawn(async move {
        let _worker = worker;
        let _child = ChildGuard {
            children: Arc::clone(&children),
            id: id.clone(),
            cancel: cancellation.clone(),
        };
        let (updates, mut events) = mpsc::channel(64);
        let execute = service.stream(request, cancellation.clone(), Some(updates));
        tokio::pin!(execute);
        let fenced = std::future::poll_fn(|cx| {
            if !parent.live() || response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE {
                cancellation.cancel();
                return Poll::Ready(None);
            }
            execute.as_mut().poll(cx).map(Some)
        });
        tokio::pin!(fenced);
        let mut sequence = 0;
        let mut bytes = 0;
        let mut output_failed = false;
        let result = loop {
            tokio::select! {
                biased;
                _ = parent.settled() => return,
                _ = child_response_settled(Arc::clone(&response_state)) => return,
                result = &mut fenced => break match result { Some(result) => result, None => return },
                Some(event) = events.recv() => {
                    if !cancellation.is_cancelled() && !output_failed && queue_event(&writer, max_message_bytes, &inference_id, &event, &mut sequence, &mut bytes).is_err() {
                        output_failed = true;
                        cancellation.cancel();
                    }
                }
            }
        };
        while let Ok(event) = events.try_recv() {
            if !cancellation.is_cancelled()
                && !output_failed
                && queue_event(
                    &writer,
                    max_message_bytes,
                    &inference_id,
                    &event,
                    &mut sequence,
                    &mut bytes,
                )
                .is_err()
            {
                output_failed = true;
                cancellation.cancel();
            }
        }
        if !parent.live() || response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE {
            return;
        }
        let reply = if output_failed {
            error(&id, -32012, "inference output exceeds transport limits")
        } else if cancellation.is_cancelled() {
            serde_json::json!({"jsonrpc":"2.0", "id":id, "result":{"cancelled":true}})
        } else {
            match result {
                Ok(response) => serde_json::json!({"jsonrpc":"2.0", "id":id, "result":{
                    "message":response.message,"usage":response.usage,"cost":response.cost,"stop_reason":response.stop_reason,"timestamp":crate::session::now_unix_millis()
                }}),
                Err(_) => error(&id, -32002, "native inference failed or is unavailable"),
            }
        };
        if try_queue_child_response(&children, &id, &writer, max_message_bytes, reply).is_err() {
            settle_child_request(&children, &id);
        }
    });
    Ok(())
}

fn queue_event(
    writer: &mpsc::Sender<WriterFrame>,
    frame_limit: usize,
    inference_id: &str,
    event: &StreamEvent,
    sequence: &mut u64,
    bytes: &mut usize,
) -> Result<(), String> {
    let (kind, payload) = match event {
        StreamEvent::Started { .. } => ("started", serde_json::json!({})),
        StreamEvent::TextStart { index } => ("text_start", serde_json::json!({"index":index})),
        StreamEvent::TextDelta { index, delta } => (
            "text_delta",
            serde_json::json!({"index":index,"delta":delta}),
        ),
        StreamEvent::TextEnd { index } => ("text_end", serde_json::json!({"index":index})),
        StreamEvent::ReasoningStart { index } => {
            ("reasoning_start", serde_json::json!({"index":index}))
        }
        StreamEvent::ReasoningDelta { index, delta } => (
            "reasoning_delta",
            serde_json::json!({"index":index,"delta":delta}),
        ),
        StreamEvent::ReasoningEnd { index } => {
            ("reasoning_end", serde_json::json!({"index":index}))
        }
        StreamEvent::ProviderLifecycle(_) | StreamEvent::Usage(_) => return Ok(()),
        _ => return Err("unsupported nested inference event".into()),
    };
    let params = serde_json::json!({"inference_id":inference_id,"sequence":sequence,"kind":kind,"payload":payload});
    *bytes = bytes
        .checked_add(
            serde_json::to_vec(&params)
                .map_err(|_| "inference event serialization failed")?
                .len(),
        )
        .ok_or("inference output byte limit")?;
    if *bytes > MAX_OUTPUT_BYTES || *sequence >= 4096 {
        return Err("inference output limit exceeded".into());
    }
    *sequence += 1;
    queue_writer_line(
        writer,
        frame_limit,
        serde_json::to_vec(
            &serde_json::json!({"jsonrpc":"2.0","method":"provider/infer_event","params":params}),
        )
        .map_err(|_| "inference event serialization failed")?,
    )
}

#[cfg(test)]
mod tests;
