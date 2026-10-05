//! API 0.4 reverse composition transport. All authority lives in the
//! request-scoped host dispatcher carried by a model-tool progress sink.

use super::*;
use std::future::Future;
use std::task::Poll;

const MAX_COMPOSITION_ARGUMENT_BYTES: usize = 128 * 1024;
const MAX_COMPOSITION_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPOSITION_FILES: usize = 256;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionRpcRequest {
    jsonrpc: String,
    id: ExtensionRequestId,
    method: CompositionMethod,
    params: serde_json::Value,
}

#[derive(Deserialize)]
enum CompositionMethod {
    #[serde(rename = "composition/context")]
    Context,
    #[serde(rename = "composition/call")]
    Call,
    #[serde(rename = "composition/store")]
    Store,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionContextRequest {
    parent_request_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionCallRequest {
    parent_request_id: u64,
    name: String,
    arguments: serde_json::Value,
    #[serde(default)]
    full_outcome: bool,
    #[serde(default)]
    updates: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionStoreRequest {
    parent_request_id: u64,
    set: serde_json::Map<String, serde_json::Value>,
    delete: Vec<String>,
}

enum CompositionOperation {
    Context,
    Call {
        name: String,
        arguments: serde_json::Value,
        full_outcome: bool,
        updates: bool,
    },
    Store {
        set: serde_json::Map<String, serde_json::Value>,
        delete: Vec<String>,
    },
}

enum CompositionResult {
    Context(serde_json::Value),
    Call(serde_json::Value),
    Store,
}

fn parse_composition_operation(
    method: CompositionMethod,
    params: serde_json::Value,
) -> Result<(u64, CompositionOperation), String> {
    match method {
        CompositionMethod::Context => {
            let request: CompositionContextRequest =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            Ok((request.parent_request_id, CompositionOperation::Context))
        }
        CompositionMethod::Call => {
            let request: CompositionCallRequest =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            if request.name.is_empty() || request.name.len() > 128 {
                return Err("composition tool name must be 1..=128 UTF-8 bytes".into());
            }
            if request.updates && !request.full_outcome {
                return Err("composition updates require full_outcome".into());
            }
            let arguments = request.arguments;
            if !request.full_outcome && !arguments.is_object() {
                return Err("composition arguments must be an object".into());
            }
            validate_composition_json(&arguments, MAX_COMPOSITION_ARGUMENT_BYTES)?;
            Ok((
                request.parent_request_id,
                CompositionOperation::Call {
                    name: request.name,
                    arguments,
                    full_outcome: request.full_outcome,
                    updates: request.updates,
                },
            ))
        }
        CompositionMethod::Store => {
            let request: CompositionStoreRequest =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            // Branch-store key/value budgets are host dispatcher policy; the
            // transport enforces its complete-frame bound and exact wire types.
            Ok((
                request.parent_request_id,
                CompositionOperation::Store {
                    set: request.set,
                    delete: request.delete,
                },
            ))
        }
    }
}

fn validate_composition_json(value: &serde_json::Value, limit: usize) -> Result<(), String> {
    fn visit(value: &serde_json::Value, depth: usize) -> Result<(), String> {
        match value {
            serde_json::Value::Array(values) => {
                if depth >= 32 {
                    return Err("composition JSON nesting exceeds 32 levels".into());
                }
                for value in values {
                    visit(value, depth + 1)?;
                }
            }
            serde_json::Value::Object(values) => {
                if depth >= 32 {
                    return Err("composition JSON nesting exceeds 32 levels".into());
                }
                for value in values.values() {
                    visit(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value, 0)?;
    serde_json::to_writer(&mut SchemaByteBudget(limit), value)
        .map_err(|_| format!("composition JSON exceeds {limit} bytes"))
}

fn composition_error(
    id: &ExtensionRequestId,
    code: i64,
    message: impl Into<String>,
) -> serde_json::Value {
    let mut message = message.into();
    truncate_utf8(&mut message, MAX_EXTENSION_REQUEST_ERROR_DETAIL_BYTES);
    serde_json::json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

fn refuse_composition(
    state: &ProtocolReadState,
    id: ExtensionRequestId,
    code: i64,
    message: impl Into<String>,
) -> Result<(), String> {
    insert_child_request(state, id.clone(), None, None)?;
    let delivery = try_queue_child_response(
        &state.child_requests,
        &id,
        &state.writer,
        state.max_message_bytes(),
        composition_error(&id, code, message),
    );
    if delivery.is_err() {
        settle_child_request(&state.child_requests, &id);
    }
    delivery.map(|_| ())
}

pub(super) fn cancel_composition_work(state: &ChildResponseState) {
    if let Some(cancellation) = lock_std_mutex(&state.composition_cancellation).as_ref() {
        cancellation.cancel();
    }
}

/// The original child response owns cancellation disposition, unlike ordinary
/// unsolicited cancellation notifications. Response admission remains exactly once.
pub(super) fn cancel_composition_request(
    state: &ProtocolReadState,
    id: &ExtensionRequestId,
) -> Result<bool, String> {
    let response_state = lock_std_mutex(&state.child_requests)
        .get(id)
        .map(|child| Arc::clone(&child.response_state));
    let Some(response_state) = response_state else {
        return Ok(false);
    };
    if lock_std_mutex(&response_state.composition_cancellation).is_none() {
        return Ok(false);
    }
    cancel_composition_work(&response_state);
    let delivery = try_queue_child_response(
        &state.child_requests,
        id,
        &state.writer,
        state.max_message_bytes(),
        composition_error(
            id,
            JSON_RPC_REQUEST_CANCELLED,
            "composition request cancelled",
        ),
    );
    if delivery.is_err() {
        settle_child_request(&state.child_requests, id);
    }
    delivery.map(|_| true)
}

struct CompositionParent {
    id: u64,
    terminal: Arc<AtomicU8>,
    pending: PendingRequests,
    changed: Arc<Notify>,
    closed: Arc<AtomicBool>,
}

impl CompositionParent {
    fn is_live(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && lock_std_mutex(&self.pending)
                .get(&self.id)
                .is_some_and(|parent| {
                    Arc::ptr_eq(&parent.terminal, &self.terminal)
                        && parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                })
    }

    async fn settled(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.is_live() {
                return;
            }
            changed.await;
        }
    }
}

struct CompositionChildGuard {
    children: ChildRequests,
    id: ExtensionRequestId,
}
impl Drop for CompositionChildGuard {
    fn drop(&mut self) {
        settle_child_request(&self.children, &self.id);
    }
}

struct CompositionCancellationGuard(CancellationToken);
impl Drop for CompositionCancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(super) fn dispatch_composition_request(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
) -> Result<(), String> {
    let id = parse_child_request_id(object, method)?;
    {
        let protocol = read_std_lock(&state.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_4
            || !protocol.supports(EXTENSION_FEATURE_TOOL_COMPOSITION)
        {
            return refuse_composition(
                state,
                id,
                -32601,
                "composition requires negotiated API 0.4 tool_composition_v1",
            );
        }
    }
    let request: CompositionRpcRequest =
        match serde_json::from_value(serde_json::Value::Object(object.clone())) {
            Ok(request) => request,
            Err(error) => {
                return refuse_composition(
                    state,
                    id,
                    -32602,
                    format!("invalid composition request: {error}"),
                );
            }
        };
    debug_assert_eq!(request.jsonrpc, "2.0");
    debug_assert_eq!(request.id, id);
    let (parent_id, operation) = match parse_composition_operation(request.method, request.params) {
        Ok(operation) => operation,
        Err(error) => {
            return refuse_composition(
                state,
                id,
                -32602,
                format!("invalid composition params: {error}"),
            );
        }
    };

    // Like register_child_request, hold pending through child insertion so
    // settlement cannot miss the child. Composition is stricter than legacy
    // reverse services: only a LIVE model-tool with an issued owner and a
    // request-scoped dispatcher can be a parent. Commands and hooks never qualify.
    let (registered, service, files, parent) = {
        let pending = lock_std_mutex(&state.pending);
        let binding = pending
            .get(&parent_id)
            .filter(|parent| {
                parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                    && parent.tool_call_policy_digest.is_some()
                    && parent.resource_owner.as_ref().is_some_and(|owner| {
                        owner.extension_instance_id == state.instance_id
                            && owner.process_generation == state.generation
                            && lock_std_mutex(&state.issued_resource_owners).contains(owner)
                    })
                    && !state.closed.load(Ordering::Acquire)
            })
            .and_then(|parent| {
                let service = parent
                    .child_interaction_progress
                    .as_ref()?
                    .composition_service()?;
                Some((parent, service))
            });
        let Some((binding, service)) = binding else {
            drop(pending);
            return refuse_composition(
                state,
                id,
                -32002,
                "composition requires a live model-tool parent with a host-owned resource owner and dispatcher",
            );
        };
        let registered = insert_child_request(state, id.clone(), Some(parent_id), None)?;
        let parent = CompositionParent {
            id: parent_id,
            terminal: Arc::clone(&binding.terminal),
            pending: Arc::clone(&state.pending),
            changed: Arc::clone(&state.pending_changed),
            closed: Arc::clone(&state.closed),
        };
        (
            registered,
            service,
            Arc::clone(&binding.composition_files),
            parent,
        )
    };
    let cancellation = CancellationToken::default();
    *lock_std_mutex(&registered.response_state.composition_cancellation) =
        Some(cancellation.clone());
    // Parent cancellation can win immediately after insertion, before token
    // binding. The worker's first poll fence below catches that race too.
    if registered.response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE || !parent.is_live()
    {
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
                composition_error(
                    &id,
                    -32002,
                    format!("composition child worker limit {MAX_CHILD_WORKERS} exceeded"),
                ),
            );
            if delivery.is_err() {
                settle_child_request(&state.child_requests, &id);
            }
            return delivery.map(|_| ());
        }
    };
    let worker = Arc::new(worker);
    let response_state = registered.response_state;
    let writer = state.writer.clone();
    let children = Arc::clone(&state.child_requests);
    let store = state.artifact_store.clone();
    let generation = state.generation;
    let max_message_bytes = state.max_message_bytes();
    let events = state.events.clone();
    tokio::spawn(async move {
        let _worker = worker;
        let _cancellation = CompositionCancellationGuard(cancellation.clone());
        let _child = CompositionChildGuard {
            children: Arc::clone(&children),
            id: id.clone(),
        };
        let wants_updates = matches!(&operation, CompositionOperation::Call { updates: true, .. });
        let (update_tx, mut update_rx) = mpsc::channel(64);
        let execute = async {
            match operation {
                CompositionOperation::Context => {
                    service.context().await.map(CompositionResult::Context)
                }
                CompositionOperation::Call { name, arguments, full_outcome, updates: _ } => {
                    if full_outcome { service.call_outcome_with_updates(name, arguments, cancellation.clone(), wants_updates.then_some(update_tx)).await }
                    else { service.call(name, arguments, cancellation.clone()).await }
                }.map(CompositionResult::Call),
                CompositionOperation::Store { set, delete } => service
                    .store(set, delete)
                    .await
                    .map(|()| CompositionResult::Store),
            }
        };
        tokio::pin!(execute);
        // Check BOTH parent membership/terminal and child state before every
        // dispatcher poll. A parent can have been removed before its child
        // notification is published. Never poll an already-lost binding in
        // the spawn race; do not hold host mutexes while polling nested tools.
        let fenced = std::future::poll_fn(|cx| {
            if cancellation.is_cancelled()
                || !parent.is_live()
                || response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE
            {
                cancellation.cancel();
                return Poll::Ready(None);
            }
            execute.as_mut().poll(cx).map(Some)
        });
        tokio::pin!(fenced);
        let mut sequence = 0;
        let result = loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => { cancellation.cancel(); return; },
                _ = child_response_settled(Arc::clone(&response_state)) => { cancellation.cancel(); return; },
                _ = parent.settled() => { cancellation.cancel(); return; },
                result = &mut fenced => break match result { Some(result) => result, None => return },
                Some(update) = update_rx.recv(), if wants_updates => {
                    if !parent.is_live() || response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE { return; }
                    sequence += 1;
                    if let Err(message) = queue_composition_update(&writer, max_message_bytes, &id, sequence, update) {
                        // Report the bounded transport refusal on the actual reverse call;
                        // dropping its scope cancels execution, not an invented tool result.
                        let delivery = try_queue_child_response(&children, &id, &writer,
                            max_message_bytes, composition_error(&id, -32602, message));
                        if delivery.is_err() { settle_child_request(&children, &id); }
                        return;
                    }
                },
            }
        };
        while let Ok(update) = update_rx.try_recv() {
            if !parent.is_live() || cancellation.is_cancelled() || response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE { return; }
            sequence += 1;
            if let Err(message) = queue_composition_update(&writer, max_message_bytes, &id, sequence, update) {
                // Report the bounded transport refusal on the actual reverse call;
                // dropping its scope cancels execution, not an invented tool result.
                let delivery = try_queue_child_response(&children, &id, &writer,
                    max_message_bytes, composition_error(&id, -32602, message));
                if delivery.is_err() { settle_child_request(&children, &id); }
                return;
            }
        }
        let encoded = match result {
            Ok(result) => {
                let store = store.clone();
                let files = Arc::clone(&files);
                let encoding_cancellation = cancellation.clone();
                let encoding_worker = Arc::clone(&_worker);
                let id = id.clone();
                let encoding = tokio::task::spawn_blocking(move || {
                    // A cancelled async waiter must not release the work slot
                    // while an already-started bounded disk job is still running.
                    let _worker = encoding_worker;
                    encode_composition_response(
                        &id,
                        result,
                        max_message_bytes,
                        &store,
                        generation,
                        &files,
                        &encoding_cancellation,
                    )
                });
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => { cancellation.cancel(); return; },
                    _ = child_response_settled(Arc::clone(&response_state)) => { cancellation.cancel(); return; },
                    _ = parent.settled() => { cancellation.cancel(); return; },
                    encoded = encoding => encoded.unwrap_or_else(|_| Err("composition response encoding failed".into())),
                }
            }
            Err(error) => Err(error.message),
        };
        if !parent.is_live() || cancellation.is_cancelled() {
            cancellation.cancel();
            return;
        }
        let (line, mut file) = match encoded {
            Ok(encoded) => encoded,
            Err(message) => (
                serde_json::to_vec(&composition_error(&id, -32002, message))
                    .expect("JSON value serializes"),
                None,
            ),
        };
        let delivery =
            try_queue_child_response_line(&children, &id, &writer, max_message_bytes, line);
        if matches!(delivery, Ok(ChildResponseAdmission::Queued)) {
            if let Some(file) = file.as_mut() {
                lock_std_mutex(&files.paths).push(file.path.clone());
                file.keep = true;
            }
        } else if let Err(message) = delivery {
            settle_child_request(&children, &id);
            let _ = events.send(ExtensionEvent::Diagnostic { message });
        }
    });
    Ok(())
}

fn queue_composition_update(writer: &mpsc::Sender<WriterFrame>, max_message_bytes: usize,
    id: &ExtensionRequestId, sequence: u64, result: serde_json::Value) -> Result<(), String> {
    let value = serde_json::json!({"jsonrpc":"2.0","method":"composition/update",
        "params":{"request_id":id,"sequence":sequence,"result":result}});
    let line = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    queue_writer_line(writer, max_message_bytes, line)
}

/// File count and cleanup belong to one pending model-tool, not the process or
/// a caller-supplied owner. No ExtensionProcess/ProcessConnection is retained.
#[derive(Default)]
pub(super) struct CompositionFiles {
    count: AtomicUsize,
    paths: StdMutex<Vec<PathBuf>>,
}

impl Drop for CompositionFiles {
    fn drop(&mut self) {
        for path in lock_std_mutex(&self.paths).drain(..) {
            let _ = crate::secure_fs::remove_regular_file_if_exists(&path);
        }
    }
}

#[derive(Debug)]
struct CompositionFile {
    path: PathBuf,
    keep: bool,
}

impl Drop for CompositionFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = crate::secure_fs::remove_regular_file_if_exists(&self.path);
        }
    }
}

