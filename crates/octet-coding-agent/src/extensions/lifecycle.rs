//! ExecutableExtensions discovery, start, reload, shutdown and telemetry.

use super::*;

impl ExecutableExtensions {
    /// Discovers and starts extensions with a fresh ordinary-host runtime manager.
    ///
    /// Product bootstrap uses [`Self::discover_and_start_with_provider_runtime`]
    /// to retain compatible workspace services across an App rebuild. This
    /// wrapper preserves the direct construction seam used by focused tests,
    /// which all drive real extension processes.
    #[cfg(all(test, unix))]
    pub fn discover_and_start(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
    ) -> Self {
        Self::discover_and_start_with_runtime_manager(
            config, session, model, reasoning, sessions, host, None,
        )
    }

    /// Discovers a static catalog, binds the current session, and activates
    /// eager lifecycle profiles through the supplied durable manager.
    ///
    /// Reached from the `discover_and_start` test seam and from the
    /// process-startup suite, so it shares that suite's platform gate.
    #[cfg(all(test, unix))]
    pub fn discover_and_start_with_runtime_manager(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
        runtime_manager: Option<ExtensionRuntimeManager>,
    ) -> Self {
        Self::discover_and_start_with_provider_runtime(
            config,
            session,
            model,
            reasoning,
            sessions,
            host,
            runtime_manager,
            ExtensionProviderRuntime::default(),
            crate::app::resource_paths::ResourceConsumerCapability::Disabled,
        )
    }

