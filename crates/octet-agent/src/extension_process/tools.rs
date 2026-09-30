//! Catalog updates, supervision and the tools an extension process provides.

use super::*;

pub(super) async fn run_catalog_updates(
    inner: Weak<ExtensionProcessInner>,
    mut updates: mpsc::Receiver<CatalogUpdateRequest>,
) {
    while let Some(update) = updates.recv().await {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let mut committed_catalog = None;
        let result = match wait_for_dynamic_registration(&inner).await {
            Err(message) => Err(message),
            Ok(registration) => {
                let _reload = inner.reload_guard.lock().await;
                let process = ExtensionProcess {
                    inner: Arc::clone(&inner),
                };
                let active = read_std_lock(&inner.connection).clone();
                let active_request = active.generation == update.generation
                    && Arc::ptr_eq(&active.tool_catalog, &update.catalog)
                    && !active.draining.load(Ordering::Acquire);
                if !active_request {
                    Err(
                        "tool catalog request belongs to an inactive extension generation"
                            .to_owned(),
                    )
                } else {
                    let mut next = read_std_lock(&update.catalog).clone();
                    match update.mutation {
                        CatalogMutation::Register(definitions) => {
                            for definition in definitions {
                                if let Some(existing) = next
                                    .iter_mut()
                                    .find(|existing| existing.name == definition.name)
                                {
                                    *existing = definition;
                                } else {
                                    next.push(definition);
                                }
                            }
                        }
                        CatalogMutation::Unregister(names) => {
                            let removed = names.into_iter().collect::<BTreeSet<_>>();
                            next.retain(|definition| !removed.contains(&definition.name));
                        }
                    }
                    validate_tool_definitions(&next, EXTENSION_API_VERSION_0_2)
                        .map_err(|error| error.to_string())
                        .and_then(|()| {
                            let process_tools = process.process_tools(Arc::clone(&active), &next);
                            let revision_stamp = Arc::clone(&process_tools.revision);
                            let reservation = registration.reserve(process_tools.tools)?;
                            let revision = active
                                .catalog_revision
                                .load(Ordering::Acquire)
                                .saturating_add(1);
                            let (_, published) = reservation.commit_with(|_, published| {
                                revision_stamp.store(revision, Ordering::Release);
                                let _catalog = write_std_lock(&active.catalog_guard);
                                *write_std_lock(&update.catalog) = next
                                    .iter()
                                    .filter(|definition| published.contains(&definition.name))
                                    .cloned()
                                    .collect();
                                active.catalog_revision.store(revision, Ordering::Release);
                            })?;
                            next.retain(|definition| published.contains(&definition.name));
                            *write_std_lock(&update.catalog) = next.clone();
                            committed_catalog = Some((registration.clone(), Arc::clone(&active)));
                            Ok(ToolCatalogUpdateResponse {
                                revision,
                                tools: next.into_iter().map(|definition| definition.name).collect(),
                            })
                        })
                }
            }
        };
        let response = match result {
            Ok(result) => serde_json::json!({
                "jsonrpc":"2.0",
                "id":update.request_id,
                "result":result,
            }),
            Err(message) => serde_json::json!({
                "jsonrpc":"2.0",
                "id":update.request_id,
                "error":{"code":-32602,"message":message},
            }),
        };
        let delivery = try_queue_child_response(
            &update.child_requests,
            &update.request_id,
            &update.writer,
            update.max_message_bytes,
            response,
        );
        if !matches!(delivery, Ok(ChildResponseAdmission::Queued)) {
            if let Some((registration, connection)) = committed_catalog {
                registration.remove();
                {
                    let _catalog = write_std_lock(&connection.catalog_guard);
                    write_std_lock(&connection.tool_catalog).clear();
                }
                update_health(
                    &connection.health,
                    ExtensionHealthState::Crashed,
                    Some("dynamic tool catalog acknowledgement was not delivered".to_owned()),
                );
                connection.terminate().await;
            }
        }
        if let Err(message) = delivery {
            let _ = inner.events.send(ExtensionEvent::Diagnostic { message });
            settle_child_request(&update.child_requests, &update.request_id);
        }
    }
}

