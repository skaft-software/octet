//! The `ExtensionProcess` handle: requests, hooks, tools, commands and lifecycle calls.

use super::*;

impl ExtensionProcess {
    /// Launches, initializes, and validates an explicitly enabled and trusted
    /// executable extension.
    pub async fn start(
        descriptor: DiscoveredExtension,
        mut config: ExtensionRuntimeConfig,
    ) -> Result<Self, ExtensionRuntimeError> {
        descriptor.ensure_startable()?;
        config.flag_values =
            resolve_extension_flag_values(&descriptor.manifest, &config.flag_values)?;
        if config.max_message_bytes == 0
            || config.max_pending_requests == 0
            || config.writer_queue_capacity == 0
            || config.provider_stream_buffer == 0
            || config.cancellation_grace.is_zero()
            || config.tombstone_ttl.is_zero()
            || config.provider_stream_idle_timeout.is_zero()
            || config.provider_stream_deadline.is_zero()
        {
            return Err(ExtensionRuntimeError::Protocol(
                "message, request, writer, provider stream, cancellation, and tombstone limits must be greater than zero"
                    .into(),
            ));
        }
        if descriptor.manifest.api_version == EXTENSION_API_VERSION_0_3
            && config.max_message_bytes <= 1
        {
            return Err(ExtensionRuntimeError::Protocol(
                "API 0.3 message limit must leave room for a non-empty frame and newline".into(),
            ));
        }
        if !config.workspace.is_dir() {
            return Err(ExtensionRuntimeError::Spawn {
                extension: descriptor.manifest.name.clone(),
                message: format!(
                    "workspace {} is not a directory",
                    config.workspace.display()
                ),
            });
        }

        // Retain the first receiver across initialization so an extension
        // cannot race a startup notification or confirmation ahead of the
        // product's first `subscribe` call.
        let (events, initial_events) = broadcast::channel(EXTENSION_EVENT_CAPACITY);
        let artifact_store =
            ArtifactStore::new().map_err(|error| ExtensionRuntimeError::Spawn {
                extension: descriptor.manifest.name.clone(),
                message: format!("cannot create artifact store: {error}"),
            })?;
        let approval_store = Arc::new(ExtensionApprovalStore::new());
        let (catalog_updates, catalog_update_rx) = mpsc::channel(DYNAMIC_CATALOG_QUEUE_CAPACITY);
        let delegation_service = Arc::new(StdRwLock::new(None));
        let generation = 1;
        let instance_id = new_extension_instance_id();
        let (connection, contributions) = spawn_connection(
            &descriptor,
            &config,
            config.host_state.clone(),
            generation,
            &instance_id,
            events.clone(),
            artifact_store.clone(),
            catalog_updates.clone(),
            Arc::clone(&delegation_service),
            Arc::clone(&approval_store),
            false,
        )
        .await?;
        let process = Self {
            inner: Arc::new(ExtensionProcessInner {
                host_state: StdRwLock::new(config.host_state.clone()),
                descriptor,
                config,
                contributions,
                connection: StdRwLock::new(connection),
                events,
                initial_events: StdMutex::new(Some(initial_events)),
                answered_confirmations: StdMutex::new(AnsweredConfirmations::default()),
                answered_inputs: StdMutex::new(AnsweredConfirmations::default()),
                generation: AtomicU64::new(generation),
                next_generation: AtomicU64::new(generation.saturating_add(1)),
                instance_id,
                generation_changed: Arc::new(Notify::new()),
                reload_guard: Mutex::new(()),
                supervisor_cancelled: AtomicBool::new(false),
                artifact_store,
                approval_store,
                lifecycle: StdMutex::new(ActiveLifecycleState::default()),
                session_hooks: StdMutex::new(BTreeMap::new()),
                dynamic_tool_registration: StdMutex::new(None),
                dynamic_tool_registration_ready: Notify::new(),
                delegation_service,
                catalog_updates,
            }),
        };
        tokio::spawn(run_catalog_updates(
            Arc::downgrade(&process.inner),
            catalog_update_rx,
        ));
        if process.inner.config.supervise {
            tokio::spawn(supervise_extension(Arc::downgrade(&process.inner)));
        }
        Ok(process)
    }

    /// Returns the discovered manifest and activation metadata.
    pub fn descriptor(&self) -> &DiscoveredExtension {
        &self.inner.descriptor
    }

    /// Returns the host-created process-instance fence. Unlike process
    /// generation, this identity does not repeat across complete host rebuilds.
    pub fn extension_instance_id(&self) -> &str {
        &self.inner.instance_id
    }

    /// Returns the contributions negotiated during initialization.
    pub fn contributions(&self) -> &ExtensionContributions {
        &self.inner.contributions
    }