    /// Discovers and starts extensions using one product-owned provider runtime.
    ///
    /// Bootstrap and rebuild pass the same value through this seam so API 0.3
    /// declarations keep their process-owner fences while the App is replaced.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn discover_and_start_with_provider_runtime(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
        runtime_manager: Option<ExtensionRuntimeManager>,
        provider_runtime: ExtensionProviderRuntime,
        resource_consumer: crate::app::resource_paths::ResourceConsumerCapability,
    ) -> Self {
        let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
        let snapshot = resolver.discover(ResourceKind::Extension, &config.extension_paths);
        let mut diagnostics = snapshot
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}: {}: {}",
                    match diagnostic.level {
                        ResourceDiagnosticLevel::Info => "info",
                        ResourceDiagnosticLevel::Warning => "warning",
                    },
                    diagnostic.path.display(),
                    diagnostic.message
                )
            })
            .collect::<Vec<_>>();

        let (policy, trust_grants) = extension_policy(config, &mut diagnostics);

        // Read through the shared no-follow, bounded boundary, then construct
        // the protocol catalog from validated values. A second filename that
        // declares the same manifest name is retained only as a diagnostic.
        let mut by_name = BTreeMap::<String, DiscoveredExtension>::new();
        for resource in snapshot.resources() {
            let Some(descriptor) =
                load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
            else {
                continue;
            };
            if let Some(first) = by_name.get(&descriptor.manifest.name) {
                diagnostics.push(format!(
                    "warning: {}: extension {:?} duplicates {}; the first manifest wins",
                    descriptor.manifest_path.display(),
                    descriptor.manifest.name,
                    first.manifest_path.display()
                ));
            } else {
                by_name.insert(descriptor.manifest.name.clone(), descriptor);
            }
        }

        let discovered_names = by_name.keys().cloned().collect::<BTreeSet<_>>();
        for name in &config.enabled_extensions {
            if !discovered_names.contains(name) {
                diagnostics.push(format!(
                    "warning: enabled extension {name:?} was not discovered"
                ));
            }
        }
        for grant in &trust_grants {
            if !by_name.values().any(|descriptor| grant.matches(descriptor)) {
                diagnostics.push(format!(
                    "info: trust grant {:?} has no matching discovered extension source",
                    grant.display()
                ));
            }
        }
        for name in &config.invocation_trusted_extensions {
            if !discovered_names.contains(name) {
                diagnostics.push(format!(
                    "info: one-shot trust grant {name:?} has no discovered extension"
                ));
            }
        }

        let mut descriptors = by_name.into_values().collect::<Vec<_>>();
        for descriptor in &mut descriptors {
            apply_experimental_streamable_http_mcp_gate(
                descriptor,
                config.experimental_streamable_http_mcp,
            );
        }
        let mut needs_host_authority = false;
        // A surface that never starts extension processes gets no grant hints:
        // no grant could change the outcome there.
        for descriptor in descriptors
            .iter()
            .filter(|_| config.start_extension_processes)
        {
            match descriptor
                .activation
                .start_decision(descriptor.source, config.workspace_trusted)
            {
                ExtensionStartDecision::NeedsHostAuthority => {
                    diagnostics.push(format!(
                        "warning: {}: extension {:?} is enabled but needs host authority; grant trusted_extensions = [{:?}] in user config or pass --trust-extension {} for this invocation",
                        descriptor.manifest_path.display(),
                        descriptor.manifest.name,
                        persistent_trust_grant(descriptor),
                        descriptor.manifest.name
                    ));
                    needs_host_authority = true;
                }
                ExtensionStartDecision::NeedsWorkspaceTrust => {
                    diagnostics.push(format!(
                        "warning: {}: trust this workspace first to start the project extension {:?}",
                        descriptor.manifest_path.display(), descriptor.manifest.name
                    ));
                }
                ExtensionStartDecision::Allowed | ExtensionStartDecision::Disabled => {}
            }
        }
        if needs_host_authority {
            diagnostics.push(CONTROLLED_EXTENSION_START_DIAGNOSTIC.to_owned());
        }
        let host_state = host_state(session, model, reasoning, sessions);
        let has_enabled = descriptors
            .iter()
            .any(|descriptor| descriptor.activation.enabled);
        // The native-host protocol never starts extension processes, and
        // --no-process/--no-shell deny them independently of host authority.
        // Discovery remains available for diagnostics.
        if !config.start_extension_processes && has_enabled {
            diagnostics.push(NATIVE_HOST_EXTENSION_START_DIAGNOSTIC.to_owned());
        } else if !config.sandbox.process_execution_allowed() && has_enabled {
            diagnostics.push(
                "executable extensions were not started: process execution is disabled by --no-process/--no-shell".to_owned(),
            );
        }
        let execution_allowed =
            config.start_extension_processes && config.sandbox.process_execution_allowed();
        let startable = descriptors
            .iter()
            .filter(|descriptor| {
                execution_allowed
                    && descriptor
                        .activation
                        .start_decision(descriptor.source, config.workspace_trusted)
                        == ExtensionStartDecision::Allowed
            })
            .cloned()
            .collect::<Vec<_>>();

        // One wake/consumer binding for the complete interactive fleet. Plain,
        // print, RPC, and native hosts never advertise remote UI success.
        let remote_ui_wake = (matches!(&config.mode, Mode::Interactive)
            && crate::tui::terminal::TerminalCapabilities::detect(config.color, config.plain)
                .interactive)
            .then(|| {
                INTERACTIVE_REMOTE_UI_WAKE
                    .get_or_init(|| Arc::new(tokio::sync::Notify::new()))
                    .clone()
            });
        let event_bus = Arc::new(ExtensionEventBus::default());
        let (session_lifecycle_service, session_lifecycle_receiver) =
            if active_session_lifecycle_enabled(config)
                && startable.iter().any(extension_session_lifecycle_eligible)
            {
                let (service, receiver) =
                    ExtensionSessionLifecycleService::channel(SESSION_LIFECYCLE_QUEUE_CAPACITY)
                        .expect("fixed session lifecycle queue capacity is bounded");
                (Some(service.with_compaction()), Some(receiver))
            } else {
                (None, None)
            };

        // Discovery remains static. Catalog construction reads bounded source
        // identity only; the durable manager is the sole owner allowed to
        // activate a process after policy/trust gates have admitted it.
        // Apply execution policy to retained runtimes as well as new starts.
        // Keep the discovered activation above intact for actionable status;
        // catalog ineligibility retires even still-trusted workspace services.
        crate::app::bootstrap::startup_phase("extensions.digest.begin");
        let catalog = ExtensionRuntimeCatalog::from_descriptors(descriptors.iter().cloned().map(
            |mut descriptor| {
                descriptor.activation.enabled &= execution_allowed
                    && descriptor
                        .activation
                        .start_decision(descriptor.source, config.workspace_trusted)
                        == ExtensionStartDecision::Allowed;
                descriptor
            },
        ));
        crate::app::bootstrap::startup_phase("extensions.digest.ready");
        crate::app::bootstrap::startup_count(
            "extensions.digest.files",
            catalog.digest_work().files,
        );
        crate::app::bootstrap::startup_count(
            "extensions.digest.bytes",
            catalog.digest_work().bytes,
        );
        crate::app::bootstrap::startup_count(
            "extensions.digest.inactive",
            catalog.digest_work().inactive,
        );
        diagnostics.extend(catalog.diagnostics().iter().map(|diagnostic| {
            format!(
                "warning: extension {:?}: runtime catalog {}",
                diagnostic.extension, diagnostic.message
            )
        }));
        let mut managed_runtime = runtime_manager;
        let mut runtime_binding = None;
        let mut starts = Vec::<(String, Result<ExtensionProcess, String>)>::new();
        if !descriptors.is_empty() || managed_runtime.is_some() {
            if managed_runtime.is_none() {
                match ExtensionRuntimeDomain::ordinary(&config.workspace) {
                    Ok(domain) => managed_runtime = Some(ExtensionRuntimeManager::new(domain)),
                    Err(error) => diagnostics.push(format!(
                        "error: executable extension runtime domain could not be created: {error}"
                    )),
                }
            }
            if let Some(manager) = managed_runtime.clone() {
                let bulk_storage = match manager.bulk_storage() {
                    Ok(storage) => Some(storage),
                    Err(error) => {
                        diagnostics.push(format!(
                            "warning: extension bulk storage unavailable: {error}"
                        ));
                        None
                    }
                };
                let workspace = config.workspace.clone();
                let state = host_state.clone();
                let session_lifecycle_service = session_lifecycle_service.clone();
                let event_bus = event_bus.clone();
                let provider_registry = provider_runtime.registry();
                let subagents_tool_available =
                    model.spec.capabilities.tools && config.tool_available("subagent_spawn");
                let extension_flag_values = config.extension_flag_values.clone();
                let remote_ui_wake = remote_ui_wake.clone();
                let owner = session.resource_owner_key();
                let startable_names = startable
                    .iter()
                    .map(|descriptor| descriptor.manifest.name.clone())
                    .collect::<Vec<_>>();
                crate::app::bootstrap::startup_count(
                    "extensions.handshake.requested",
                    startable_names.len(),
                );
                crate::app::bootstrap::startup_phase("extensions.handshake.begin");
                let activation_result = block_on_runtime(async move {
                    manager.replace_catalog(catalog).await;
                    let binding = manager
                        .bind_session(owner)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    let starts = binding
                        .activate_eager(startable_names, |entry| {
                            let mut runtime = ExtensionRuntimeConfig::new(workspace.clone());
                            // Admission is supplied by the actual App consumer constructor,
                            // never inferred from Mode, an enabled factory or a manifest.
                            runtime.resource_paths = resource_consumer == crate::app::resource_paths::ResourceConsumerCapability::AppFrontend;
                            runtime.provider_pipeline = true;
                            runtime.host_state = state.clone();
                            runtime.remote_ui = remote_ui_wake.clone();
                            // Generic request-scoped composition is offered to
                            // API 0.4 extensions. Authority is attached later,
                            // only to live model-tool contexts, never commands.
                            runtime.tool_composition = true;
                            runtime.bulk_store = bulk_storage.clone();
                            runtime.flag_values = extension_flag_values
                                .get(&entry.descriptor.manifest.name)
                                .cloned()
                                .unwrap_or_default();
                            runtime.agent_sessions = entry.descriptor.manifest.name
                                == SUBAGENTS_EXTENSION_NAME
                                && subagents_tool_available;
                            runtime.session_lifecycle =
                                if extension_session_lifecycle_eligible(&entry.descriptor) {
                                    session_lifecycle_service.clone()
                                } else {
                                    None
                                };
                            if extension_session_lifecycle_eligible(&entry.descriptor) {
                                runtime.event_bus = Some(event_bus.clone());
                            }
                            runtime.provider_registry = Some(provider_registry.clone());
                            runtime
                        })
                        .await;
                    Ok::<_, anyhow::Error>((binding, starts))
                });
                crate::app::bootstrap::startup_phase("extensions.handshake.ready");
                match activation_result {
                    Ok(Ok((binding, activations))) => {
                        runtime_binding = Some(binding);
                        for activation in activations {
                            let name = activation.extension;
                            match (activation.outcome, activation.process) {
                                (ExtensionRuntimeActivationOutcome::Ready, Some(process)) => {
                                    starts.push((name, Ok(process)));
                                }
                                (outcome, _) => starts.push((
                                    name,
                                    Err(match outcome {
                                        ExtensionRuntimeActivationOutcome::Inactive => {
                                            "extension is not eligible for activation".to_owned()
                                        }
                                        ExtensionRuntimeActivationOutcome::StaleSource => {
                                            "extension source changed before activation".to_owned()
                                        }
                                        ExtensionRuntimeActivationOutcome::ResourceExhausted(error) => {
                                            error.to_string()
                                        }
                                        ExtensionRuntimeActivationOutcome::Failed(failure) => {
                                            format!("runtime startup {failure:?}")
                                        }
                                        ExtensionRuntimeActivationOutcome::Ready => {
                                            "runtime activation returned no process".to_owned()
                                        }
                                    }),
                                )),
                            }
                        }
                    }
                    Ok(Err(error)) => diagnostics.push(format!(
                        "error: executable extensions could not bind to the runtime manager: {error}"
                    )),
                    Err(error) => diagnostics.push(format!(
                        "error: executable extensions could not start: {error}"
                    )),
                }
            }
        }

        let mut processes = Vec::new();
        let mut receivers = Vec::new();
        let mut running = BTreeSet::new();
        let mut start_failures = BTreeMap::new();
        for (name, start) in starts {
            match start {
                Ok(process) => {
                    receivers.push(process.subscribe());
                    running.insert(name);
                    processes.push(process);
                }
                Err(error) => {
                    let error = error.to_string();
                    diagnostics.push(format!("error: extension {name:?} launch failed: {error}"));
                    start_failures.insert(name, clip_lifecycle_reason(&error, 4 * 1024));
                }
            }
        }

        // API 0.3 provider declarations are reverse RPCs intentionally sent
        // after initialize. Observe their bounded startup barrier before the
        // caller projects models from the registry.
        provider_runtime.await_initial_registrations(&processes);

        // Register tool catalogs before observers/hooks so all live processes
        // have a host-owned dynamic group. A compatible shared process was
        // detached from the previous App binding before this point.
        for process in &processes {
            process.register_dynamic_tool_catalog(host);
        }
        for process in &processes {
            host.load(process);
        }

        let (shortcuts, shortcut_diagnostics) = register_extension_shortcuts(&processes);
        diagnostics.extend(shortcut_diagnostics);

        let runtime_statuses = managed_runtime
            .as_ref()
            .map(ExtensionRuntimeManager::statuses)
            .unwrap_or_default()
            .into_iter()
            .map(|status| (status.provenance.extension.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let processes_by_name = processes
            .iter()
            .map(|process| (process.descriptor().manifest.name.as_str(), process))
            .collect::<BTreeMap<_, _>>();
        let summaries = descriptors
            .into_iter()
            .map(|descriptor| {
                let process = processes_by_name
                    .get(descriptor.manifest.name.as_str())
                    .copied();
                let contributions = process.map(ExtensionProcess::contributions);
                let health = process.map(ExtensionProcess::health_snapshot).or_else(|| {
                    start_failures.get(&descriptor.manifest.name).map(|error| {
                        ExtensionHealthSnapshot {
                            state: ExtensionHealthState::Parked,
                            generation: 0,
                            pending_requests: 0,
                            last_error: Some(error.clone()),
                        }
                    })
                });
                let negotiated_features: Vec<String> = process
                    .map(|process| process.negotiated_features().iter().cloned().collect())
                    .unwrap_or_default();
                let (telemetry_schema, compatibility) = extension_compatibility(
                    &descriptor.manifest.name,
                    running.contains(&descriptor.manifest.name),
                    &negotiated_features,
                    health.as_ref(),
                );
                let manifest_path = descriptor.manifest_path;
                let manifest_digest = sha256_manifest(&manifest_path);
                let bundle_digest = installed_bundle_digest(&manifest_path);
                ExtensionSummary {
                    name: descriptor.manifest.name.clone(),
                    version: descriptor.manifest.version,
                    manifest_path,
                    manifest_digest,
                    bundle_digest,
                    source: descriptor.source,
                    enabled: descriptor.activation.enabled,
                    trusted: descriptor.activation.trust == ExtensionTrust::Trusted,
                    running: running.contains(&descriptor.manifest.name),
                    api_version: process
                        .map(|process| process.api_version().to_owned())
                        .unwrap_or_else(|| descriptor.manifest.api_version.clone()),
                    negotiated_features,
                    telemetry_schema,
                    compatibility,
                    health,
                    runtime: runtime_statuses.get(&descriptor.manifest.name).cloned(),
                    tools: contributions
                        .map(|value| value.tools.iter().map(|tool| tool.name.clone()).collect())
                        .unwrap_or_else(|| descriptor.manifest.contributes.tools.clone()),
                    commands: contributions
                        .map(|value| {
                            value
                                .commands
                                .iter()
                                .map(|command| command.name.clone())
                                .collect()
                        })
                        .unwrap_or_else(|| descriptor.manifest.contributes.commands.clone()),
                    hooks: descriptor.manifest.contributes.hooks,
                    ui: descriptor.manifest.contributes.ui,
                    // Live declarations are overlaid by `summaries()` from the
                    // shared registry; discovery alone has no provider state.
                    providers: Vec::new(),
                }
            })
            .collect();

        let mut extensions = Self::default();
        extensions.processes = processes;
        extensions.provider_runtime = provider_runtime;
        extensions.runtime_manager = managed_runtime;
        extensions.runtime_binding = runtime_binding;
        extensions.receivers = receivers;
        extensions.shortcuts = shortcuts;
        extensions.summaries = summaries;
        extensions.diagnostics.extend(diagnostics);
        extensions.event_bus = Some(event_bus);
        extensions.remote_ui_wake = remote_ui_wake;
        extensions.session_lifecycle_service = session_lifecycle_service;
        extensions.session_lifecycle_receiver = session_lifecycle_receiver;
        extensions.session_id = host_state.session_id.clone();
        extensions.host_state = Mutex::new(host_state);
        extensions.workspace = config.workspace.clone();
        extensions.resource_owner = Some(session.resource_owner_key());
        extensions.rescan_config = Some(config.clone());
        extensions.rescan_global_config = crate::cli::global_config_path();
        extensions.effect_policy = config.effect_policy;
        extensions.start_policy_supervisors();
        // Install actual native history before either immediate or deferred
        // session_start callbacks. Initialize's coarse model/skill projection
        // deliberately does not expose a namespace-independent session mirror.
        extensions.refresh_host_state(session, model, reasoning, sessions);
        extensions.start_session_lifecycle();
        extensions
    }

    /// Returns the durable process-fleet owner, if discovery created one.
    ///
    /// App rebuilds retain this exact manager and only replace their session
    /// binding, which allows a compatible workspace service to survive without
    /// transferring process ownership to a session object.
    pub fn runtime_manager(&self) -> Option<ExtensionRuntimeManager> {
        self.runtime_manager.clone()
    }

    /// Returns the shared product provider runtime retained across App rebuilds.
    pub(crate) fn provider_runtime(&self) -> ExtensionProviderRuntime {
        self.provider_runtime.clone()
    }

    /// Projects the current registry snapshot into this App's local catalog.
    ///
    /// Callers own the catalog/client mutation boundary; executable extensions
    /// retain only declarations and process handles.
    pub(crate) fn synchronize_provider_catalog(
        &mut self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
    ) -> Vec<String> {
        let diagnostics = self
            .provider_runtime
            .synchronize(catalog, client, &self.processes);
        self.diagnostics.extend(diagnostics.clone());
        diagnostics
    }

    pub(crate) fn synchronize_provider_catalog_report(
        &mut self,
        catalog: &mut ModelCatalog,
        client: &AiClient,
    ) -> ProviderCatalogReport {
        let report = self
            .provider_runtime
            .synchronize_report(catalog, client, &self.processes);
        if report.checked {
            self.diagnostics.extend(report.problems.iter().cloned());
        }
        report
    }

    /// Withdraws this host's provider projection before its processes stop.
    pub(crate) fn clear_provider_catalog(&mut self, catalog: &mut ModelCatalog, client: &AiClient) {
        self.provider_runtime.clear(catalog, client);
    }

    /// Enables requests only after an interactive application has a safely
    /// bound active session. Startup and rebuild leave the queue inactive.
    pub fn activate_session_lifecycle_driver(&self) {
        if let Some(service) = &self.session_lifecycle_service {
            service.activate();
        }
    }

    /// Fences queued work before shutdown or replacement of the owning app.
    pub fn deactivate_session_lifecycle_driver(&self) {
        if let Some(service) = &self.session_lifecycle_service {
            service.deactivate();
        }
    }

    /// Takes one current active-session request at an interactive idle boundary.
    pub fn next_session_lifecycle_request(&mut self) -> Option<ExtensionSessionLifecycleRequest> {
        self.session_lifecycle_receiver
            .as_mut()
            .and_then(ExtensionSessionLifecycleReceiver::try_next)
    }

    pub fn bind_agent_sessions(&self, agent: &Agent) -> anyhow::Result<usize> {
        let mut bound = 0;
        for process in &self.processes {
            if agent
                .bind_extension_agent_sessions(process)
                .with_context(|| {
                    format!(
                        "could not bind agent_sessions for extension {:?}",
                        process.descriptor().manifest.name
                    )
                })?
            {
                bound += 1;
            }
        }
        Ok(bound)
    }

    pub fn has_dynamic_tool_provider(&self) -> bool {
        self.processes
            .iter()
            .any(|process| process.supports_feature(EXTENSION_FEATURE_DYNAMIC_TOOLS))
    }

    pub(super) fn start_policy_supervisors(&mut self) {
        let Ok(handle) = Handle::try_current() else {
            if !self.processes.is_empty() {
                self.diagnostics
                    .push("warning: extension policy supervision requires the octet Tokio runtime");
            }
            return;
        };
        // A reload replaces the subscriptions, not the policy authority. Never
        // leave duplicate responders attached to a retained process.
        for task in self.policy_supervisors.drain(..) {
            task.abort();
        }
        let effect_policy = self.effect_policy;
        self.policy_supervisors
            .extend(self.processes.iter().cloned().map(|process| {
                let mut events = process.subscribe();
                handle.spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(ExtensionEvent::PolicyEvaluationRequested {
                                request_id,
                                generation,
                                parent_request_id,
                                intent,
                            }) => {
                                let response = mcp_policy_response(
                                    effect_policy,
                                    &process,
                                    generation,
                                    parent_request_id,
                                    &intent,
                                );
                                let _ = process
                                    .respond_to_policy_evaluation(request_id, generation, response)
                                    .await;
                            }
                            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                })
            }));
    }

    pub(super) fn start_session_lifecycle(&mut self) {
        if self.processes.is_empty() || self.session_lifecycle_started {
            return;
        }
        let processes = self.processes.clone();
        if let Some(session_id) = self.session_id.clone() {
            let event = ExtensionLifecycleEvent::SessionStarted {
                session_id,
                run_id: None,
            };
            match block_on_runtime(async move { notify_lifecycle_all(&processes, event).await }) {
                Ok(messages) => self.diagnostics.extend(messages),
                Err(error) => self.diagnostics.push(format!(
                    "warning: extension session lifecycle could not start: {error}"
                )),
            }
        }
        if let Some(resource_owner) = self.resource_owner.clone() {
            let (deferred_processes, processes): (Vec<_>, Vec<_>) =
                self.processes.iter().cloned().partition(|process| {
                    // Resource startup must run under the frontend's event pump,
                    // even without remote UI. It can await durable session work
                    // (or a real headless refusal) before discovery is admitted.
                    (process.supports_feature(
                        octet_agent::extension_process::EXTENSION_FEATURE_RESOURCE_PATHS,
                    ) && process
                        .contributions()
                        .hooks
                        .contains(&ExtensionHook::ResourcesDiscover))
                        || (self.remote_ui_wake.is_some()
                            && process.supports_feature(EXTENSION_FEATURE_REMOTE_UI))
                });
            self.pending_session_hook_starts.extend(
                deferred_processes
                    .into_iter()
                    .filter(|process| process.declares_session_hooks())
                    .map(|process| (process, resource_owner.clone())),
            );
            match block_on_runtime(async move {
                start_session_hooks_all(&processes, &resource_owner).await
            }) {
                Ok(messages) => self.diagnostics.extend(messages),
                Err(error) => self.diagnostics.push(format!(
                    "warning: extension session hooks could not start: {error}"
                )),
            }
        }
        self.session_lifecycle_started = true;
        self.session_started_at = Instant::now();
    }

    pub(crate) fn set_telemetry(&mut self, telemetry: Option<octet_agent::TelemetryObserver>) {
        self.telemetry = telemetry;
    }

    /// Status is memory-only. Actual rejected records are new loss events, not
    /// repeatable configuration problems, and never affect session accounting.
    pub(super) fn poll_telemetry(&mut self) {
        let Some(observer) = &self.telemetry else {
            return;
        };
        let status = observer.status();
        if status.rejected_records > self.telemetry_rejected {
            crate::output::stderr!(
                "warning: optional telemetry lost {} record(s); session accounting is unaffected",
                status.rejected_records - self.telemetry_rejected
            );
            self.telemetry_rejected = status.rejected_records;
        }
        if status.write_error != self.telemetry_error {
            if let Some(error) = status.write_error {
                crate::output::stderr!("warning: optional telemetry writer failed: {error:?}");
            }
            self.telemetry_error = status.write_error;
        }
    }

    pub(super) async fn shutdown_telemetry(&mut self) {
        self.poll_telemetry();
        let Some(observer) = self.telemetry.take() else {
            return;
        };
        // Neither disk waits nor bounded writer joins belong on the async
        // control owner. Await the explicit deadline before normal exit/rebuild.
        if let Err(error) =
            tokio::task::spawn_blocking(move || shutdown_telemetry_observer(observer)).await
        {
            crate::output::stderr!("warning: optional telemetry shutdown worker failed: {error}");
        }
    }

    pub(super) fn cancel_session_hook_starts(&mut self) {
        self.pending_session_hook_starts.clear();
        for task in self.session_hook_start_tasks.drain(..) {
            task.abort();
        }
    }

    pub(super) fn schedule_session_hook_starts(&mut self) {
        self.session_hook_start_tasks
            .retain(|task| !task.is_finished());
        for (process, owner) in self.pending_session_hook_starts.drain(..) {
            let tx = self.background_tx.clone();
            self.session_hook_start_tasks.push(tokio::spawn(async move {
                // The process owns its bounded request deadline. The shell is
                // free to service ui/open while this hook awaits its result.
                if let Err(error) = process.start_session_hook_binding(owner).await {
                    let _ = tx
                        .send(ExtensionBackgroundUpdate::Diagnostics(vec![format!(
                            "warning: extension {:?} session_start hook failed: {error}",
                            process.descriptor().manifest.name,
                        )]))
                        .await;
                }
            }));
        }
    }

    pub(super) fn cancel_background_work(&mut self) {
        self.cancel_session_hook_starts();
        for task in self.renderer_tasks.drain(..) {
            task.abort();
        }
        for task in self.autocomplete_tasks.drain(..) {
            task.abort();
        }
        for task in self.shortcut_tasks.drain(..) {
            task.abort();
        }
        for task in self.confirmation_tasks.drain(..) {
            task.abort();
        }
        for task in self.input_tasks.drain(..) {
            task.abort();
        }
        for task in self.policy_supervisors.drain(..) {
            task.abort();
        }
        self.confirmation_denials.clear();
        self.input_cancellations.clear();
        while self.background_rx.try_recv().is_ok() {}
    }

    pub(super) async fn settle_session_lifecycle(&mut self) {
        if self.session_lifecycle_started {
            let outcome = self
                .last_lifecycle_outcome
                .unwrap_or(ExtensionLifecycleOutcome::Shutdown);
            if let Some(resource_owner) = self.resource_owner.clone() {
                let diagnostics =
                    settle_session_hooks_all(&self.processes, &resource_owner, outcome).await;
                self.diagnostics.extend(diagnostics);
            }
            if let Some(session_id) = self.session_id.clone() {
                let diagnostics = notify_lifecycle_all(
                    &self.processes,
                    ExtensionLifecycleEvent::SessionSettled {
                        session_id,
                        run_id: None,
                        outcome,
                        duration_ms: duration_millis(self.session_started_at.elapsed()),
                        reason: None,
                    },
                )
                .await;
                self.diagnostics.extend(diagnostics);
            }
            self.session_lifecycle_started = false;
        }
    }

    pub(super) fn retire_active_resources(&self) {
        self.resource_paths_live
            .store(false, std::sync::atomic::Ordering::Release);
        // Authority ends before fallible observational hooks or native cleanup.
        if let Some(owner) = &self.resource_owner {
            for process in &self.processes {
                process.retire_resource_owner(owner);
            }
        }
    }

    /// Releases this App/session's attachment to the durable process fleet.
    ///
    /// Isolated profiles are stopped. Explicitly shared workspace services are
    /// deliberately left with the runtime manager so a compatible replacement
    /// App can bind them without a stop/restart gap. Interactive callers must
    /// revoke the terminal grant with their shell before releasing this owner.
    pub async fn release_binding(&mut self) {
        self.retire_active_resources();
        // A replacement App must not inherit work queued against the old
        // owner. Isolated lifecycle processes are stopped below; shared and
        // legacy processes never receive this service.
        self.deactivate_session_lifecycle_driver();
        self.remote_ui
            .revoke("the foreground extension binding ended");
        self.cancel_background_work();
        self.settle_session_lifecycle().await;
        for process in &self.processes {
            process.detach_dynamic_tool_catalog();
        }
        if let Some(binding) = self.runtime_binding.take() {
            // `ExtensionProcess::shutdown` is individually bounded. Do not
            // cancel binding release midway: a dropped release future has
            // already closed the binding and must finish detaching its keys.
            binding.release().await;
        } else {
            let processes = self.processes.clone();
            let shutdowns =
                futures_util::future::join_all(processes.iter().map(ExtensionProcess::shutdown));
            let _ = tokio::time::timeout(SHUTDOWN_DEADLINE, shutdowns).await;
        }
        self.processes.clear();
        self.receivers.clear();
        for summary in &mut self.summaries {
            summary.running = false;
        }
        self.shutdown_telemetry().await;
    }

    /// Synchronous App rebuild boundary that preserves the durable manager.
    pub fn release_binding_blocking(&mut self) {
        let _ = block_on_runtime(self.release_binding());
    }

    /// Gracefully stops every runtime owned by this host after releasing the
    /// current session binding. Each protocol shutdown has its own hard timeout
    /// in `ExtensionProcess`; the outer timeout prevents terminal restoration
    /// from being delayed indefinitely.
    pub async fn shutdown(&mut self) {
        self.release_binding().await;
        if let Some(manager) = self.runtime_manager.take() {
            let _ = tokio::time::timeout(SHUTDOWN_DEADLINE, manager.shutdown()).await;
        }
    }

    /// Synchronous terminal shutdown boundary.
    pub fn shutdown_blocking(&mut self) {
        let _ = block_on_runtime(self.shutdown());
    }

    // Compatibility convenience for in-crate lifecycle tests; production
    // callers retain typed reload outcomes and report problems separately.
    // Every caller drives real extension processes, so this exists on unix
    // test builds only.
    #[cfg(all(test, unix))]
    pub async fn reload(&mut self) -> Vec<String> {
        self.reload_report().await.into_notices()
    }

    pub(crate) async fn reload_report(&mut self) -> ExtensionReloadReport {
        let results = self.prepare_resource_process_reload().await;
        self.finish_resource_process_reload(results).await
    }

    /// Borrow-free replacement: the existing terminal owner must pump reverse
    /// UI while the new generation's session_start is awaited inside reload.
    pub(crate) fn prepare_resource_process_reload(
        &mut self,
    ) -> impl Future<
        Output = Vec<(
            String,
            Result<octet_agent::extension_process::ExtensionReloadReport, String>,
        )>,
    > + Send
    + 'static {
        self.resource_paths_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.cancel_background_work();
        let manager = self.runtime_manager.clone();
        let processes = self.processes.clone();
        async move {
            if let Some(manager) = manager {
                let names = processes
                    .iter()
                    .map(|process| process.descriptor().manifest.name.clone())
                    .collect::<BTreeSet<_>>();
                futures_util::future::join_all(names.into_iter().map(|name| {
                    let manager = manager.clone();
                    async move { (name.clone(), manager.reload(&name).await) }
                }))
                .await
                .into_iter()
                .flat_map(|(name, results)| {
                    results.into_iter().map(move |result| {
                        (name.clone(), result.map_err(|error| error.to_string()))
                    })
                })
                .collect::<Vec<_>>()
            } else {
                let reloads = processes.into_iter().map(|process| async move {
                    let name = process.descriptor().manifest.name.clone();
                    (
                        name,
                        process.reload().await.map_err(|error| error.to_string()),
                    )
                });
                // Concurrent polling preserves input order without serializing
                // unrelated extension reloads behind a hung child.
                futures_util::future::join_all(reloads).await
            }
        }
    }

    pub(crate) async fn finish_resource_process_reload(
        &mut self,
        results: Vec<(
            String,
            Result<octet_agent::extension_process::ExtensionReloadReport, String>,
        )>,
    ) -> ExtensionReloadReport {
        let mut output = ExtensionReloadReport::default();
        let mut reloaded = BTreeSet::new();
        let mut completed_mutations = Vec::new();
        for (name, result) in results {
            match result {
                Ok(report) => {
                    reloaded.insert(name.clone());
                    output.processes.push((
                        name.clone(),
                        Ok(format!(
                            "reloaded {name} (generation {}, previous shutdown {})",
                            report.generation,
                            if report.previous_shutdown_graceful {
                                "clean"
                            } else {
                                "forced"
                            }
                        )),
                    ));
                    let resource = opaque_extension_resource_id(&name);
                    let mutation_id = format!("resource-reload:{resource}:{}", report.generation);
                    if let Some(mutation) = PostMutationContext::new(
                        mutation_id,
                        PostMutationKind::Resource,
                        vec![resource],
                        report.generation,
                        PostMutationState::Committed,
                    ) {
                        completed_mutations.push(mutation);
                    }
                }
                Err(error) => output.processes.push((
                    name.clone(),
                    Err(format!("unable to reload {name}: {error}")),
                )),
            }
        }
        self.await_reloaded_provider_registrations(&reloaded).await;
        for mutation in completed_mutations {
            let rescans = self.notify_post_mutation(mutation).await;
            output.details.extend(rescans.into_iter().map(|request| {
                format!(
                    "extension {:?} requested bounded rescan of {} resource(s)",
                    request.extension,
                    request.resource_ids.len()
                )
            }));
        }
        self.start_policy_supervisors();
        let (shortcuts, diagnostics) = register_extension_shortcuts(&self.processes);
        self.shortcuts = shortcuts;
        output.shortcuts = diagnostics;
        output.events = self.drain_events();
        output.events.extend(self.discard_stale_host_requests());
        // A generation replacement is a product resource mutation. Drain the
        // bounded rescan queue it just admitted so an admitted hook cannot leave
        // resolver work queued forever. Re-resolution reuses the same trusted
        // discovery path; a stale or unavailable owner is dropped with a
        // diagnostic and a changed source is never activated implicitly.
        output.rescans = self.drain_post_mutation_report();
        output
    }

    /// Waits for post-initialize provider declarations from just-reloaded owners
    /// before callers can reconcile their host-owned model routes.
    pub(super) async fn await_reloaded_provider_registrations(&self, reloaded: &BTreeSet<String>) {
        if reloaded.is_empty() {
            return;
        }
        let processes = self
            .processes
            .iter()
            .filter(|process| reloaded.contains(&process.descriptor().manifest.name))
            .cloned()
            .collect::<Vec<_>>();
        self.provider_runtime
            .await_initial_registrations_async(&processes)
            .await;
    }
}