struct CompositionJsonBuffer(Vec<u8>);
impl Write for CompositionJsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_COMPOSITION_FILE_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "composition result exceeds 8 MiB; reduce or filter the nested tool output",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_composition_response(
    id: &ExtensionRequestId,
    result: CompositionResult,
    max_message_bytes: usize,
    store: &ArtifactStore,
    generation: u64,
    files: &CompositionFiles,
    cancellation: &CancellationToken,
) -> Result<(Vec<u8>, Option<CompositionFile>), String> {
    if cancellation.is_cancelled() {
        return Err("composition request cancelled".into());
    }
    let (value, file_field, call) = match result {
        CompositionResult::Context(value) => (value, "context_file", false),
        CompositionResult::Call(value) => (value, "value_file", true),
        CompositionResult::Store => (serde_json::json!({}), "context_file", false),
    };
    let mut raw = CompositionJsonBuffer(Vec::new());
    serde_json::to_writer(&mut raw, &value).map_err(|error| error.to_string())?;
    let result = if call {
        serde_json::json!({"value":value})
    } else {
        value
    };
    let envelope = serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result});
    // An envelope adds only a small fixed overhead to the bounded JSON value.
    let inline = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    if inline.len().saturating_add(1) <= max_message_bytes {
        return Ok((inline, None));
    }
    if files
        .count
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < MAX_COMPOSITION_FILES).then_some(count + 1)
        })
        .is_err()
    {
        return Err(
            "composition scratch file limit 256 per parent exceeded; reduce the returned output"
                .into(),
        );
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random)
        .map_err(|error| format!("composition scratch allocation failed: {error}"))?;
    let basename = format!("composition-{}.json", crate::tool::content_hash(&random));
    let path = store
        .scratch_directory(generation)
        .map_err(|error| error.to_string())?
        .join(&basename);
    let mut output = crate::secure_fs::create_regular_file_for_append(&path)
        .map_err(|error| format!("composition scratch allocation failed: {error}"))?;
    let file = CompositionFile { path, keep: false };
    for chunk in raw.0.chunks(64 * 1024) {
        if cancellation.is_cancelled() {
            return Err("composition request cancelled".into());
        }
        output
            .write_all(chunk)
            .map_err(|error| format!("composition scratch write failed: {error}"))?;
    }
    drop(output);
    if cancellation.is_cancelled() {
        return Err("composition request cancelled".into());
    }
    let metadata = serde_json::json!({"path":basename,"bytes":raw.0.len(),"sha256":crate::tool::content_hash(&raw.0)});
    let response = serde_json::json!({"jsonrpc":"2.0", "id":id, "result":{file_field:metadata}});
    let line = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
    if line.len().saturating_add(1) > max_message_bytes {
        return Err("composition file response exceeds the negotiated frame limit".into());
    }
    Ok((line, Some(file)))
}

#[cfg(test)]
mod tests;