pub(super) fn new_extension_instance_id() -> String {
    let mut random = [0_u8; 16];
    if getrandom::fill(&mut random).is_ok() {
        return random.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    let sequence = NEXT_EXTENSION_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    format!("local-{}-{sequence}", std::process::id())
}

pub(super) fn supervisor_backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(20);
    let base_ms = u64::try_from(SUPERVISOR_BASE_BACKOFF.as_millis()).unwrap_or(u64::MAX);
    let cap_ms = u64::try_from(SUPERVISOR_MAX_BACKOFF.as_millis()).unwrap_or(u64::MAX);
    let exponential = base_ms.saturating_mul(1_u64 << shift).min(cap_ms);
    let mut random = [0_u8; 8];
    let jitter = if getrandom::fill(&mut random).is_ok() {
        u64::from_le_bytes(random) % exponential.saturating_add(1)
    } else {
        exponential
    };
    Duration::from_millis(jitter)
}

pub(super) fn permanent_supervisor_error(error: &ExtensionRuntimeError) -> bool {
    matches!(
        error,
        ExtensionRuntimeError::InvalidManifest(_)
            | ExtensionRuntimeError::UnsupportedApiVersion { .. }
            | ExtensionRuntimeError::ReloadRequiresReregistration { .. }
    )
}

pub(super) fn supervisor_is_stopping(inner: &ExtensionProcessInner) -> bool {
    HOST_SHUTDOWN_REQUESTED.load(Ordering::Acquire)
        || inner.supervisor_cancelled.load(Ordering::Acquire)
}

pub(super) async fn wait_for_supervisor_revival(
    inner: &Weak<ExtensionProcessInner>,
    parked_generation: u64,
) -> bool {
    loop {
        let Some(current_inner) = inner.upgrade() else {
            return false;
        };
        if supervisor_is_stopping(&current_inner) {
            return false;
        }
        let active_generation = read_std_lock(&current_inner.connection).generation;
        drop(current_inner);
        if active_generation != parked_generation {
            return true;
        }
        tokio::time::sleep(SUPERVISOR_POLL).await;
    }
}

pub(super) async fn supervise_extension(inner: Weak<ExtensionProcessInner>) {
    let mut restart_attempts = 0_u32;
    loop {
        let Some(current_inner) = inner.upgrade() else {
            return;
        };
        if supervisor_is_stopping(&current_inner) {
            return;
        }
        let connection = read_std_lock(&current_inner.connection).clone();
        let generation = connection.generation;
        drop(current_inner);
        let ready_since = Instant::now();
        let mut reset_attempts = false;
        loop {
            let Some(current_inner) = inner.upgrade() else {
                return;
            };
            if supervisor_is_stopping(&current_inner) {
                return;
            }
            let active = read_std_lock(&current_inner.connection).clone();
            drop(current_inner);
            if active.generation != generation {
                break;
            }
            if ready_since.elapsed() >= SUPERVISOR_STABLE_READY {
                reset_attempts = true;
            }
            if connection.closed.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(SUPERVISOR_POLL).await;
        }
        let Some(current_inner) = inner.upgrade() else {
            return;
        };
        if supervisor_is_stopping(&current_inner) {
            return;
        }
        if read_std_lock(&current_inner.connection).generation != generation {
            continue;
        }
        if let Some(registration) =
            lock_std_mutex(&current_inner.dynamic_tool_registration).as_ref()
        {
            // A frozen provider turn retains its pinned failing endpoint, but
            // subsequent turns must not keep advertising a dead process.
            registration.remove();
        }
        if reset_attempts {
            restart_attempts = 0;
        }
        restart_attempts = restart_attempts.saturating_add(1);
        if restart_attempts > SUPERVISOR_MAX_RESTARTS {
            update_health(
                &connection.health,
                ExtensionHealthState::Parked,
                Some(format!(
                    "extension restart budget exhausted after {SUPERVISOR_MAX_RESTARTS} attempts"
                )),
            );
            if let Some(registration) =
                lock_std_mutex(&current_inner.dynamic_tool_registration).as_ref()
            {
                registration.remove();
            }
            let _ = current_inner.events.send(ExtensionEvent::Diagnostic {
                message: format!(
                    "extension `{}` parked after repeated crashes",
                    current_inner.descriptor.manifest.name
                ),
            });
            drop(current_inner);
            if wait_for_supervisor_revival(&inner, generation).await {
                restart_attempts = 0;
                continue;
            }
            return;
        }
        update_health(
            &connection.health,
            ExtensionHealthState::Backoff,
            Some(format!(
                "unexpected exit; restart attempt {restart_attempts}/{SUPERVISOR_MAX_RESTARTS}"
            )),
        );
        let delay = supervisor_backoff(restart_attempts);
        drop(current_inner);
        tokio::time::sleep(delay).await;

        let Some(current_inner) = inner.upgrade() else {
            return;
        };
        if supervisor_is_stopping(&current_inner) {
            return;
        }
        let reload_guard = current_inner.reload_guard.lock().await;
        if supervisor_is_stopping(&current_inner) {
            return;
        }
        let active = read_std_lock(&current_inner.connection).clone();
        if active.generation != generation || !active.closed.load(Ordering::Acquire) {
            continue;
        }
        let process = ExtensionProcess {
            inner: Arc::clone(&current_inner),
        };
        let result = process.reload_locked(Some(generation)).await;
        drop(reload_guard);
        drop(current_inner);
        if let Err(error) = result {
            let Some(current_inner) = inner.upgrade() else {
                return;
            };
            let active = read_std_lock(&current_inner.connection).clone();
            update_health(
                &active.health,
                if permanent_supervisor_error(&error) {
                    ExtensionHealthState::Parked
                } else {
                    ExtensionHealthState::Crashed
                },
                Some(error.to_string()),
            );
            if permanent_supervisor_error(&error) {
                if let Some(registration) =
                    lock_std_mutex(&current_inner.dynamic_tool_registration).as_ref()
                {
                    registration.remove();
                }
                let _ = current_inner.events.send(ExtensionEvent::Diagnostic {
                    message: format!(
                        "extension `{}` parked after a permanent restart error: {error}",
                        current_inner.descriptor.manifest.name
                    ),
                });
                drop(current_inner);
                if wait_for_supervisor_revival(&inner, generation).await {
                    restart_attempts = 0;
                    continue;
                }
                return;
            }
        }
    }
}