    /// Returns the active generation's complete live tool catalog.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let connection = read_std_lock(&self.inner.connection);
        let _catalog = read_std_lock(&connection.catalog_guard);
        let tools = read_std_lock(&connection.tool_catalog).clone();
        tools
    }

    /// Returns the exact manifest-selected protocol version.
    pub fn api_version(&self) -> &str {
        &self.inner.descriptor.manifest.api_version
    }

    /// Returns immutable feature, limit, and lifecycle negotiation for the
    /// active process generation.
    pub fn negotiated_protocol(&self) -> ExtensionNegotiatedProtocol {
        let connection = read_std_lock(&self.inner.connection);
        let protocol = read_std_lock(&connection.protocol).clone();
        protocol
    }

    /// Returns the active generation's negotiated additive feature set.
    pub fn negotiated_features(&self) -> BTreeSet<String> {
        self.negotiated_protocol().features
    }

    /// Tests one feature against the current process generation without cloning
    /// the negotiated catalog. The connection fence is held through the lookup.
    pub fn supports_feature(&self, feature: &str) -> bool {
        let connection = read_std_lock(&self.inner.connection);
        let supported = read_std_lock(&connection.protocol).supports(feature);
        supported
    }

    pub(crate) fn bind_agent_session_service(
        &self,
        service: ExtensionDelegationService,
    ) -> Result<(), ExtensionRuntimeError> {
        if !self.supports_feature(EXTENSION_FEATURE_AGENT_SESSIONS) {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "extension `{}` did not negotiate `{EXTENSION_FEATURE_AGENT_SESSIONS}`",
                self.inner.descriptor.manifest.name
            )));
        }
        *write_std_lock(&self.inner.delegation_service) = Some(service);
        Ok(())
    }

    /// Returns the stable path-free principal used to isolate host-owned child
    /// sessions across supervised process restarts.
    pub fn agent_session_principal(&self) -> String {
        let manifest_identity = self
            .inner
            .descriptor
            .manifest_path
            .canonicalize()
            .unwrap_or_else(|_| self.inner.descriptor.manifest_path.clone());
        let digest = Sha256::digest(manifest_identity.to_string_lossy().as_bytes());
        format!("{}@sha256:{digest:x}", self.inner.descriptor.manifest.name)
    }

    /// Returns an inspectable bounded health snapshot for the active process.
    pub fn health_snapshot(&self) -> ExtensionHealthSnapshot {
        let connection = read_std_lock(&self.inner.connection);
        let health = read_std_lock(&connection.health);
        let pending_requests = lock_std_mutex(&connection.pending).len();
        ExtensionHealthSnapshot {
            state: health.state,
            generation: connection.generation,
            pending_requests,
            last_error: health.last_error.clone(),
        }
    }

    /// Subscribes to notifications, confirmations, contributions, and bounded
    /// stderr/protocol diagnostics. The first subscriber also receives events
    /// buffered during initialization. Slow receivers may observe a lag error.
    pub fn subscribe(&self) -> broadcast::Receiver<ExtensionEvent> {
        lock_std_mutex(&self.inner.initial_events)
            .take()
            .unwrap_or_else(|| self.inner.events.subscribe())
    }

    /// Updates the session/model/skill snapshot attached to future calls and
    /// future reload initialization. Negotiated API 0.4 remote UI owners also
    /// receive a bounded `context/updated {resource_owner, host}` replacement.
    pub fn set_host_state(&self, state: ExtensionHostState) {
        let connection = read_std_lock(&self.inner.connection).clone();
        self.set_host_state_on_connection(state, connection, false, false, None);
    }

    pub(super) fn set_host_state_on_connection(
        &self,
        state: ExtensionHostState,
        connection: Arc<ProcessConnection>,
        mirror_changed: bool,
        mirror_refreshed: bool,
        mut retired_owner: Option<String>,
    ) {
        let replaced_session;
        {
            let mut current = write_std_lock(&self.inner.host_state);
            if *current == state && !mirror_changed {
                return;
            }
            replaced_session = current.session_id != state.session_id || retired_owner.is_some();
            if replaced_session && !mirror_refreshed {
                if let Some(previous) = lock_std_mutex(&connection.session_leaf.mirror).take() {
                    retired_owner = Some(previous.owner.session_id);
                }
            }
            if replaced_session && retired_owner.is_none() {
                retired_owner = current.session_id.clone();
            }
            *current = state.clone();
        }
        if let Some(owner) = retired_owner {
            // Retire on this pinned connection, without reacquiring the active
            // connection lock held by native snapshot publication. Authority is
            // the opaque resource owner, not the display session filename.
            lock_std_mutex(&connection.resources).retire_owner(&owner);
            lock_std_mutex(&connection.issued_resource_owners)
                .retain(|issued| issued.session_id != owner);
            connection.pending_changed.notify_waiters();
            if let Some(service) = read_std_lock(&self.inner.delegation_service).clone() {
                service.shutdown_owner(&owner);
            }
            connection.resource_cleanup_changed.notify_one();
        }
        let protocol = read_std_lock(&connection.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_4
            || !protocol.supports(EXTENSION_FEATURE_REMOTE_UI)
            || !connection_is_usable(&connection)
            || connection.draining.load(Ordering::Acquire)
        {
            return;
        }
        drop(protocol);
        if replaced_session {
            for surface_id in connection.remote_ui.surface_ids() {
                let _ = connection.queue_notification(methods::UI_CLOSED,
                    serde_json::json!({"surface_id":surface_id,"reason":"foreground owner replaced"}));
            }
            connection.remote_ui.clear();
            lock_std_mutex(&connection.issued_resource_owners).clear();
            return;
        }
        // Only already-admitted UI owners receive retained context updates.
        // The bounded surface map supplies at most sixteen distinct owners.
        for owner in connection.remote_ui.owners() {
            if lock_std_mutex(&connection.issued_resource_owners).contains(&owner) {
                let mut host = serde_json::to_value(&state).expect("host state serializes");
                if let Err(error) =
                    session_leaf::attach_session_mirror(&connection, &owner, &mut host, true)
                {
                    let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                        message: format!("context update refused: {error}"),
                    });
                    // Even a mismatched owner must not keep a previously
                    // published mirror alive after replacement was refused.
                    connection.begin_drain();
                    connection.kill_process_group();
                    break;
                }
                if !connection.queue_notification(
                    methods::CONTEXT_UPDATED,
                    serde_json::json!({"resource_owner": owner, "host": host}),
                ) {
                    // A retained mirror must not remain usable after losing a
                    // replacement. Retire the generation rather than present
                    // an old complete snapshot as the current session.
                    if lock_std_mutex(&connection.session_leaf.mirror).is_some() {
                        connection.begin_drain();
                        connection.kill_process_group();
                    }
                    break;
                }
            }
        }
    }

    /// Returns whether the current process transport is open.
    pub fn is_running(&self) -> bool {
        !read_std_lock(&self.inner.connection)
            .closed
            .load(Ordering::Acquire)
    }

    /// Builds a host-owned transport for one registered extension provider
    /// model. The product chooses where to install this transport in its
    /// canonical model catalog; the extension never receives endpoint URLs,
    /// headers, or credentials through this handle.
    pub fn provider_stream_transport(
        &self,
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Arc<dyn HostStreamTransport> {
        Arc::new(ExtensionProviderStreamTransport {
            process: Arc::downgrade(&self.inner),
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        })
    }

    /// Builds the current ambient execution context for command, hook,
    /// context, status, and renderer calls.
    pub fn current_context(&self) -> ExtensionExecutionContext {
        self.execution_context()
    }

    /// Builds a session-owned API `0.2`/`0.3` context for product boundaries such as
    /// commands, prompt hooks, and context collection. The host supplies only
    /// the durable owner key; this method attaches the unforgeable instance and
    /// active process-generation fences. Frozen API `0.1` remains ownerless.
    pub fn current_context_for_resource_owner(
        &self,
        session_id: impl Into<String>,
    ) -> ExtensionExecutionContext {
        let mut context = self.execution_context();
        if is_stateful_api(self.api_version()) {
            let generation = read_std_lock(&self.inner.connection).generation;
            context.resource_owner = Some(ExtensionResourceOwner {
                session_id: session_id.into(),
                extension_instance_id: self.inner.instance_id.clone(),
                process_generation: generation,
            });
        }
        context
    }

    /// Returns whether this process declared host-owned session hooks. Canonical
    /// API 0.3 requires the pair; API 0.4 may declare either hook independently.
    pub fn declares_session_hooks(&self) -> bool {
        let hooks = &self.inner.contributions.hooks;
        match self.api_version() {
            EXTENSION_API_VERSION_0_3 => {
                hooks.contains(&ExtensionHook::SessionStart)
                    && hooks.contains(&ExtensionHook::SessionEnd)
            }
            EXTENSION_API_VERSION_0_4 => hooks.iter().any(|hook| hook.is_session_hook()),
            _ => false,
        }
    }

    /// Starts one declared, owner-scoped session-hook binding exactly once.
    ///
    /// The supplied ID must be the host's opaque session owner key. The typed
    /// wire payload never carries a session path, mutable `Session`, prompt,
    /// or host-state snapshot.
    pub async fn start_session_hook_binding(
        &self,
        session_id: impl Into<String>,
    ) -> Result<(), ExtensionRuntimeError> {
        if !self.declares_session_hooks() {
            return Err(self.undeclared("session hook", "session_start/session_end".to_owned()));
        }
        let session_id = session_id.into();
        validate_session_hook_id(&session_id)?;
        let _guard = self.inner.reload_guard.lock().await;
        let connection = read_std_lock(&self.inner.connection).clone();
        if connection.draining.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeError::Closed(
                "extension generation is draining".into(),
            ));
        }
        connection.require_api_v03_host_method(methods::HOOK_RUN)?;
        let binding = {
            let mut bindings = lock_std_mutex(&self.inner.session_hooks);
            if bindings.contains_key(&session_id) {
                return Ok(());
            }
            let binding = ActiveSessionHookBinding {
                start_outcome: Arc::new(SessionHookStartOutcome::default()),
                session_id: session_id.clone(),
                started_at: Instant::now(),
                endpoint: LifecycleEndpoint {
                    generation: connection.generation,
                    connection,
                },
            };
            // Retain ownership before awaiting the remote result. A timed-out
            // start may have reached the extension, so the finalizer must
            // still issue exactly one terminal attempt rather than retrying.
            bindings.insert(session_id, binding.clone());
            binding
        };
        self.dispatch_session_hook_start(&binding).await
    }

    /// Settles one declared session-hook binding exactly once.
    ///
    /// Repeated calls are idempotent. A valid non-continue disposition is
    /// diagnostic-only and cannot veto this host-owned finalizer.
    pub async fn settle_session_hook_binding(
        &self,
        session_id: &str,
        outcome: ExtensionLifecycleOutcome,
    ) -> Result<(), ExtensionRuntimeError> {
        if !self.declares_session_hooks() {
            return Err(self.undeclared("session hook", "session_start/session_end".to_owned()));
        }
        let _guard = self.inner.reload_guard.lock().await;
        let binding = lock_std_mutex(&self.inner.session_hooks).remove(session_id);
        let Some(binding) = binding else {
            return Ok(());
        };
        self.dispatch_session_hook_end(
            &binding,
            &binding.endpoint,
            outcome,
            session_hook_shutdown_reason(outcome),
        )
        .await
    }

    pub(super) async fn settle_all_session_hook_bindings_locked(
        &self,
        outcome: ExtensionLifecycleOutcome,
        reason: &'static str,
    ) {
        let bindings = std::mem::take(&mut *lock_std_mutex(&self.inner.session_hooks))
            .into_values()
            .collect::<Vec<_>>();
        let results = futures_util::future::join_all(bindings.iter().map(|binding| {
            self.dispatch_session_hook_end(binding, &binding.endpoint, outcome, reason)
        }))
        .await;
        for result in results {
            if let Err(error) = result {
                self.emit_session_hook_failure("session_end", &error);
            }
        }
    }

    pub(super) fn take_session_hook_bindings_for_generation(
        &self,
        generation: u64,
    ) -> Vec<ActiveSessionHookBinding> {
        let mut bindings = lock_std_mutex(&self.inner.session_hooks);
        let session_ids = bindings
            .iter()
            .filter_map(|(session_id, binding)| {
                (binding.endpoint.generation == generation).then_some(session_id.clone())
            })
            .collect::<Vec<_>>();
        session_ids
            .into_iter()
            .filter_map(|session_id| bindings.remove(&session_id))
            .collect()
    }

    pub(super) fn install_replacement_session_hook_bindings(
        &self,
        settled_bindings: Vec<ActiveSessionHookBinding>,
        endpoint: LifecycleEndpoint,
    ) -> Vec<ActiveSessionHookBinding> {
        let mut bindings = lock_std_mutex(&self.inner.session_hooks);
        settled_bindings
            .into_iter()
            .map(|binding| {
                let replacement = ActiveSessionHookBinding {
                    start_outcome: Arc::new(SessionHookStartOutcome::default()),
                    session_id: binding.session_id,
                    started_at: Instant::now(),
                    endpoint: endpoint.clone(),
                };
                bindings.insert(replacement.session_id.clone(), replacement.clone());
                replacement
            })
            .collect()
    }

    pub(super) async fn dispatch_session_hook_start(
        &self,
        binding: &ActiveSessionHookBinding,
    ) -> Result<(), ExtensionRuntimeError> {
        let attempt = SessionHookStartAttempt(binding.start_outcome.clone());
        let result = async {
            if !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::SessionStart)
            {
                return Ok(());
            }
            let params = api_v03::SessionHookParams::SessionStart {
                payload: api_v03::SessionStart {
                    binding: session_hook_wire_binding(binding, &self.inner.instance_id)?,
                },
            };
            self.dispatch_session_hook(&binding.endpoint, &binding.session_id, params)
                .await
        }
        .await;
        attempt.finish(result.is_ok());
        result
    }

    pub(super) async fn dispatch_session_hook_end(
        &self,
        binding: &ActiveSessionHookBinding,
        endpoint: &LifecycleEndpoint,
        outcome: ExtensionLifecycleOutcome,
        reason: &'static str,
    ) -> Result<(), ExtensionRuntimeError> {
        let params = api_v03::SessionHookParams::SessionEnd {
            payload: api_v03::SessionEnd {
                binding: session_hook_wire_binding(binding, &self.inner.instance_id)?,
                outcome: session_hook_outcome(outcome).to_owned(),
                reason: reason.to_owned(),
                duration_ms: session_hook_duration_millis(binding.started_at.elapsed()),
            },
        };
        let result = if self
            .inner
            .contributions
            .hooks
            .contains(&ExtensionHook::SessionEnd)
        {
            self.dispatch_session_hook(endpoint, &binding.session_id, params)
                .await
        } else {
            Ok(())
        };
        let owner = ExtensionResourceOwner {
            session_id: binding.session_id.clone(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: endpoint.generation,
        };
        endpoint.connection.remote_ui.discard_owner(&owner);
        lock_std_mutex(&endpoint.connection.issued_resource_owners).remove(&owner);
        result
    }

    pub(super) async fn dispatch_session_hook(
        &self,
        endpoint: &LifecycleEndpoint,
        session_id: &str,
        params: api_v03::SessionHookParams,
    ) -> Result<(), ExtensionRuntimeError> {
        endpoint
            .connection
            .require_api_v03_host_method(methods::HOOK_RUN)?;
        let params = serde_json::to_value(params)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let params = api_v03::parse_session_hook_params(params).map_err(api_v03_protocol_error)?;
        let mut params = serde_json::to_value(params)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let resource_owner = ExtensionResourceOwner {
            session_id: session_id.to_owned(),
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: endpoint.generation,
        };
        let canonical =
            read_std_lock(&endpoint.connection.protocol).version == EXTENSION_API_VERSION_0_3;
        if !canonical {
            let mut context = self.execution_context();
            context.resource_owner = Some(resource_owner.clone());
            params["context"] = serde_json::to_value(context)
                .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
            session_leaf::attach_request_session_mirror(
                &endpoint.connection,
                Some(&resource_owner),
                &mut params,
            )?;
        }
        let result = endpoint
            .connection
            .request_lifecycle(
                methods::HOOK_RUN,
                params,
                self.inner.config.request_timeout.min(SESSION_HOOK_DEADLINE),
                resource_owner,
            )
            .await?;
        let continued = if canonical {
            let result =
                api_v03::parse_session_hook_result(result).map_err(api_v03_protocol_error)?;
            api_v03::validate_disposition(&result.disposition).map_err(api_v03_protocol_error)?;
            result.disposition.kind == "continue"
        } else {
            let result: ExtensionHookOutput = serde_json::from_value(result).map_err(|error| {
                ExtensionRuntimeError::Protocol(format!("invalid session hook response: {error}"))
            })?;
            result.disposition == ExtensionHookDisposition::Continue
        };
        if !continued {
            let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                message: "session hook returned a non-veto disposition; ignored".into(),
            });
        }
        Ok(())
    }

    pub(super) fn emit_session_hook_failure(&self, phase: &str, error: &ExtensionRuntimeError) {
        let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
            message: format!("{phase} hook failed ({})", session_hook_error_kind(error)),
        });
    }

    /// Invokes a manifest-declared model tool.
    pub async fn call_tool(
        &self,
        name: impl Into<String>,
        arguments: serde_json::Value,
        mut context: ExtensionExecutionContext,
    ) -> Result<ToolCallOutput, ExtensionRuntimeError> {
        let name = name.into();
        let connection = read_std_lock(&self.inner.connection).clone();
        connection.require_api_v03_host_method(methods::TOOL_CALL)?;
        let _catalog = read_std_lock(&connection.catalog_guard);
        let definition = self.require_tool(&connection, &name)?;
        let catalog_revision = read_std_lock(&connection.protocol)
            .supports(EXTENSION_FEATURE_DYNAMIC_TOOLS)
            .then(|| connection.catalog_revision.load(Ordering::Acquire));
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        let params = if read_std_lock(&connection.protocol).version == EXTENSION_API_VERSION_0_3 {
            let params = api_v03::ToolCallParams {
                name,
                arguments,
                context: serde_json::to_value(context)
                    .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            };
            api_v03::validate_tool_call_params(&params).map_err(api_v03_protocol_error)?;
            serde_json::to_value(params)
        } else {
            serde_json::to_value(ToolCallRequest {
                name,
                arguments,
                catalog_revision,
                context,
            })
        }
        .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        drop(_catalog);
        let _artifact_lease = connection.acquire_artifact_lease();
        let policy = lock_std_mutex(&self.inner.dynamic_tool_registration).clone();
        connection
            .request_tool(
                definition,
                params,
                self.inner.config.request_timeout,
                resource_owner,
                None,
                None,
                None,
                policy,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn call_tool_controlled(
        &self,
        connection: Arc<ProcessConnection>,
        definition: ToolDefinition,
        catalog_revision: u64,
        arguments: serde_json::Value,
        mut context: ExtensionExecutionContext,
        cancellation: CancellationToken,
        progress: ToolProgressSink,
        request_started: oneshot::Sender<ExtensionOperationToken>,
    ) -> Result<ToolCallOutput, ExtensionRuntimeError> {
        connection.require_api_v03_host_method(methods::TOOL_CALL)?;
        let catalog_revision = read_std_lock(&connection.protocol)
            .supports(EXTENSION_FEATURE_DYNAMIC_TOOLS)
            .then_some(catalog_revision);
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        let params = if read_std_lock(&connection.protocol).version == EXTENSION_API_VERSION_0_3 {
            let params = api_v03::ToolCallParams {
                name: definition.name.clone(),
                arguments,
                context: serde_json::to_value(context)
                    .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?,
            };
            api_v03::validate_tool_call_params(&params).map_err(api_v03_protocol_error)?;
            serde_json::to_value(params)
        } else {
            serde_json::to_value(ToolCallRequest {
                name: definition.name.clone(),
                arguments,
                catalog_revision,
                context,
            })
        }
        .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let _artifact_lease = connection.acquire_artifact_lease();
        let policy = lock_std_mutex(&self.inner.dynamic_tool_registration).clone();
        connection
            .request_tool(
                definition,
                params,
                self.inner.config.request_timeout,
                resource_owner,
                Some(cancellation),
                Some(progress),
                Some(request_started),
                policy,
            )
            .await
    }

    /// Invokes a manifest-declared slash command.
    pub async fn execute_command(
        &self,
        name: impl Into<String>,
        arguments: Vec<String>,
        context: ExtensionExecutionContext,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        self.execute_command_inner(
            name.into(),
            arguments,
            context,
            None,
            None,
            None,
            self.inner.config.request_timeout,
        )
        .await
    }

    /// Invokes a manifest-declared slash command and reports the exact
    /// generation-scoped operation identity once the request is admitted.
    pub async fn execute_command_controlled(
        &self,
        name: impl Into<String>,
        arguments: Vec<String>,
        context: ExtensionExecutionContext,
        request_started: oneshot::Sender<ExtensionOperationToken>,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        self.execute_command_inner(
            name.into(),
            arguments,
            context,
            Some(request_started),
            None,
            None,
            self.inner.config.request_timeout,
        )
        .await
    }

    /// Invokes a manifest-declared slash command through the bounded,
    /// request-scoped progress channel used by model tools.
    ///
    /// The operation token, cancellation token, and progress sink all belong
    /// to the same active request. Late progress is discarded when that
    /// request settles or its generation is replaced.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_command_controlled_with_progress(
        &self,
        name: impl Into<String>,
        arguments: Vec<String>,
        context: ExtensionExecutionContext,
        cancellation: CancellationToken,
        progress: ToolProgressSink,
        request_started: oneshot::Sender<ExtensionOperationToken>,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        self.execute_command_inner(
            name.into(),
            arguments,
            context,
            Some(request_started),
            Some(cancellation),
            Some(progress),
            self.inner.config.request_timeout,
        )
        .await
    }

    /// Like [`Self::execute_command_controlled_with_progress`], for a command
    /// a person started and is watching. It may run until `deadline` instead
    /// of the ordinary request timeout, because its progress is on screen and
    /// the person can cancel it at any time.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_attended_command_with_progress(
        &self,
        name: impl Into<String>,
        arguments: Vec<String>,
        context: ExtensionExecutionContext,
        cancellation: CancellationToken,
        progress: ToolProgressSink,
        request_started: oneshot::Sender<ExtensionOperationToken>,
        deadline: Duration,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        self.execute_command_inner(
            name.into(),
            arguments,
            context,
            Some(request_started),
            Some(cancellation),
            Some(progress),
            deadline,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_command_inner(
        &self,
        name: String,
        arguments: Vec<String>,
        context: ExtensionExecutionContext,
        request_started: Option<oneshot::Sender<ExtensionOperationToken>>,
        cancellation: Option<CancellationToken>,
        progress: Option<ToolProgressSink>,
        timeout: Duration,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        if !self
            .inner
            .contributions
            .commands
            .iter()
            .any(|command| command.name == name)
        {
            return Err(self.undeclared("command", name));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        connection.require_api_v03_host_method(methods::COMMAND_EXECUTE)?;
        let mut context = context;
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        let params = serde_json::to_value(CommandRequest {
            name,
            arguments,
            context,
        })
        .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let result = match (request_started, cancellation, progress) {
            (Some(request_started), Some(cancellation), Some(progress)) => {
                connection
                    .request_with_command_progress(
                        methods::COMMAND_EXECUTE,
                        params,
                        timeout,
                        cancellation,
                        progress,
                        resource_owner,
                        request_started,
                    )
                    .await?
            }
            (Some(request_started), _, _) => {
                connection
                    .request_with_operation(
                        methods::COMMAND_EXECUTE,
                        params,
                        timeout,
                        resource_owner,
                        request_started,
                    )
                    .await?
            }
            (None, _, _) => {
                connection
                    .request_with_resource_owner(
                        methods::COMMAND_EXECUTE,
                        params,
                        timeout,
                        resource_owner,
                    )
                    .await?
            }
        };
        serde_json::from_value(result).map_err(|error| {
            ExtensionRuntimeError::Protocol(format!(
                "invalid `{}` response from `{}`: {error}",
                methods::COMMAND_EXECUTE,
                self.inner.descriptor.manifest.name
            ))
        })
    }

    /// Invokes a manifest-declared terminal shortcut action.
    pub async fn execute_shortcut(
        &self,
        name: impl Into<String>,
        context: ExtensionExecutionContext,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        self.execute_shortcut_inner(name.into(), context, None)
            .await
    }

    /// Invokes a manifest-declared terminal shortcut action and reports the
    /// exact generation-scoped operation identity once the request is admitted.
    pub async fn execute_shortcut_controlled(
        &self,
        name: impl Into<String>,
        context: ExtensionExecutionContext,
        request_started: oneshot::Sender<ExtensionOperationToken>,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        self.execute_shortcut_inner(name.into(), context, Some(request_started))
            .await
    }

    pub(super) async fn execute_shortcut_inner(
        &self,
        name: String,
        mut context: ExtensionExecutionContext,
        request_started: Option<oneshot::Sender<ExtensionOperationToken>>,
    ) -> Result<CommandOutput, ExtensionRuntimeError> {
        if !self
            .inner
            .contributions
            .shortcuts
            .iter()
            .any(|shortcut| shortcut.name == name)
        {
            return Err(self.undeclared("shortcut", name));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        connection.require_api_v03_host_method(methods::SHORTCUT_EXECUTE)?;
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        let params = serde_json::to_value(ShortcutRequest { name, context })
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let result = match request_started {
            Some(request_started) => {
                connection
                    .request_with_operation(
                        methods::SHORTCUT_EXECUTE,
                        params,
                        self.inner.config.request_timeout,
                        resource_owner,
                        request_started,
                    )
                    .await?
            }
            None => {
                connection
                    .request_with_resource_owner(
                        methods::SHORTCUT_EXECUTE,
                        params,
                        self.inner.config.request_timeout,
                        resource_owner,
                    )
                    .await?
            }
        };
        serde_json::from_value(result).map_err(|error| {
            ExtensionRuntimeError::Protocol(format!(
                "invalid `{}` response from `{}`: {error}",
                methods::SHORTCUT_EXECUTE,
                self.inner.descriptor.manifest.name
            ))
        })
    }

    /// Runs a manifest-declared lifecycle hook. Product code decides where an
    /// interceptable hook is applied; private agent state is never exposed.
    pub async fn run_hook(
        &self,
        hook: ExtensionHook,
        payload: serde_json::Value,
        context: ExtensionExecutionContext,
    ) -> Result<ExtensionHookOutput, ExtensionRuntimeError> {
        if hook.is_provider_pipeline()
            || hook.is_session_operation()
            || hook == ExtensionHook::ProviderContext
        {
            return Err(ExtensionRuntimeError::Protocol(
                "this hook requires its owning provider or session driver".into(),
            ));
        }
        if hook == ExtensionHook::ResourcesDiscover {
            return Err(ExtensionRuntimeError::Protocol(
                "resources_discover is host-owned; use discover_resource_paths".into(),
            ));
        }
        if hook.is_session_hook() {
            return Err(ExtensionRuntimeError::Protocol(
                "session_start and session_end are host-owned API 0.3/0.4 lifecycle hooks".into(),
            ));
        }
        if !self.inner.contributions.hooks.contains(&hook) {
            return Err(self.undeclared("hook", format!("{hook:?}").to_ascii_lowercase()));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let mut context = context;
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        self.request_typed_on_connection(
            connection,
            methods::HOOK_RUN,
            &HookRequest {
                hook,
                payload,
                context,
            },
            resource_owner,
        )
        .await
    }

    /// Notifies this API `0.2` process after a completed host mutation.
    ///
    /// The caller owns mutation de-duplication and validates any selected
    /// rescan resources against the context's affected-resource list. This
    /// method never exposes paths, contents, credentials, or partial writes.
    pub async fn post_mutation(
        &self,
        mutation: &PostMutationContext,
        resource_owner: Option<&str>,
    ) -> Result<PostMutationDisposition, ExtensionRuntimeError> {
        if !is_stateful_api(self.api_version())
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::PostMutation)
        {
            return Ok(PostMutationDisposition::NoRescan);
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let generation = connection.generation;
        let mut context = self.execution_context();
        if let Some(resource_owner) = resource_owner {
            context.resource_owner = Some(ExtensionResourceOwner {
                session_id: resource_owner.to_owned(),
                extension_instance_id: self.inner.instance_id.clone(),
                process_generation: generation,
            });
        }
        let payload = serde_json::json!({
            "mutation_id": mutation.mutation_id(),
            "kind": match mutation.kind() {
                crate::extension::PostMutationKind::Configuration => "configuration",
                crate::extension::PostMutationKind::Resource => "resource",
                crate::extension::PostMutationKind::MigrationIngestion => "migration_ingestion",
            },
            "affected_resources": mutation.affected_resources(),
            "generation": mutation.generation(),
            "state": match mutation.state() {
                crate::extension::PostMutationState::Committed => "committed",
                crate::extension::PostMutationState::RolledBack => "rolled_back",
            },
        });
        let output = self
            .run_hook(ExtensionHook::PostMutation, payload, context)
            .await?;
        if read_std_lock(&self.inner.connection).generation != generation {
            return Err(ExtensionRuntimeError::Protocol(
                "discarded stale post_mutation response after process generation changed".into(),
            ));
        }
        let disposition = match output.post_mutation.unwrap_or_default() {
            ExtensionPostMutationDisposition::NoRescan => PostMutationDisposition::NoRescan,
            ExtensionPostMutationDisposition::RequestRescan { resource_ids } => {
                PostMutationDisposition::request_rescan(resource_ids).ok_or_else(|| {
                    ExtensionRuntimeError::Protocol(
                        "invalid bounded post_mutation rescan disposition".into(),
                    )
                })?
            }
        };
        Ok(disposition)
    }

    /// Collects prompt context through the typed context contribution point.
    pub async fn collect_context(
        &self,
        prompt: Option<String>,
        context: ExtensionExecutionContext,
    ) -> Result<Vec<ContextContribution>, ExtensionRuntimeError> {
        if !self.inner.contributions.context {
            return Err(self.undeclared("context contribution", "context".into()));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let mut context = context;
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        self.request_typed_on_connection(
            connection,
            methods::CONTEXT_COLLECT,
            &ContextRequest { prompt, context },
            resource_owner,
        )
        .await
    }

    /// Collects a semantic status, header, or footer contribution.
    pub async fn collect_status(
        &self,
        surface: ExtensionUiSurface,
        context: ExtensionExecutionContext,
    ) -> Result<Option<ExtensionStatusContribution>, ExtensionRuntimeError> {
        if !self.inner.contributions.ui.contains(&surface) {
            return Err(self.undeclared("UI surface", format!("{surface:?}").to_ascii_lowercase()));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let mut context = context;
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        self.request_typed_on_connection(
            connection,
            methods::STATUS_COLLECT,
            &StatusRequest { surface, context },
            resource_owner,
        )
        .await
    }

    /// Collects the extension's complete `/extensions` options menu.
    ///
    /// Every action must route to one of this extension's declared commands;
    /// anything else is a protocol error, never a partially rendered menu.
    pub async fn collect_menu(
        &self,
        context: ExtensionExecutionContext,
    ) -> Result<crate::ExtensionMenu, ExtensionRuntimeError> {
        if !self.inner.contributions.menu {
            return Err(self.undeclared("menu", "menu".to_owned()));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let mut context = context;
        context.resource_owner = context.resource_owner.map(|owner| ExtensionResourceOwner {
            session_id: owner.session_id,
            extension_instance_id: self.inner.instance_id.clone(),
            process_generation: connection.generation,
        });
        let resource_owner = context.resource_owner.clone();
        let menu: crate::ExtensionMenu = self
            .request_typed_on_connection(
                connection,
                methods::MENU_COLLECT,
                &MenuRequest { context },
                resource_owner,
            )
            .await?;
        let declared = self
            .inner
            .contributions
            .commands
            .iter()
            .map(|command| command.name.clone())
            .collect::<Vec<_>>();
        menu.validate(&declared)
            .map_err(ExtensionRuntimeError::Protocol)?;
        Ok(menu)
    }

    /// Asks an extension to semantically render a declared tool lifecycle.
    pub async fn render_tool(
        &self,
        mut request: ToolRenderRequest,
    ) -> Result<RenderedToolCall, ExtensionRuntimeError> {
        if !self
            .inner
            .contributions
            .tool_renderers
            .iter()
            .any(|name| name == &request.name)
        {
            return Err(self.undeclared("tool renderer", request.name));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        request.context.resource_owner =
            request
                .context
                .resource_owner
                .map(|owner| ExtensionResourceOwner {
                    session_id: owner.session_id,
                    extension_instance_id: self.inner.instance_id.clone(),
                    process_generation: connection.generation,
                });
        let resource_owner = request.context.resource_owner.clone();
        let rendered: RenderedToolCall = self
            .request_typed_on_connection(connection, methods::TOOL_RENDER, &request, resource_owner)
            .await?;
        rendered
            .validate()
            .map_err(ExtensionRuntimeError::Protocol)?;
        Ok(rendered)
    }

    /// Queries one registered host-mediated autocomplete chain without granting
    /// it terminal or editor ownership.
    pub async fn request_autocomplete(
        &self,
        request: ExtensionAutocompleteRequest,
    ) -> Result<ExtensionAutocompleteResponse, ExtensionRuntimeError> {
        request
            .validate()
            .map_err(ExtensionRuntimeError::Protocol)?;
        let connection = read_std_lock(&self.inner.connection).clone();
        if !read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_AUTOCOMPLETE) {
            return Err(ExtensionRuntimeError::Protocol(
                "extension did not negotiate autocomplete".into(),
            ));
        }
        let edit_v1 =
            read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_AUTOCOMPLETE_EDIT_V1);
        let response: ExtensionAutocompleteResponse = self
            .request_typed_on_connection(connection, methods::AUTOCOMPLETE_COMPLETE, &request, None)
            .await?;
        response
            .validate_for_request(&request, edit_v1)
            .map_err(ExtensionRuntimeError::Protocol)?;
        Ok(response)
    }

    /// Delivers a host-owned editor snapshot to an extension. The request is a
    /// no-op when the extension did not negotiate editor handoff.
    pub fn notify_editor_state(
        &self,
        response: ExtensionEditorResponse,
    ) -> Result<(), ExtensionRuntimeError> {
        validate_extension_editor_text(&response.text).map_err(ExtensionRuntimeError::Protocol)?;
        self.queue_ui_notification(
            EXTENSION_FEATURE_EDITOR_HANDOFF,
            methods::UI_EDITOR_STATE,
            &response,
        )
    }

    /// Delivers one observer-only normalized terminal input event. Extensions
    /// cannot consume, replace, or delay normal host input handling.
    pub fn notify_terminal_input(
        &self,
        input: ExtensionTerminalInput,
    ) -> Result<(), ExtensionRuntimeError> {
        if input.data.len() > MAX_EXTENSION_TERMINAL_INPUT_BYTES
            || input.data.chars().any(|character| character == '\u{1b}')
        {
            return Err(ExtensionRuntimeError::Protocol(
                "normalized terminal input exceeded its bounded plain-text contract".into(),
            ));
        }
        self.queue_ui_notification(
            EXTENSION_FEATURE_TERMINAL_INPUT,
            methods::UI_TERMINAL_INPUT,
            &input,
        )
    }

    /// Takes at most one latest validated frame per surface from the current
    /// generation. Frames never pass through the broadcast/model event stream.
    pub fn take_remote_ui_frames(&self) -> Vec<ExtensionRemoteUiFrame> {
        let connection = read_std_lock(&self.inner.connection);
        if !connection_is_usable(&connection) || connection.draining.load(Ordering::Acquire) {
            return Vec::new();
        }
        connection.remote_ui.take_frames()
    }

    /// Tests whether an admitted open or active surface still belongs to this
    /// owner. Frontends use this on wake to restore UI after request cancellation
    /// even when the extension's generation itself remains healthy.
    pub fn remote_ui_surface_is_current(
        &self,
        owner: &ExtensionResourceOwner,
        surface_id: &str,
    ) -> bool {
        let connection = read_std_lock(&self.inner.connection);
        connection_is_usable(&connection)
            && !connection.draining.load(Ordering::Acquire)
            && owner.process_generation == connection.generation
            && owner.extension_instance_id == self.inner.instance_id
            && connection.remote_ui.contains(owner, surface_id)
    }

    /// Atomically admits one editor draft mutation and its exact checkpoint ACK
    /// against parent/child cancellation, surface retirement and generation
    /// replacement. Writer capacity is reserved before `commit` can run.
    ///
    /// `commit` must synchronously validate and mutate only local frontend state;
    /// it must not call process/mailbox APIs, await, or perform IO. On failure it
    /// must leave the draft unchanged. Success already admits the response: do
    /// not also call `respond_to_extension_request` for this checkpoint.
    pub fn commit_editor_checkpoint(
        &self,
        request_id: &ExtensionRequestId,
        generation: u64,
        owner: &ExtensionResourceOwner,
        checkpoint: &ExtensionEditorCheckpoint,
        commit: impl FnOnce() -> Result<(), (ExtensionRequestFailure, String)>,
    ) -> Result<(), (ExtensionRequestFailure, String)> {
        let connection = read_std_lock(&self.inner.connection);
        if generation != connection.generation
            || owner.process_generation != generation
            || owner.extension_instance_id != self.inner.instance_id
            || !connection_is_usable(&connection)
        {
            return Err((
                ExtensionRequestFailure::NotForegroundOwner,
                "editor checkpoint process generation is no longer current".into(),
            ));
        }
        connection.commit_editor_checkpoint(request_id, owner, checkpoint, commit)
    }

    /// Queues focused input without waiting for extension rendering or stdin IO.
    pub fn notify_remote_ui_key(
        &self,
        key: ExtensionRemoteUiKey,
    ) -> Result<(), ExtensionRuntimeError> {
        key.validate()
            .map_err(|(_, detail)| ExtensionRuntimeError::Protocol(detail))?;
        let connection = read_std_lock(&self.inner.connection);
        self.remote_ui_notification_owner(&connection, &key.surface_id)?;
        Self::queue_remote_ui_notification(&connection, methods::UI_KEY, &key)
    }

    /// Queues normalized mouse input only for an admitted fullscreen capture
    /// lease, without touching the terminal or waiting for extension rendering.
    pub fn notify_remote_ui_mouse(
        &self,
        mouse: ExtensionRemoteUiMouse,
    ) -> Result<(), ExtensionRuntimeError> {
        mouse
            .validate()
            .map_err(|(_, detail)| ExtensionRuntimeError::Protocol(detail))?;
        let connection = read_std_lock(&self.inner.connection);
        let owner = self.remote_ui_notification_owner(&connection, &mouse.surface_id)?;
        connection
            .remote_ui
            .validate_mouse(&owner, &mouse)
            .map_err(|(_, detail)| ExtensionRuntimeError::Protocol(detail))?;
        Self::queue_remote_ui_notification(&connection, methods::UI_MOUSE, &mouse)
    }

    /// Invalidates cached old-size frames and queues the host's new geometry.
    pub fn notify_remote_ui_resize(
        &self,
        resize: ExtensionRemoteUiResize,
    ) -> Result<(), ExtensionRuntimeError> {
        resize
            .validate()
            .map_err(|(_, detail)| ExtensionRuntimeError::Protocol(detail))?;
        let connection = read_std_lock(&self.inner.connection);
        let owner = self.remote_ui_notification_owner(&connection, &resize.surface_id)?;
        // Host geometry is authoritative even if bounded delivery is refused.
        connection.remote_ui.resize(&owner, &resize);
        Self::queue_remote_ui_notification(&connection, methods::UI_RESIZE, &resize)
    }

    /// Discards the surface immediately; host restoration never waits for the
    /// best-effort bounded observation to reach the extension.
    pub fn notify_remote_ui_closed(
        &self,
        closed: ExtensionRemoteUiClosed,
    ) -> Result<(), ExtensionRuntimeError> {
        closed
            .validate()
            .map_err(|(_, detail)| ExtensionRuntimeError::Protocol(detail))?;
        let connection = read_std_lock(&self.inner.connection);
        let owner = self.remote_ui_notification_owner(&connection, &closed.surface_id)?;
        connection.remote_ui.close(&owner, &closed.surface_id);
        Self::queue_remote_ui_notification(&connection, methods::UI_CLOSED, &closed)
    }

    fn remote_ui_notification_owner(
        &self,
        connection: &ProcessConnection,
        surface_id: &str,
    ) -> Result<ExtensionResourceOwner, ExtensionRuntimeError> {
        if !connection_is_usable(connection) || connection.draining.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeError::Closed(
                "remote UI generation is unavailable".into(),
            ));
        }
        let protocol = read_std_lock(&connection.protocol);
        if protocol.version != EXTENSION_API_VERSION_0_4
            || !protocol.supports(EXTENSION_FEATURE_REMOTE_UI)
            || !connection.remote_ui.is_bound()
        {
            return Err(ExtensionRuntimeError::Protocol(
                "remote UI was not negotiated".into(),
            ));
        }
        let owner = connection
            .remote_ui
            .host_owner(surface_id)
            .map_err(|(_, detail)| ExtensionRuntimeError::Protocol(detail))?;
        if owner.process_generation != connection.generation
            || owner.extension_instance_id != self.inner.instance_id
            || !lock_std_mutex(&connection.issued_resource_owners).contains(&owner)
        {
            return Err(ExtensionRuntimeError::Closed(
                "remote UI owner is stale".into(),
            ));
        }
        Ok(owner)
    }

    fn queue_remote_ui_notification<T: Serialize>(
        connection: &ProcessConnection,
        method: &str,
        params: &T,
    ) -> Result<(), ExtensionRuntimeError> {
        let params = serde_json::to_value(params)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        if connection.queue_notification(method, params) {
            Ok(())
        } else {
            Err(ExtensionRuntimeError::Closed(format!(
                "unable to queue `{method}` notification"
            )))
        }
    }

    /// Delivers one observer-only terminal resize event.
    pub fn notify_terminal_resize(
        &self,
        resize: ExtensionTerminalResize,
    ) -> Result<(), ExtensionRuntimeError> {
        self.queue_ui_notification(
            EXTENSION_FEATURE_TERMINAL_INPUT,
            methods::UI_RESIZE,
            &resize,
        )
    }

    pub(super) fn queue_ui_notification<T: Serialize>(
        &self,
        feature: &str,
        method: &str,
        params: &T,
    ) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        if !read_std_lock(&connection.protocol).supports(feature) {
            return Ok(());
        }
        connection.require_api_v03_host_method(method)?;
        let params = serde_json::to_value(params)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        if connection.queue_notification(method, params) {
            Ok(())
        } else {
            Err(ExtensionRuntimeError::Closed(format!(
                "unable to queue `{method}` notification"
            )))
        }
    }

    /// Best-effort Wave-1 lifecycle fan-out. A closed, draining, or replaced
    /// generation drops the notification instead of erroring a live turn.
    pub(super) fn queue_lifecycle_notification<T: Serialize>(
        &self,
        feature: &str,
        method: &str,
        params: &T,
    ) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        if !connection_is_usable(&connection) {
            return Ok(());
        }
        self.queue_ui_notification(feature, method, params)
    }

    /// Answers one admitted owner-scoped request only while its generation is
    /// current. This is the resource-owner fence for the request surface.
    pub async fn respond_to_extension_request(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        outcome: ExtensionRequestOutcome,
    ) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "extension request belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        match outcome {
            ExtensionRequestOutcome::Ok(result) => {
                connection.send_child_response(request_id, &result).await
            }
            ExtensionRequestOutcome::Failed(failure, detail) => {
                connection
                    .send_child_error_response(request_id, failure.code(), failure.message(&detail))
                    .await
            }
        }
    }

    /// Coalesces one observed assistant delta. A `message/updated` notification
    /// is emitted only at a flush boundary, so no per-delta round trip exists.
    pub fn push_message_delta(&self, delta: &str) -> Result<(), ExtensionRuntimeError> {
        if delta.len() > MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "message delta exceeded {MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES} UTF-8 bytes"
            )));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        let (batches, message_id) = {
            let mut coalescer = lock_std_mutex(&connection.message_deltas);
            let batches = coalescer.push(delta, Instant::now());
            (batches, coalescer.active_message_id())
        };
        self.emit_message_delta_batches(batches, message_id)
    }

    /// Flushes every coalesced delta as one bounded `message/updated`.
    pub fn flush_message_deltas(&self) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let (batches, message_id) = {
            let mut coalescer = lock_std_mutex(&connection.message_deltas);
            let batches = coalescer.flush(Instant::now());
            (batches, coalescer.active_message_id())
        };
        self.emit_message_delta_batches(batches, message_id)
    }

    pub(super) fn emit_message_delta_batches(
        &self,
        batches: Vec<MessageDeltaBatch>,
        message_id: Option<String>,
    ) -> Result<(), ExtensionRuntimeError> {
        for batch in batches {
            self.queue_lifecycle_notification(
                EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
                methods::MESSAGE_UPDATED,
                &batch.into_updated(message_id.clone()),
            )?;
        }
        Ok(())
    }

    /// Opens one observable assistant message boundary.
    pub fn notify_message_started(&self, message_id: &str) -> Result<(), ExtensionRuntimeError> {
        let message_id =
            bounded_notification_text("message id", message_id, MAX_EXTENSION_UI_KEY_BYTES)?;
        let connection = read_std_lock(&self.inner.connection).clone();
        lock_std_mutex(&connection.message_deltas).begin_message(&message_id);
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::MESSAGE_STARTED,
            &ExtensionMessageLifecycle {
                message_id: Some(message_id),
            },
        )
    }

    /// Closes one observable assistant message, flushing coalesced deltas first.
    pub fn notify_message_settled(&self, message_id: &str) -> Result<(), ExtensionRuntimeError> {
        let message_id =
            bounded_notification_text("message id", message_id, MAX_EXTENSION_UI_KEY_BYTES)?;
        self.flush_message_deltas()?;
        let connection = read_std_lock(&self.inner.connection).clone();
        lock_std_mutex(&connection.message_deltas).end_message();
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::MESSAGE_SETTLED,
            &ExtensionMessageLifecycle {
                message_id: Some(message_id),
            },
        )
    }

    /// Begins one host compaction boundary.
    pub fn notify_compaction_started(&self) -> Result<(), ExtensionRuntimeError> {
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::COMPACTION_STARTED,
            &ExtensionCompactionReport { reason: None },
        )
    }

    /// Reports one successful host compaction.
    pub fn notify_compaction_settled(&self) -> Result<(), ExtensionRuntimeError> {
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::COMPACTION_SETTLED,
            &ExtensionCompactionReport { reason: None },
        )
    }

    /// Reports one failed host compaction with a bounded reason.
    pub fn notify_compaction_failed(&self, reason: &str) -> Result<(), ExtensionRuntimeError> {
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::COMPACTION_FAILED,
            &ExtensionCompactionReport {
                reason: Some(truncated_notification_text(
                    reason,
                    MAX_LIFECYCLE_REASON_BYTES,
                )),
            },
        )
    }

    /// Reports a host session name or label change for the current host state.
    pub fn notify_session_info_changed(&self) -> Result<(), ExtensionRuntimeError> {
        let host = read_std_lock(&self.inner.host_state).clone();
        let Some(session_id) = host.session_id.clone() else {
            return Ok(());
        };
        let session_id =
            bounded_notification_text("session id", &session_id, MAX_EXTENSION_UI_KEY_BYTES)?;
        let name = match host.session_name {
            Some(name) => Some(bounded_notification_text(
                "session name",
                &name,
                MAX_EXTENSION_SESSION_NAME_BYTES,
            )?),
            None => None,
        };
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::SESSION_INFO_CHANGED,
            &ExtensionSessionInfoChanged {
                session_id,
                name,
                entry_id: None,
            },
        )
    }

    /// Reports one durable entry label change for the current host session.
    pub fn notify_entry_label_changed(&self, entry_id: &str) -> Result<(), ExtensionRuntimeError> {
        let host = read_std_lock(&self.inner.host_state).clone();
        let Some(session_id) = host.session_id.clone() else {
            return Ok(());
        };
        let session_id =
            bounded_notification_text("session id", &session_id, MAX_EXTENSION_UI_KEY_BYTES)?;
        let entry_id = bounded_notification_text(
            "session entry id",
            entry_id,
            MAX_CONFIRMATION_REQUEST_ID_BYTES,
        )?;
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::SESSION_INFO_CHANGED,
            &ExtensionSessionInfoChanged {
                session_id,
                name: None,
                entry_id: Some(entry_id),
            },
        )
    }

    /// Opens one host-owned dialog boundary.
    pub fn notify_dialog_started(&self, dialog: &str) -> Result<(), ExtensionRuntimeError> {
        let dialog = bounded_notification_text("dialog", dialog, MAX_EXTENSION_UI_KEY_BYTES)?;
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::DIALOG_STARTED,
            &ExtensionDialogLifecycle { dialog },
        )
    }

    /// Closes one host-owned dialog boundary.
    pub fn notify_dialog_settled(&self, dialog: &str) -> Result<(), ExtensionRuntimeError> {
        let dialog = bounded_notification_text("dialog", dialog, MAX_EXTENSION_UI_KEY_BYTES)?;
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::DIALOG_SETTLED,
            &ExtensionDialogLifecycle { dialog },
        )
    }

    /// Reports the current host model selection.
    pub fn notify_model_selected(&self) -> Result<(), ExtensionRuntimeError> {
        let host = read_std_lock(&self.inner.host_state).clone();
        let model = match host.model {
            Some(model) => Some(bounded_notification_text(
                "model",
                &model,
                MAX_EXTENSION_UI_KEY_BYTES,
            )?),
            None => None,
        };
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::MODEL_SELECTED,
            &ExtensionModelSelected {
                model,
                model_view: host.model_view,
            },
        )
    }

    /// Reports the current host reasoning selection.
    pub fn notify_reasoning_selected(&self) -> Result<(), ExtensionRuntimeError> {
        let host = read_std_lock(&self.inner.host_state).clone();
        let reasoning = match host.reasoning {
            Some(serde_json::Value::String(level)) => Some(truncated_notification_text(
                &level,
                MAX_EXTENSION_UI_KEY_BYTES,
            )),
            Some(value) => Some(truncated_notification_text(
                &value.to_string(),
                MAX_EXTENSION_UI_KEY_BYTES,
            )),
            None => None,
        };
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::REASONING_SELECTED,
            &ExtensionReasoningSelected { reasoning },
        )
    }

    /// Reports one user `!`/`!!` bash execution.
    pub fn notify_user_bash(&self, command: &str) -> Result<(), ExtensionRuntimeError> {
        let command =
            bounded_notification_text("bash command", command, MAX_EXTENSION_BASH_COMMAND_BYTES)?;
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
            methods::BASH_USER,
            &ExtensionUserBash { command },
        )
    }

    /// Fires one runtime-registered shortcut back to its owning extension.
    pub fn notify_shortcut_trigger(&self, shortcut_id: &str) -> Result<(), ExtensionRuntimeError> {
        let id =
            bounded_notification_text("shortcut id", shortcut_id, MAX_EXTENSION_SHORTCUT_ID_BYTES)?;
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_SHORTCUTS,
            methods::SHORTCUT_TRIGGER,
            &ExtensionShortcutTrigger { id },
        )
    }

    /// Revokes one granted foreground terminal with a bounded reason.
    ///
    /// Best-effort like the Wave-1 lifecycle fan-out: an extension that did not
    /// negotiate `terminal_handoff` is a silent no-op, and a closed or draining
    /// generation drops the notification instead of failing a live turn. The
    /// frontend fires this exactly when a grant it handed out stopped being
    /// valid: the holder crashed, the session or process generation moved, or a
    /// coordinated shutdown force-restored the terminal.
    pub fn notify_terminal_grant_lost(&self, reason: &str) -> Result<(), ExtensionRuntimeError> {
        self.queue_lifecycle_notification(
            EXTENSION_FEATURE_TERMINAL_HANDOFF,
            methods::TERMINAL_GRANT_LOST,
            &TerminalGrantLost {
                reason: truncated_notification_text(reason, MAX_LIFECYCLE_REASON_BYTES),
            },
        )
    }

    /// Answers a process-originated editor request only when its generation is
    /// still current. This is the editor lease's stale-owner fence.
    pub async fn respond_to_editor(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        response: ExtensionEditorResponse,
    ) -> Result<(), ExtensionRuntimeError> {
        validate_extension_editor_text(&response.text).map_err(ExtensionRuntimeError::Protocol)?;
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "editor request belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        if !read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_EDITOR_HANDOFF) {
            return Err(ExtensionRuntimeError::Protocol(
                "extension did not negotiate editor_handoff".into(),
            ));
        }
        connection.send_child_response(request_id, &response).await
    }

    /// Acknowledges an autocomplete registration only when the original
    /// generation is still current.
    pub async fn respond_to_autocomplete_registration(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        accepted: bool,
    ) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "autocomplete registration belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        if !read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_AUTOCOMPLETE) {
            return Err(ExtensionRuntimeError::Protocol(
                "extension did not negotiate autocomplete".into(),
            ));
        }
        connection
            .send_child_response(request_id, &serde_json::json!({"accepted": accepted}))
            .await
    }

    /// Answers a process-originated confirmation request. Requests from a
    /// previous process generation are rejected after reload.
    pub async fn respond_to_confirmation(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        response: ConfirmationResponse,
    ) -> Result<(), ExtensionRuntimeError> {
        request_id
            .validate_confirmation_id()
            .map_err(ExtensionRuntimeError::Protocol)?;
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "confirmation belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        if !self.inner.contributions.confirmations {
            return Err(self.undeclared("confirmation capability", "confirmations".into()));
        }
        {
            let mut answered = lock_std_mutex(&self.inner.answered_confirmations);
            if !answered.insert(generation, request_id.clone()) {
                return Ok(());
            }
        }
        let mut reservation = AnsweredConfirmationReservation {
            answered: &self.inner.answered_confirmations,
            generation,
            request_id: request_id.clone(),
            committed: false,
        };
        connection
            .send_child_response(request_id.clone(), &response)
            .await?;
        reservation.commit();
        Ok(())
    }

    /// Whether a frontend or tool-progress consumer already answered this
    /// request. Product event drains use this to avoid duplicate UI/actions.
    pub fn confirmation_answered(&self, request_id: &ExtensionRequestId, generation: u64) -> bool {
        lock_std_mutex(&self.inner.answered_confirmations).contains(generation, request_id)
    }

    /// Checks an adapter's target against the exact host-issued, owner-scoped
    /// tool call. Commands, settled requests, and other generations cannot lend
    /// their authority to a tool policy request. The digest never retains raw
    /// tool arguments beyond the existing request frame.
    pub fn policy_matches_tool_call(
        &self,
        generation: u64,
        parent_request_id: u64,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return false;
        }
        let expected = tool_call_policy_digest(tool, arguments);
        let pending = lock_std_mutex(&connection.pending);
        pending.get(&parent_request_id).is_some_and(|parent| {
            parent.resource_owner.is_some()
                && parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                && parent.tool_call_policy_digest == Some(expected)
        })
    }

    /// Answers an extension-originated API `0.2` policy evaluation request.
    /// Classification and approval issuance remain host-owned; this method
    /// only sends the already-decided typed result to the matching generation.
    pub async fn respond_to_policy_evaluation(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        response: ExtensionPolicyEvaluationResponse,
    ) -> Result<(), ExtensionRuntimeError> {
        if response.approval_token.is_some() {
            return Err(ExtensionRuntimeError::Protocol(
                "approval capabilities must be issued by respond_to_policy_approval".into(),
            ));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "policy request belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        if !read_std_lock(&connection.protocol).supports("policy_intents") {
            return Err(ExtensionRuntimeError::Protocol(
                "extension did not negotiate policy_intents".into(),
            ));
        }
        connection.send_child_response(request_id, &response).await
    }

    /// Resolves an interactive policy prompt and, when approved, issues a
    /// short-lived single-use capability bound to the exact intent, process
    /// generation, and originating parent request.
    ///
    /// An approved response remains `ask` and carries the capability. The
    /// extension must repeat `policy/evaluate` with that token; the host then
    /// atomically consumes it and returns `allow`. This keeps approval
    /// consumption on the operation boundary instead of treating a UI answer
    /// as authority forever.
    pub async fn respond_to_policy_approval(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        parent_request_id: u64,
        approved: bool,
        ttl: Duration,
    ) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "policy request belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        let supports_approvals = {
            let protocol = read_std_lock(&connection.protocol);
            protocol.supports(EXTENSION_FEATURE_POLICY_INTENTS)
                && protocol.supports(EXTENSION_FEATURE_APPROVALS)
        };
        if !supports_approvals {
            return Err(ExtensionRuntimeError::Protocol(
                "extension did not negotiate policy_intents and approvals".into(),
            ));
        }
        let requested_intent = {
            let children = lock_std_mutex(&connection.child_requests);
            children
                .get(&request_id)
                .filter(|child| {
                    child.parent_request_id == parent_request_id
                        && child.response_state.state.load(Ordering::Acquire) == CHILD_ACTIVE
                })
                .and_then(|child| child.policy_intent.clone())
        };
        let parent_has_owner = {
            let pending = lock_std_mutex(&connection.pending);
            pending
                .get(&parent_request_id)
                .is_some_and(|parent| parent.resource_owner.is_some())
        };
        let Some(intent) = requested_intent else {
            return Err(ExtensionRuntimeError::Closed(
                "policy approval no longer belongs to its original active intent".into(),
            ));
        };
        if !parent_has_owner {
            return Err(ExtensionRuntimeError::Closed(
                "policy approval no longer belongs to an active owner-scoped request".into(),
            ));
        }
        if !approved {
            return connection
                .send_child_response(
                    request_id,
                    &ExtensionPolicyEvaluationResponse {
                        decision: ExtensionPolicyDecision::Deny,
                        approval_token: None,
                    },
                )
                .await;
        }
        let parent = ExtensionRequestId::Number(parent_request_id);
        let token = self
            .inner
            .approval_store
            .issue(&intent, generation, parent.clone(), ttl)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let response = ExtensionPolicyEvaluationResponse {
            decision: ExtensionPolicyDecision::Ask,
            approval_token: Some(token.clone()),
        };
        let sent = connection
            .send_child_response_admitted(request_id, &response)
            .await;
        if !matches!(sent, Ok(ChildResponseAdmission::Queued)) {
            let _ = self
                .inner
                .approval_store
                .consume(&token, &intent, generation, &parent);
        }
        sent.map(|_| ())
    }

    /// Answers an extension-originated API `0.2` ephemeral input request.
    /// A `None` value is the deterministic cancellation/no-frontend answer.
    pub async fn respond_to_input(
        &self,
        request_id: ExtensionRequestId,
        generation: u64,
        response: ExtensionInputResponse,
    ) -> Result<(), ExtensionRuntimeError> {
        request_id
            .validate_confirmation_id()
            .map_err(ExtensionRuntimeError::Protocol)?;
        if response
            .value
            .as_ref()
            .is_some_and(|value| value.len() > MAX_EXTENSION_INPUT_VALUE_BYTES)
        {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "input response exceeded {MAX_EXTENSION_INPUT_VALUE_BYTES} UTF-8 bytes"
            )));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation {
            return Err(ExtensionRuntimeError::Closed(format!(
                "input request belongs to stale generation {generation}; current generation is {}",
                connection.generation
            )));
        }
        if !is_stateful_api(&read_std_lock(&connection.protocol).version) {
            return Err(ExtensionRuntimeError::Protocol(
                "input/request requires API 0.2".into(),
            ));
        }
        {
            let mut answered = lock_std_mutex(&self.inner.answered_inputs);
            if !answered.insert(generation, request_id.clone()) {
                return Ok(());
            }
        }
        let mut reservation = AnsweredConfirmationReservation {
            answered: &self.inner.answered_inputs,
            generation,
            request_id: request_id.clone(),
            committed: false,
        };
        connection
            .send_child_response(request_id, &response)
            .await?;
        reservation.commit();
        Ok(())
    }

    /// Whether an interactive owner already answered this input request.
    pub fn input_answered(&self, request_id: &ExtensionRequestId, generation: u64) -> bool {
        lock_std_mutex(&self.inner.answered_inputs).contains(generation, request_id)
    }

    /// Sends a non-veto lifecycle observation to a subscribed API `0.2`
    /// extension. Non-negotiated events are successful no-ops.
    pub async fn notify_lifecycle(
        &self,
        event: &ExtensionLifecycleEvent,
    ) -> Result<(), ExtensionRuntimeError> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let current = LifecycleEndpoint {
            generation: connection.generation,
            connection,
        };
        let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
        if current.connection.draining.load(Ordering::Acquire)
            && matches!(
                event,
                ExtensionLifecycleEvent::SessionStarted { .. }
                    | ExtensionLifecycleEvent::TurnStarted { .. }
            )
        {
            return Ok(());
        }
        if current.connection.draining.load(Ordering::Acquire)
            && lifecycle.sessions.is_empty()
            && lifecycle.turns.is_empty()
            && lifecycle.tools.is_empty()
        {
            return Ok(());
        }
        let mut skip_delivery = false;
        let endpoint = match event {
            ExtensionLifecycleEvent::SessionSettled { session_id, .. } => lifecycle
                .sessions
                .get(session_id)
                .map(|session| session.endpoint.clone())
                .unwrap_or_else(|| current.clone()),
            ExtensionLifecycleEvent::TurnStarted { turn_id, .. } => {
                if let Some(turn) = lifecycle
                    .turns
                    .values()
                    .find(|turn| turn.context.turn_id == *turn_id)
                {
                    if turn.start_queued {
                        return Ok(());
                    }
                    turn.endpoint.clone()
                } else {
                    current.clone()
                }
            }
            ExtensionLifecycleEvent::TurnSettled { turn_id, .. } => {
                if let Some(turn) = lifecycle
                    .turns
                    .values()
                    .find(|turn| turn.context.turn_id == *turn_id)
                {
                    if !turn.start_queued {
                        skip_delivery = true;
                    }
                    turn.endpoint.clone()
                } else {
                    current.clone()
                }
            }
            _ => current,
        };
        let delivery = if skip_delivery {
            Ok(false)
        } else {
            Self::queue_lifecycle_observation(&endpoint, event.clone())
        };
        let queued = matches!(delivery, Ok(true));
        match event {
            ExtensionLifecycleEvent::SessionStarted { session_id, run_id } if queued => {
                lifecycle.sessions.insert(
                    session_id.clone(),
                    ActiveLifecycleSession {
                        session_id: session_id.clone(),
                        run_id: run_id.clone(),
                        started_at: Instant::now(),
                        endpoint,
                    },
                );
            }
            ExtensionLifecycleEvent::SessionSettled { session_id, .. } => {
                lifecycle.sessions.remove(session_id);
            }
            ExtensionLifecycleEvent::TurnStarted { turn_id, .. } if queued => {
                if let Some(turn) = lifecycle
                    .turns
                    .values_mut()
                    .find(|turn| turn.context.turn_id == *turn_id)
                {
                    turn.start_queued = true;
                }
            }
            ExtensionLifecycleEvent::TurnSettled { turn_id, .. } => {
                clear_matching_lifecycle_turn(&mut lifecycle, None, turn_id);
            }
            _ => {}
        }
        delivery.map(|_| ())
    }

    /// Sets the stable turn IDs used by synchronous global tool observation.
    /// Product code must clear this at the same terminal boundary that emits
    /// `turn/settled`.
    pub fn set_active_lifecycle_turn(
        &self,
        resource_owner: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) {
        let connection = read_std_lock(&self.inner.connection).clone();
        if connection.draining.load(Ordering::Acquire) {
            return;
        }
        let endpoint = LifecycleEndpoint {
            generation: connection.generation,
            connection,
        };
        let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
        if endpoint.connection.draining.load(Ordering::Acquire) {
            return;
        }
        let resource_owner = resource_owner.into();
        lifecycle.turns.insert(
            resource_owner.clone(),
            ActiveLifecycleTurn {
                context: ExtensionLifecycleTurnContext {
                    session_id: session_id.into(),
                    run_id: run_id.into(),
                    turn_id: turn_id.into(),
                },
                started_at: Instant::now(),
                endpoint,
                start_queued: false,
            },
        );
        lifecycle
            .tools
            .retain(|(owner, _), _| owner != &resource_owner);
    }

    /// Clears active turn IDs and any unmatched observed tool starts.
    pub fn clear_active_lifecycle_turn(&self, resource_owner: &str, turn_id: &str) {
        let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
        clear_matching_lifecycle_turn(&mut lifecycle, Some(resource_owner), turn_id);
    }

    pub(super) fn queue_lifecycle_observation(
        endpoint: &LifecycleEndpoint,
        event: ExtensionLifecycleEvent,
    ) -> Result<bool, ExtensionRuntimeError> {
        let method = event.method();
        let protocol = read_std_lock(&endpoint.connection.protocol).clone();
        if !protocol.supports(EXTENSION_FEATURE_LIFECYCLE_EVENTS)
            || !protocol.lifecycle_events.contains(method)
        {
            return Ok(false);
        }
        let params = event.params()?;
        if !endpoint.connection.queue_notification(method, params) {
            return Err(ExtensionRuntimeError::Closed(format!(
                "unable to queue lifecycle notification `{method}` for generation {}",
                endpoint.generation
            )));
        }
        Ok(true)
    }

    /// Marks the active generation draining and rejects all new dispatch.
    /// Returns `true` only for the transition which won.
    pub fn begin_drain(&self) -> bool {
        read_std_lock(&self.inner.connection).begin_drain()
    }

    /// Allows admitted operations to settle until `deadline`, then cancels
    /// any remainder without replaying them.
    pub async fn drain(&self, deadline: Duration) -> bool {
        let connection = read_std_lock(&self.inner.connection).clone();
        connection.drain(deadline, "reload drain deadline").await
    }

    /// Restarts the process and atomically swaps it in after a successful
    /// handshake. API `0.2` extensions negotiating `dynamic_tools` may publish
    /// a different tool catalog; frozen/static contribution surfaces must
    /// remain compatible.
    pub async fn reload(&self) -> Result<ExtensionReloadReport, ExtensionRuntimeError> {
        let _guard = self.inner.reload_guard.lock().await;
        self.reload_locked(None).await
    }

    pub(super) async fn reload_locked(
        &self,
        expected_generation: Option<u64>,
    ) -> Result<ExtensionReloadReport, ExtensionRuntimeError> {
        if self.inner.supervisor_cancelled.load(Ordering::Acquire) {
            return Err(ExtensionRuntimeError::Closed(format!(
                "extension `{}` is shutting down",
                self.inner.descriptor.manifest.name
            )));
        }
        let active_generation = read_std_lock(&self.inner.connection).generation;
        if expected_generation.is_some_and(|expected| expected != active_generation) {
            return Err(ExtensionRuntimeError::Closed(format!(
                "extension generation changed before restart (expected {}, active {active_generation})",
                expected_generation.unwrap_or_default()
            )));
        }
        let generation = self
            .inner
            .next_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                next.checked_add(1)
            })
            .map_err(|_| {
                ExtensionRuntimeError::Protocol(
                    "extension process generation space is exhausted".to_owned(),
                )
            })?;
        let host_state = read_std_lock(&self.inner.host_state).clone();
        let (candidate_events, candidate_event_rx) = broadcast::channel(EXTENSION_EVENT_CAPACITY);
        let (replacement, contributions) = spawn_connection(
            &self.inner.descriptor,
            &self.inner.config,
            host_state,
            generation,
            &self.inner.instance_id,
            candidate_events,
            self.inner.artifact_store.clone(),
            self.inner.catalog_updates.clone(),
            Arc::clone(&self.inner.delegation_service),
            Arc::clone(&self.inner.approval_store),
            true,
        )
        .await?;
        tokio::spawn(forward_candidate_events(
            Arc::downgrade(&self.inner),
            generation,
            Arc::downgrade(&replacement),
            candidate_event_rx,
        ));
        let current = read_std_lock(&self.inner.connection).clone();
        let current_dynamic_tools =
            read_std_lock(&current.protocol).supports(EXTENSION_FEATURE_DYNAMIC_TOOLS);
        let replacement_dynamic_tools =
            read_std_lock(&replacement.protocol).supports(EXTENSION_FEATURE_DYNAMIC_TOOLS);
        if current_dynamic_tools != replacement_dynamic_tools {
            replacement.terminate().await;
            let _ = self.inner.artifact_store.settle_generation(generation);
            return Err(ExtensionRuntimeError::ReloadRequiresReregistration {
                extension: self.inner.descriptor.manifest.name.clone(),
            });
        }
        let dynamic_tools = current_dynamic_tools;
        if !contributions_compatible(&self.inner.contributions, &contributions, dynamic_tools) {
            replacement.terminate().await;
            let _ = self.inner.artifact_store.settle_generation(generation);
            return Err(ExtensionRuntimeError::ReloadRequiresReregistration {
                extension: self.inner.descriptor.manifest.name.clone(),
            });
        }
        let replacement_tools = read_std_lock(&replacement.tool_catalog).clone();
        let dynamic_registration = lock_std_mutex(&self.inner.dynamic_tool_registration).clone();
        let mut replacement_reservation = if let Some(registration) = &dynamic_registration {
            let replacement_process_tools =
                self.process_tools(Arc::clone(&replacement), &replacement_tools);
            let revision = Arc::clone(&replacement_process_tools.revision);
            match registration.reserve(replacement_process_tools.tools) {
                Ok(reservation) => Some((reservation, revision)),
                Err(message) => {
                    replacement.terminate().await;
                    let _ = self.inner.artifact_store.settle_generation(generation);
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "replacement tool catalog was rejected: {message}"
                    )));
                }
            }
        } else {
            None
        };
        if !connection_is_usable(&replacement) {
            replacement.terminate().await;
            let _ = self.inner.artifact_store.settle_generation(generation);
            return Err(ExtensionRuntimeError::Closed(format!(
                "replacement generation {generation} exited before reload cutover"
            )));
        }

        let previous = {
            let active = write_std_lock(&self.inner.connection);
            let previous = Arc::clone(&active);
            previous.begin_drain();
            previous
        };

        // Give already-admitted requests their bounded natural settlement
        // window before synthesizing interruption for lifecycle records that
        // remain active.
        let previous_quiesced = previous.quiesce(self.inner.config.shutdown_timeout).await;
        if !connection_is_usable(&replacement) {
            if previous_quiesced {
                previous.resume_after_failed_drain();
            } else {
                previous.cancel_all_pending("replacement exited during reload drain");
                if let Some(registration) = &dynamic_registration {
                    registration.remove();
                }
                let _ = previous.shutdown().await;
            }
            replacement.terminate().await;
            let _ = self.inner.artifact_store.settle_generation(generation);
            return Err(ExtensionRuntimeError::Closed(format!(
                "replacement generation {generation} exited while the active generation drained"
            )));
        }

        let replacement_endpoint = LifecycleEndpoint {
            generation,
            connection: Arc::clone(&replacement),
        };
        let previous_crashed = previous.closed.load(Ordering::Acquire);
        let session_hook_end_reason = if previous_crashed { "crash" } else { "reload" };
        let settled_session_hook_bindings =
            self.take_session_hook_bindings_for_generation(previous.generation);
        if !previous_crashed {
            let session_hook_ends = futures_util::future::join_all(
                settled_session_hook_bindings.iter().map(|binding| {
                    self.dispatch_session_hook_end(
                        binding,
                        &binding.endpoint,
                        ExtensionLifecycleOutcome::Interrupted,
                        session_hook_end_reason,
                    )
                }),
            )
            .await;
            for result in session_hook_ends {
                if let Err(error) = result {
                    self.emit_session_hook_failure("session_end", &error);
                }
            }
        }

        let (old_turns, old_sessions) = {
            let _active = write_std_lock(&self.inner.connection);
            let previous_generation = previous.generation;
            let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
            let reason = Some("extension generation reloaded".to_owned());

            let mut old_tools = Vec::new();
            let mut retained_tools = HashMap::new();
            for (tool_key, tool) in std::mem::take(&mut lifecycle.tools) {
                if tool.endpoint.generation == previous_generation {
                    old_tools.push((tool_key, tool));
                } else {
                    retained_tools.insert(tool_key, tool);
                }
            }
            old_tools.sort_by(|left, right| left.0.cmp(&right.0));
            lifecycle.tools = retained_tools;

            let mut old_turns = Vec::new();
            lifecycle.turns.retain(|owner, turn| {
                if turn.endpoint.generation == previous_generation {
                    old_turns.push((owner.clone(), turn.clone()));
                    false
                } else {
                    true
                }
            });
            old_turns.sort_by(|left, right| left.0.cmp(&right.0));
            let mut old_sessions = Vec::new();
            lifecycle.sessions.retain(|session_id, session| {
                if session.endpoint.generation == previous_generation {
                    old_sessions.push((session_id.clone(), session.clone()));
                    false
                } else {
                    true
                }
            });
            old_sessions.sort_by(|left, right| left.0.cmp(&right.0));

            for ((_, tool_call_id), tool) in old_tools {
                let _ = Self::queue_lifecycle_observation(
                    &tool.endpoint,
                    ExtensionLifecycleEvent::ToolSettled {
                        session_id: tool.context.session_id,
                        run_id: tool.context.run_id,
                        turn_id: tool.context.turn_id,
                        tool_call_id,
                        tool_name: tool.name,
                        outcome: ExtensionLifecycleOutcome::Interrupted,
                        duration_ms: u64::try_from(tool.started_at.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        reason: reason.clone(),
                    },
                );
            }
            for (_, turn) in old_turns.iter().filter(|(_, turn)| turn.start_queued) {
                let _ = Self::queue_lifecycle_observation(
                    &turn.endpoint,
                    ExtensionLifecycleEvent::TurnSettled {
                        session_id: turn.context.session_id.clone(),
                        run_id: turn.context.run_id.clone(),
                        turn_id: turn.context.turn_id.clone(),
                        outcome: ExtensionLifecycleOutcome::Interrupted,
                        duration_ms: u64::try_from(turn.started_at.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        reason: reason.clone(),
                    },
                );
            }
            for (_, session) in &old_sessions {
                let _ = Self::queue_lifecycle_observation(
                    &session.endpoint,
                    ExtensionLifecycleEvent::SessionSettled {
                        session_id: session.session_id.clone(),
                        run_id: session.run_id.clone(),
                        outcome: ExtensionLifecycleOutcome::Interrupted,
                        duration_ms: u64::try_from(session.started_at.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        reason: reason.clone(),
                    },
                );
            }

            (old_turns, old_sessions)
        };

        // No await is allowed between the final candidate liveness check,
        // lifecycle transfer, connection swap, and catalog publication. This
        // keeps the cutover admission boundary synchronous. The old process is
        // shut down immediately after the new generation becomes authoritative.
        {
            let mut active = write_std_lock(&self.inner.connection);
            let mut lifecycle = lock_std_mutex(&self.inner.lifecycle);
            for (session_key, session) in old_sessions {
                let event = ExtensionLifecycleEvent::SessionStarted {
                    session_id: session.session_id.clone(),
                    run_id: session.run_id.clone(),
                };
                if Self::queue_lifecycle_observation(&replacement_endpoint, event).unwrap_or(false)
                {
                    lifecycle.sessions.insert(
                        session_key,
                        ActiveLifecycleSession {
                            session_id: session.session_id,
                            run_id: session.run_id,
                            started_at: Instant::now(),
                            endpoint: replacement_endpoint.clone(),
                        },
                    );
                }
            }
            for (owner, turn) in old_turns {
                let start_queued = Self::queue_lifecycle_observation(
                    &replacement_endpoint,
                    ExtensionLifecycleEvent::TurnStarted {
                        session_id: turn.context.session_id.clone(),
                        run_id: turn.context.run_id.clone(),
                        turn_id: turn.context.turn_id.clone(),
                    },
                )
                .unwrap_or(false);
                lifecycle.turns.insert(
                    owner,
                    ActiveLifecycleTurn {
                        context: turn.context,
                        started_at: Instant::now(),
                        endpoint: replacement_endpoint.clone(),
                        start_queued,
                    },
                );
            }

            // Cutover is committed. Withdraw the old topic owner before the
            // replacement reader can bind and run its start hooks; speculative
            // initialization and failed reloads leave the old owner intact.
            if let Some(bus) = &previous.event_bus {
                bus.remove(
                    &previous.provider_owner.extension_instance_id,
                    previous.generation,
                );
            }
            // This is a host-owned observation, not a retained append grant.
            // Rebind only at accepted cutover, after old callbacks settled;
            // failed candidates never receive or alter the active mirror.
            let mut mirror = lock_std_mutex(&previous.session_leaf.mirror).clone();
            if let Some(mirror) = &mut mirror {
                mirror.rebind_generation(generation);
            }
            *lock_std_mutex(&replacement.session_leaf.mirror) = mirror;
            *active = Arc::clone(&replacement);
            replacement.activate_post_initialize();
            self.inner.generation.store(generation, Ordering::Release);
            self.inner.generation_changed.notify_waiters();
            let catalog_publication = replacement_reservation
                .take()
                .map(|(reservation, revision)| {
                    reservation.commit_with(|_, published| {
                        // Wire catalog epochs are local to one subprocess.
                        // A replacement starts at the SDK's initial epoch 0;
                        // the host-wide registry revision remains internal.
                        revision.store(0, Ordering::Release);
                        let _catalog = write_std_lock(&replacement.catalog_guard);
                        write_std_lock(&replacement.tool_catalog)
                            .retain(|definition| published.contains(&definition.name));
                        replacement.catalog_revision.store(0, Ordering::Release);
                    })
                })
                .unwrap_or_else(|| {
                    Ok((
                        0,
                        replacement_tools
                            .iter()
                            .map(|definition| definition.name.clone())
                            .collect(),
                    ))
                });
            if let Err(message) = &catalog_publication {
                let _catalog = write_std_lock(&replacement.catalog_guard);
                write_std_lock(&replacement.tool_catalog).clear();
                let _ = self.inner.events.send(ExtensionEvent::Diagnostic {
                    message: format!(
                        "replacement tool catalog could not be published after cutover: {message}"
                    ),
                });
            }
        }
        // A replacement's reader remains paused until cutover. Crash recovery
        // finalizers must therefore wait until the replacement is authoritative
        // before awaiting its response; otherwise the bounded finalizer times
        // out against its own paused reader.
        if previous_crashed {
            let session_hook_ends = futures_util::future::join_all(
                settled_session_hook_bindings.iter().map(|binding| {
                    self.dispatch_session_hook_end(
                        binding,
                        &replacement_endpoint,
                        ExtensionLifecycleOutcome::Interrupted,
                        session_hook_end_reason,
                    )
                }),
            )
            .await;
            for result in session_hook_ends {
                if let Err(error) = result {
                    self.emit_session_hook_failure("session_end", &error);
                }
            }
        }
        let replacement_session_hook_bindings = self.install_replacement_session_hook_bindings(
            settled_session_hook_bindings,
            replacement_endpoint,
        );
        let session_hook_starts = futures_util::future::join_all(
            replacement_session_hook_bindings
                .iter()
                .map(|binding| self.dispatch_session_hook_start(binding)),
        )
        .await;
        for result in session_hook_starts {
            if let Err(error) = result {
                self.emit_session_hook_failure("session_start", &error);
            }
        }
        let previous_shutdown_graceful = previous.shutdown().await;
        self.inner
            .approval_store
            .invalidate_generation(previous.generation);
        lock_std_mutex(&self.inner.answered_confirmations).retain_generation(generation);
        lock_std_mutex(&self.inner.answered_inputs).retain_generation(generation);
        Ok(ExtensionReloadReport {
            generation,
            previous_shutdown_graceful,
        })
    }

    /// Requests graceful shutdown using the configured per-stage timeout, waits
    /// for child exit using that timeout again, then kills it if needed. Returns
    /// whether it acknowledged and exited within their respective stages.
    pub async fn shutdown(&self) -> bool {
        self.inner
            .supervisor_cancelled
            .store(true, Ordering::Release);
        self.inner.generation_changed.notify_waiters();
        let _guard = self.inner.reload_guard.lock().await;
        let connection = read_std_lock(&self.inner.connection).clone();
        self.settle_all_session_hook_bindings_locked(
            ExtensionLifecycleOutcome::Shutdown,
            "shutdown",
        )
        .await;
        let _ = connection
            .drain(self.inner.config.shutdown_timeout, "shutdown")
            .await;
        let graceful = connection.shutdown().await;
        self.inner
            .approval_store
            .invalidate_generation(connection.generation);
        if let Some(registration) = lock_std_mutex(&self.inner.dynamic_tool_registration).as_ref() {
            registration.remove();
        }
        if let Some(service) = write_std_lock(&self.inner.delegation_service).take() {
            service.shutdown_owned();
        }
        graceful
    }

    pub(super) fn execution_context(&self) -> ExtensionExecutionContext {
        ExtensionExecutionContext {
            workspace: self.inner.config.workspace.clone(),
            execution_scope: None,
            resource_owner: None,
            host: read_std_lock(&self.inner.host_state).clone(),
        }
    }

    pub(super) fn require_tool(
        &self,
        connection: &ProcessConnection,
        name: &str,
    ) -> Result<ToolDefinition, ExtensionRuntimeError> {
        read_std_lock(&connection.tool_catalog)
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
            .ok_or_else(|| self.undeclared("tool", name.to_owned()))
    }

    pub(super) fn undeclared(&self, kind: &'static str, name: String) -> ExtensionRuntimeError {
        ExtensionRuntimeError::UndeclaredContribution {
            extension: self.inner.descriptor.manifest.name.clone(),
            kind,
            name,
        }
    }

    pub(super) async fn request_typed_on_connection<P, R>(
        &self,
        connection: Arc<ProcessConnection>,
        method: &'static str,
        params: &P,
        resource_owner: Option<ExtensionResourceOwner>,
    ) -> Result<R, ExtensionRuntimeError>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        connection.require_api_v03_host_method(method)?;
        let params = serde_json::to_value(params)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let result = connection
            .request_with_resource_owner(
                method,
                params,
                self.inner.config.request_timeout,
                resource_owner,
            )
            .await?;
        serde_json::from_value(result).map_err(|error| {
            ExtensionRuntimeError::Protocol(format!(
                "invalid `{method}` response from `{}`: {error}",
                self.inner.descriptor.manifest.name
            ))
        })
    }
}