pub(super) struct ProcessToolSet {
    pub(super) tools: Vec<Arc<dyn Tool>>,
    pub(super) revision: Arc<AtomicU64>,
}

pub(super) struct ProcessTool {
    pub(super) process: ExtensionProcess,
    pub(super) connection: Arc<ProcessConnection>,
    pub(super) definition: ToolDefinition,
    pub(super) catalog_revision: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl Tool for ProcessTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: self.definition.name.clone(),
            description: self.definition.description.clone(),
            parameters: self.definition.parameters.clone(),
        }
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Unsafe
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Extension)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let context = self
            .process
            .tool_execution_context(ctx, self.connection.generation);
        let mut events = self.process.subscribe();
        let mut events_open = true;
        let legacy_uncorrelated = self.process.api_version() == EXTENSION_API_VERSION_0_1;
        let (request_started, started) = oneshot::channel();
        let call = self.process.call_tool_controlled(
            Arc::clone(&self.connection),
            self.definition.clone(),
            self.catalog_revision.load(Ordering::Acquire),
            args,
            context,
            ctx.cancellation.clone(),
            ctx.progress.clone(),
            request_started,
        );
        tokio::pin!(call);
        let operation = tokio::select! {
            output = &mut call => return lower_process_tool_output(output, legacy_uncorrelated),
            started = started => match started {
                Ok(operation) => operation,
                Err(_) => return lower_process_tool_output(call.await, legacy_uncorrelated),
            },
        };
        let output = loop {
            tokio::select! {
                output = &mut call => break output,
                event = events.recv(), if events_open => match event {
                    Ok(ExtensionEvent::ConfirmationRequested {
                        request_id,
                        generation,
                        parent_request_id: event_parent,
                        request,
                    }) if event_parent.is_some_and(|parent| operation.owns(generation, parent))
                        || (legacy_uncorrelated
                            && generation == operation.generation
                            && event_parent.is_none()) => {
                        let confirmation = ctx.progress.confirmation(
                                request.prompt,
                                request.detail,
                                request.destructive,
                                request.default,
                            );
                        tokio::pin!(confirmation);
                        let confirmed = tokio::select! {
                            confirmed = &mut confirmation => confirmed,
                            _ = ctx.cancellation.cancelled() => false,
                        };
                        self.process.respond_to_confirmation(
                            request_id,
                            generation,
                            ConfirmationResponse { confirmed },
                        ).await.map_err(|error| ToolError::new(error.to_string()))?;
                    }
                    Ok(ExtensionEvent::ConfirmationRequested { .. }) => {}
                    Ok(ExtensionEvent::Notification { notification }) => {
                        ctx.progress.status(format!(
                            "extension notification: {}",
                            notification.message
                        ));
                    }
                    Ok(ExtensionEvent::Diagnostic { message }) => {
                        ctx.progress.status(format!("extension diagnostic: {message}"));
                    }
                    Ok(ExtensionEvent::StatusContributed { contribution }) => {
                        ctx.progress.status(contribution.text);
                    }
                    Ok(ExtensionEvent::PresentationUpdated { .. }) => {}
                    Ok(ExtensionEvent::UiContributed { .. }) => {}
                    Ok(ExtensionEvent::EditorRequested { .. }) => {}
                    Ok(ExtensionEvent::AutocompleteRegistered { .. }) => {}
                    Ok(ExtensionEvent::ContextContributed { .. }) => {}
                    Ok(ExtensionEvent::PolicyEvaluationRequested {
                        ..
                    }) => {}
                    Ok(ExtensionEvent::InputRequested {
                        request_id,
                        generation,
                        parent_request_id: event_parent,
                        request,
                    }) if operation.owns(generation, event_parent) => {
                        let input = ctx.progress.input(request.prompt, request.secret);
                        tokio::pin!(input);
                        let value = tokio::select! {
                            answer = &mut input => answer.and_then(|answer| {
                                let bytes = answer.as_bytes();
                                (bytes.len() <= MAX_EXTENSION_INPUT_VALUE_BYTES)
                                    .then(|| std::str::from_utf8(bytes).ok().map(str::to_owned))
                                    .flatten()
                            }),
                            _ = ctx.cancellation.cancelled() => None,
                        };
                        self.process.respond_to_input(
                            request_id,
                            generation,
                            ExtensionInputResponse { value },
                        ).await.map_err(|error| ToolError::new(error.to_string()))?;
                    }
                    Ok(ExtensionEvent::InputRequested { .. }) => {}
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        ctx.progress.status(format!(
                            "extension event stream dropped {count} event(s)"
                        ));
                    }
                    Ok(ExtensionEvent::ComposerRequested { .. })
                    | Ok(ExtensionEvent::SessionEntryRequested { .. })
                    | Ok(ExtensionEvent::MessageInjectionRequested { .. })
                    | Ok(ExtensionEvent::ShortcutRequested { .. })
                    | Ok(ExtensionEvent::ActiveToolsRequested { .. })
                    | Ok(ExtensionEvent::TerminalRequested { .. })
                    | Ok(ExtensionEvent::ContextSnapshotRequested { .. })
                    | Ok(ExtensionEvent::ModelViewRequested { .. }) => {}
                    Err(broadcast::error::RecvError::Closed) => events_open = false,
                }
            }
        };
        lower_process_tool_output(output, legacy_uncorrelated)
    }
}

pub(super) fn lower_process_tool_output(
    output: Result<ToolCallOutput, ExtensionRuntimeError>,
    api_0_1: bool,
) -> Result<ToolOutput, ToolError> {
    match output {
        Ok(output) if api_0_1 && output.is_error => Err(ToolError::new(output.content)),
        Ok(output) => {
            let is_error = output.is_error;
            output
                .into_native()
                .map(|output| output.with_is_error(is_error))
                .map_err(|error| ToolError::new(error.to_string()))
        }
        Err(error) => Err(ToolError::new(error.to_string())),
    }
}

pub(super) fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

pub(super) const FRAME_QUEUED: u8 = 0;

pub(super) const FRAME_WRITING: u8 = 1;

pub(super) const FRAME_WRITTEN: u8 = 2;

pub(super) const FRAME_SKIPPED: u8 = 3;

pub(super) const REQUEST_ACTIVE: u8 = 0;

pub(super) const REQUEST_COMPLETED: u8 = 1;

pub(super) const REQUEST_CANCELLED: u8 = 2;

pub(super) const JSON_RPC_REQUEST_CANCELLED: i64 = -32800;
