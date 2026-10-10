//! ExecutableExtensions discovery, start, reload, shutdown and telemetry.

use super::*;

#[cfg(all(test, unix))]
#[path = "lifecycle_exit_tests.rs"]
mod lifecycle_exit_tests;

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
            None,
        )
    }

    /// Discovers and starts extensions using one product-owned provider runtime.
    ///
    /// Bootstrap and rebuild pass the same value through this seam so API 0.3
    /// declarations keep their process-owner fences while the App is replaced.
    #[cfg(all(test, unix))]
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
        remote_ui_consumer: Option<&crate::tui::view::InteractiveShell>,
    ) -> Self {
        Self::discover_and_start_with_provider_runtime_and_reason(
            config,
            session,
            model,
            reasoning,
            sessions,
            host,
            runtime_manager,
            provider_runtime,
            resource_consumer,
            remote_ui_consumer,
            "startup",
            ExtensionStartupTiming::Synchronous,
        )
    }

    /// Discovers and starts extensions using one product-owned provider runtime.
    ///
    /// Bootstrap and rebuild pass the same value through this seam so API 0.3
    /// declarations keep their process-owner fences while the App is replaced.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn discover_and_start_with_provider_runtime_and_reason(
        config: &Config,
        session: &Session,
        model: &Model,
        reasoning: &ReasoningConfig,
        sessions: &SessionStore,
        host: &mut ExtensionHost,
        runtime_manager: Option<ExtensionRuntimeManager>,
        provider_runtime: ExtensionProviderRuntime,
        resource_consumer: crate::app::resource_paths::ResourceConsumerCapability,
        remote_ui_consumer: Option<&crate::tui::view::InteractiveShell>,
        session_start_reason: &'static str,
        extension_startup_timing: ExtensionStartupTiming,
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
        let mut host_state = host_state(session, model, reasoning, sessions);
        host_state.project_trusted = Some(config.workspace_trusted);
        host_state.mode = Some(
            match config.mode {
                Mode::Interactive => "tui",
                Mode::Print { .. } => "print",
                Mode::Rpc => "rpc",
            }
            .into(),
        );
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
        // A tool-name clash turns the external extension off instead of
        // failing startup; the first-party extension keeps the name.
        let (startable, tool_clashes) = turn_off_tool_clashes(startable);
        for (name, reason) in &tool_clashes {
            diagnostics.push(format!(
                "warning: extension {name:?} was turned off: {reason}"
            ));
        }

        // Bind before initialize. An already constructed native shell is the
        // consumer even when the calling process's stdout is not a TTY (an
        // embedded frontend or the real-shell acceptance fixture). Without a
        // supplied shell, ordinary terminal bootstrap keeps its existing gate.
        // A supplied inactive shell cannot fall back to an unrelated stdout.
        let remote_ui_wake = if matches!(&config.mode, Mode::Interactive) {
            match remote_ui_consumer {
                Some(shell) => shell.extension_remote_ui_binding(),
                None => {
                    crate::tui::terminal::TerminalCapabilities::detect(config.color, config.plain)
                        .interactive
                        .then(|| {
                            INTERACTIVE_REMOTE_UI_WAKE
                                .get_or_init(|| Arc::new(tokio::sync::Notify::new()))
                                .clone()
                        })
                }
            }
        } else {
            None
        };
        host_state.has_ui = Some(remote_ui_wake.is_some());
        if remote_ui_wake.is_some() {
            host_state.keybindings = Some(remote_ui_consumer.map_or_else(
                || {
                    crate::tui::keymap::keybindings::KeybindingsManager::for_user()
                        .get_resolved_bindings()
                },
                crate::tui::view::InteractiveShell::extension_keybindings,
            ));
        }
        let event_bus = Arc::new(ExtensionEventBus::default());
        let (session_lifecycle_service, session_lifecycle_receiver) =
            if active_session_lifecycle_enabled(config)
                && startable.iter().any(extension_session_lifecycle_eligible)
            {
                let (service, receiver) =
                    ExtensionSessionLifecycleService::channel(SESSION_LIFECYCLE_QUEUE_CAPACITY)
                        .expect("fixed session lifecycle queue capacity is bounded");
                (
                    Some(service.with_compaction().with_model_control()),
                    Some(receiver),
                )
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
                        == ExtensionStartDecision::Allowed
                    && !tool_clashes.contains_key(&descriptor.manifest.name);
                descriptor
            },
        ));
        crate::app::bootstrap::startup_phase("extensions.digest.ready");
        crate::app::bootstrap::startup_phase("extensions.discover.ready");
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
        // Every discovered descriptor, keyed by name. The status summaries of a
        // deferred fleet are rebuilt from this map on every attach, so disabled
        // and failed extensions keep their discovery-time entries.
        let descriptors_by_name = descriptors
            .iter()
            .map(|descriptor| (descriptor.manifest.name.clone(), descriptor.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut managed_runtime = runtime_manager;
        let mut runtime_binding = None;
        let mut starts = Vec::<(String, Result<ExtensionProcess, String>)>::new();
        let mut deferred_startup = None;
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
                // Configuration only: deriving the authoritative private home
                // performs no provisioning, filesystem creation or download.
                // Explicit action-scoped consent is never retained by the fleet.
                let python_runtime = match super::status::private_python_runtime_config(None) {
                    Ok(runtime) => Some(runtime),
                    Err(error) => {
                        diagnostics.push(format!(
                            "warning: private Python runtime location unavailable: {error}"
                        ));
                        None
                    }
                };
                let agent_sessions_tool_available =
                    model.spec.capabilities.tools && config.tool_available("subagent_spawn");
                let activation_plan = ExtensionActivationPlan {
                    workspace: config.workspace.clone(),
                    python_runtime,
                    // Admission is supplied by the actual App consumer constructor,
                    // never inferred from Mode, an enabled factory or a manifest.
                    resource_paths: resource_consumer
                        == crate::app::resource_paths::ResourceConsumerCapability::AppFrontend,
                    host_state: host_state.clone(),
                    remote_ui: remote_ui_wake.clone(),
                    bulk_store: bulk_storage,
                    flag_values: config.extension_flag_values.clone(),
                    agent_sessions_tool_available,
                    session_lifecycle: session_lifecycle_service.clone(),
                    event_bus: Some(event_bus.clone()),
                    provider_registry: Some(provider_runtime.registry()),
                };
                let startable_names = startable
                    .iter()
                    .map(|descriptor| descriptor.manifest.name.clone())
                    .collect::<Vec<_>>();
                // A pending first-party subagents extension is the same one the
                // synchronous path would have measured, so delegation readiness
                // is decided at discovery and the process binds at attach.
                let agent_sessions_pending = agent_sessions_tool_available
                    && startable_names
                        .iter()
                        .any(|name| name == SUBAGENTS_EXTENSION_NAME);
                crate::app::bootstrap::startup_count(
                    "extensions.handshake.requested",
                    startable_names.len(),
                );
                crate::app::bootstrap::startup_phase("extensions.handshake.begin");
                let owner = session.resource_owner_key();
                match extension_startup_timing {
                    ExtensionStartupTiming::Synchronous => {
                        let activation_result = block_on_runtime(async move {
                            manager.replace_catalog(catalog).await;
                            let binding = manager
                                .bind_session(owner)
                                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                            let starts = binding
                                .activate_eager(startable_names, |entry| {
                                    activation_plan.runtime_for(&entry.descriptor)
                                })
                                .await;
                            Ok::<_, anyhow::Error>((binding, starts))
                        });
                        crate::app::bootstrap::startup_phase("extensions.handshake.ready");
                        match activation_result {
                            Ok(Ok((binding, activations))) => {
                                runtime_binding = Some(binding);
                                for activation in activations {
                                    starts.push(activation_start(activation));
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
                    ExtensionStartupTiming::AfterFirstFrame => {
                        // Flattened because the async block already returns a
                        // `Result`: one binding error, one failure report.
                        let bound = block_on_runtime(async move {
                            manager.replace_catalog(catalog).await;
                            let binding = manager
                                .bind_session(owner)
                                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                            Ok::<_, anyhow::Error>(binding)
                        })
                        .and_then(|bound| bound);
                        match bound {
                            Ok(binding) => {
                                runtime_binding = Some(binding.clone());
                                deferred_startup = Some(spawn_deferred_startup(
                                    binding,
                                    startable,
                                    &descriptors_by_name,
                                    &activation_plan,
                                    tool_clashes.clone(),
                                    agent_sessions_pending,
                                ));
                            }
                            Err(error) => diagnostics.push(format!(
                                "error: executable extensions could not bind to the runtime manager: {error}"
                            )),
                        }
                    }
                }
            }
        }

        let mut extensions = Self::default();
        extensions.provider_runtime = provider_runtime;
        extensions.runtime_manager = managed_runtime;
        extensions.runtime_binding = runtime_binding;
        extensions.diagnostics.extend(diagnostics);
        extensions.event_bus = Some(event_bus);
        extensions.remote_ui_wake = remote_ui_wake;
        extensions.session_lifecycle_service = session_lifecycle_service;
        extensions.session_lifecycle_receiver = session_lifecycle_receiver;
        extensions.session_id = host_state.session_id.clone();
        extensions.session_start_reason = session_start_reason;
        extensions.host_state = Mutex::new(host_state);
        extensions.workspace = config.workspace.clone();
        extensions.resource_owner = Some(session.resource_owner_key());
        extensions.rescan_config = Some(config.clone());
        extensions.rescan_global_config = crate::cli::global_config_path();
        extensions.effect_policy = config.effect_policy;
        // One install path for boot and late attach. A deferred fleet installs
        // an empty batch here (every extension keeps its discovered summary)
        // and installs each settled handshake through the same method.
        if let Err(error) = block_on_runtime(extensions.install_activations(
            host,
            &descriptors_by_name,
            starts,
            &tool_clashes,
        )) {
            extensions.diagnostics.push(format!(
                "error: executable extension activation could not be installed: {error}"
            ));
        }
        extensions.deferred_startup = deferred_startup;
        extensions.start_policy_supervisors();
        // Install actual native history before either immediate or deferred
        // session_start callbacks. Initialize's coarse model/skill projection
        // deliberately does not expose a namespace-independent session mirror.
        extensions.refresh_host_state(session, model, reasoning, sessions);
        extensions.start_session_lifecycle();
        extensions.bind_tool_host(host);
        crate::app::bootstrap::startup_phase("extensions.installed");
        extensions
    }

    /// Whether any process handshake is still owed after the first frame.
    pub(crate) fn startup_pending(&self) -> bool {
        self.deferred_startup
            .as_ref()
            .is_some_and(|deferred| !deferred.pending.is_empty())
    }

    /// Names of the extensions whose handshake is still in flight.
    pub(crate) fn pending_startup_names(&self) -> Vec<String> {
        self.deferred_startup
            .as_ref()
            .map(|deferred| deferred.pending.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Names of the still-starting extensions that own an awaited prompt hook.
    ///
    /// This is exactly the set a submission waits for; every other pending
    /// extension attaches in the background.
    pub(crate) fn pending_prompt_hook_names(&self) -> Vec<String> {
        self.deferred_startup
            .as_ref()
            .map(|deferred| deferred.pending_prompt_hooks.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether a pending first-party `octet-subagents` was admitted with the
    /// `subagent_spawn` tool. Delegation readiness uses this until the process
    /// attaches, so a deferred boot cannot silently downgrade Ultra reasoning.
    pub(crate) fn pending_agent_session_service(&self) -> bool {
        self.deferred_startup
            .as_ref()
            .is_some_and(|deferred| deferred.agent_sessions_pending)
    }

    /// Installs every extension handshake that already settled.
    ///
    /// `host` is the host the running Agent consults, so a late attach reaches
    /// its observers, session-operation hooks, and dynamic tool registry. The
    /// pump never awaits a handshake: it drains what has landed and returns, so
    /// it is safe at any interactive boundary, including before a submission.
    pub(crate) async fn pump_deferred_startup(
        &mut self,
        host: &mut octet_agent::ExtensionHost,
    ) -> DeferredStartupProgress {
        let Some(mut deferred) = self.deferred_startup.take() else {
            return DeferredStartupProgress::default();
        };
        let starts = drain_deferred_activations(&mut deferred);
        let mut progress = DeferredStartupProgress::default();
        if !starts.is_empty() {
            let installed = self
                .install_activations(host, &deferred.descriptors, starts, &deferred.tool_clashes)
                .await;
            progress.attached = installed.attached;
            progress.notices = installed.notices;
            progress.changed = installed.changed;
            crate::app::bootstrap::startup_phase("extensions.attached");
            crate::app::bootstrap::startup_count(
                "extensions.attached.processes",
                progress.attached.len(),
            );
        }
        if deferred.pending.is_empty() {
            // The deferred state has served its purpose: the fleet is complete
            // and a settled startup must not keep presenting as in progress.
            finish_deferred_startup(&mut deferred);
        } else {
            self.deferred_startup = Some(deferred);
        }
        progress
    }

    /// Waits until every pending extension that declares the native
    /// `before_prompt` hook has settled.
    ///
    /// Correctness is per hook: an extension that registered `before_prompt`
    /// must see a submitted prompt even while it is still loading, while an
    /// unrelated extension is never waited for. The wait is bounded because the
    /// runtime manager already bounds each activation with its startup budget;
    /// when that budget expires the failure is reported, the remaining
    /// extensions stay deferred for the ordinary pump, and the submission
    /// proceeds with the hooks that did attach.
    pub(crate) async fn await_pending_prompt_hooks(
        &mut self,
        host: &mut octet_agent::ExtensionHost,
    ) -> anyhow::Result<()> {
        const PROMPT_HOOK_WAIT: Duration = Duration::from_secs(30);
        let deadline = tokio::time::Instant::now() + PROMPT_HOOK_WAIT;
        loop {
            let Some(mut deferred) = self.deferred_startup.take() else {
                return Ok(());
            };
            let starts = drain_deferred_activations(&mut deferred);
            if !starts.is_empty() {
                self.install_activations(
                    host,
                    &deferred.descriptors,
                    starts,
                    &deferred.tool_clashes,
                )
                .await;
            }
            if deferred.pending_prompt_hooks.is_empty() {
                if deferred.pending.is_empty() {
                    finish_deferred_startup(&mut deferred);
                } else {
                    self.deferred_startup = Some(deferred);
                }
                return Ok(());
            }
            let waited = tokio::time::timeout_at(deadline, deferred.completions.recv()).await;
            match waited {
                Err(_) => {
                    let names = deferred
                        .pending_prompt_hooks
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.deferred_startup = Some(deferred);
                    anyhow::bail!(
                        "prompt hooks are unavailable: {names} did not finish starting within {PROMPT_HOOK_WAIT:?}"
                    );
                }
                Ok(None) => {
                    // Every task has reported; the next pump settles the rest.
                    self.deferred_startup = Some(deferred);
                }
                Ok(Some(activation)) => {
                    queue_deferred_activation(&mut deferred, activation);
                    self.deferred_startup = Some(deferred);
                }
            }
        }
    }

    /// Installs one batch of completed activations into a live fleet.
    ///
    /// Boot and late attach share this exact path: the same first-party-then-
    /// external catalogue registration order, the same summary digests, the
    /// same session-start delivery, and the same failure reporting. `host` is
    /// the host the running Agent consults, so a late attach reaches observers
    /// and session hooks.
    async fn install_activations(
        &mut self,
        host: &mut octet_agent::ExtensionHost,
        descriptors: &BTreeMap<String, DiscoveredExtension>,
        starts: Vec<(String, Result<ExtensionProcess, String>)>,
        tool_clashes: &BTreeMap<String, String>,
    ) -> DeferredStartupProgress {
        let mut progress = DeferredStartupProgress::default();
        let mut ready = Vec::new();
        // Tool-name clashes decided at discovery keep their recorded reason
        // across a later batch, and a failure recorded earlier is never erased
        // by a batch that does not mention it.
        let mut start_failures = self.start_failures.clone();
        start_failures.extend(
            tool_clashes
                .iter()
                .map(|(name, reason)| (name.clone(), format!("turned off: {reason}"))),
        );
        for (name, start) in starts {
            match start {
                Ok(process) => ready.push(process),
                Err(error) => {
                    self.diagnostics
                        .push(format!("error: extension {name:?} launch failed: {error}"));
                    start_failures.insert(name.clone(), clip_lifecycle_reason(&error, 4 * 1024));
                    progress.notices.push(format!(
                        "extension {name} did not start: {error} (see /extensions)"
                    ));
                }
            }
        }
        if !ready.is_empty() {
            // API 0.3 provider declarations are reverse RPCs intentionally sent
            // after initialize. Observe their bounded startup barrier before
            // the caller projects models from the registry.
            self.provider_runtime
                .await_initial_registrations_async(&ready)
                .await;
            // Register tool catalogs before observers/hooks so all live
            // processes have a host-owned dynamic group. A compatible shared
            // process was detached from the previous App binding before this
            // point. First-party catalogs register first, so a clash found only
            // at runtime turns the external extension off instead of failing
            // startup.
            let (first_party, external): (Vec<_>, Vec<_>) = ready
                .iter()
                .partition(|process| first_party_tool_owner(&process.descriptor().manifest.name));
            let mut turned_off = BTreeSet::new();
            for process in first_party.into_iter().chain(external) {
                if let Err(error) = process.try_register_dynamic_tool_catalog(host) {
                    let name = process.descriptor().manifest.name.clone();
                    self.diagnostics.push(format!(
                        "warning: extension {name:?} was turned off: {error}"
                    ));
                    start_failures.insert(name.clone(), format!("turned off: {error}"));
                    progress.notices.push(format!(
                        "extension {name} did not start: {error} (see /extensions)"
                    ));
                    turned_off.insert(name);
                }
            }
            ready.retain(|process| !turned_off.contains(&process.descriptor().manifest.name));
            for process in &ready {
                host.load(process);
            }
            if !ready.is_empty() {
                self.attach_session_lifecycle(&ready).await;
            }
            progress.attached = ready
                .iter()
                .map(|process| process.descriptor().manifest.name.clone())
                .collect();
            progress.changed = !progress.attached.is_empty();
            self.receivers
                .extend(ready.iter().map(ExtensionProcess::subscribe));
            self.processes.extend(ready);
        }
        progress.changed |= !progress.notices.is_empty();
        self.start_failures = start_failures;
        let (shortcuts, diagnostics) = register_extension_shortcuts(&self.processes);
        self.shortcuts = shortcuts;
        self.diagnostics.extend(diagnostics);
        let runtime_statuses = self
            .runtime_manager
            .as_ref()
            .map(ExtensionRuntimeManager::statuses)
            .unwrap_or_default()
            .into_iter()
            .map(|status| (status.provenance.extension.clone(), status))
            .collect::<BTreeMap<_, _>>();
        self.summaries = descriptors
            .values()
            .map(|descriptor| {
                let process = self
                    .processes
                    .iter()
                    .find(|process| process.descriptor().manifest.name == descriptor.manifest.name);
                extension_summary(
                    descriptor,
                    process,
                    &self.start_failures,
                    runtime_statuses.get(&descriptor.manifest.name).cloned(),
                )
            })
            .collect();
        progress
    }

    /// Delivers the session-start lifecycle to freshly attached processes.
    ///
    /// The partition rule is the same one synchronous boot uses: a process
    /// whose session start can await durable session work runs under the
    /// frontend's event pump, never inside the attach boundary.
    async fn attach_session_lifecycle(&mut self, processes: &[ExtensionProcess]) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let event = ExtensionLifecycleEvent::SessionStarted {
            session_id,
            run_id: None,
        };
        self.diagnostics
            .extend(notify_lifecycle_all(processes, event).await);
        let Some(resource_owner) = self.resource_owner.clone() else {
            return;
        };
        let (deferred_processes, immediate): (Vec<_>, Vec<_>) =
            processes.iter().cloned().partition(|process| {
                session_hooks_need_frontend_pump(process, self.remote_ui_wake.is_some())
            });
        self.pending_session_hook_starts.extend(
            deferred_processes
                .into_iter()
                .filter(ExtensionProcess::declares_session_hooks)
                .map(|process| (process, resource_owner.clone())),
        );
        self.diagnostics.extend(
            start_session_hooks_all(&immediate, &resource_owner, self.session_start_reason).await,
        );
        self.session_lifecycle_started = true;
        self.session_started_at = Instant::now();
    }

    /// Bind the final host-policed catalog after static tools and policy are assembled.
    pub fn bind_tool_host(&mut self, host: &octet_agent::ExtensionHost) {
        self.tool_host = Some(host.clone());
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

    /// Activates one explicitly selected extension after its private Python
    /// runtime has been provisioned, then publishes the process through the
    /// same host-owned registries as startup activation.
    pub(crate) async fn activate_python_extension(
        &mut self,
        extension: &str,
    ) -> anyhow::Result<()> {
        let python_runtime = super::status::private_python_runtime_config(None)?;
        self.activate_python_extension_with_runtime(extension, python_runtime)
            .await
    }

    async fn activate_python_extension_with_runtime(
        &mut self,
        extension: &str,
        python_runtime: octet_agent::extension_process::PythonRuntimeConfig,
    ) -> anyhow::Result<()> {
        if self
            .processes
            .iter()
            .any(|process| process.descriptor().manifest.name == extension && process.is_running())
        {
            // Setup is also offered for live extensions; provisioning a runtime
            // for one that is already publishing its tools must not fail.
            return Ok(());
        }
        let binding = self
            .runtime_binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("extension runtime session is unavailable"))?;
        let manager = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("extension runtime catalogue is unavailable"))?;
        let catalog = manager.catalog();
        let entry = catalog
            .get(extension)
            .ok_or_else(|| anyhow::anyhow!("selected extension was not discovered"))?;
        let mut runtime = ExtensionRuntimeConfig::new(self.workspace.clone());
        // Mirrors the eager interactive startup closure: the selected extension
        // is reactivated with the same host-owned services it would have had at
        // startup, using the already configured private runtime location.
        runtime.python_runtime = Some(python_runtime);
        runtime.host_state = self
            .host_state
            .lock()
            .expect("extension host state")
            .clone();
        runtime.remote_ui = self.remote_ui_wake.clone();
        runtime.transcript_render = runtime.remote_ui.is_some();
        runtime.resource_paths = true;
        runtime.tool_composition = true;
        runtime.provider_pipeline = true;
        runtime.provider_registry = Some(self.provider_runtime.registry());
        runtime.bulk_store = manager.bulk_storage().ok();
        if let Some(config) = self.rescan_config.as_ref() {
            runtime.flag_values = config
                .extension_flag_values
                .get(extension)
                .cloned()
                .unwrap_or_default();
            runtime.agent_sessions =
                extension == SUBAGENTS_EXTENSION_NAME && config.tool_available("subagent_spawn");
        }
        // Isolated lifecycle owners also receive the session event bus; shared
        // workspace services reject either service at activation, exactly as at
        // startup, so they must never be attached implicitly.
        if extension_session_lifecycle_eligible(&entry.descriptor) {
            runtime.session_lifecycle = self.session_lifecycle_service.clone();
            runtime.event_bus = self.event_bus.clone();
        }

        let lease = binding.activate(extension, runtime).await?;
        let process = lease.process().clone();
        let registration = {
            let host = self
                .tool_host
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("extension tool host is unavailable"))?;
            let registration = process.try_register_dynamic_tool_catalog(host);
            if registration.is_ok() {
                host.load(&process);
            }
            registration
        };
        if let Err(error) = registration {
            let reason = format!("turned off: {error}");
            self.diagnostics.push(format!(
                "warning: extension {extension:?} was turned off: {error}"
            ));
            self.start_failures.insert(extension.to_owned(), reason);
            anyhow::bail!("extension tool catalogue registration failed: {error}");
        }
        self.receivers.push(process.subscribe());
        let contributions = process.contributions();
        if let Some(summary) = self
            .summaries
            .iter_mut()
            .find(|item| item.name == extension)
        {
            summary.running = true;
            summary.api_version = process.api_version().to_owned();
            summary.negotiated_features = process.negotiated_features().iter().cloned().collect();
            summary.tools = contributions
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            summary.commands = contributions
                .commands
                .iter()
                .map(|command| command.name.clone())
                .collect();
            summary.hooks = contributions.hooks.clone();
            summary.ui = contributions.ui.clone();
            summary.health = Some(process.health_snapshot());
            (summary.telemetry_schema, summary.compatibility) = extension_compatibility(
                extension,
                true,
                &summary.negotiated_features,
                summary.health.as_ref(),
            );
        }
        self.processes.push(process.clone());
        self.start_failures.remove(extension);
        self.start_policy_supervisors();
        self.activate_new_process_session_lifecycle(process).await;
        let (shortcuts, diagnostics) = register_extension_shortcuts(&self.processes);
        self.shortcuts = shortcuts;
        self.diagnostics.extend(diagnostics);
        self.provider_runtime
            .await_initial_registrations_async(&self.processes)
            .await;
        Ok(())
    }

    async fn activate_new_process_session_lifecycle(&mut self, process: ExtensionProcess) {
        if let Some(session_id) = self.session_id.clone() {
            let event = ExtensionLifecycleEvent::SessionStarted {
                session_id,
                run_id: None,
            };
            let messages = notify_lifecycle_all(std::slice::from_ref(&process), event).await;
            self.diagnostics.extend(messages);
        }
        if process.declares_session_hooks() {
            if let Some(owner) = self.resource_owner.clone() {
                self.pending_session_hook_starts.push((process, owner));
            }
        }
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

    /// Busy controls acknowledge queue admission, never an in-flight mutation.
    pub fn next_model_control_request(&mut self) -> Option<ExtensionSessionLifecycleRequest> {
        self.session_lifecycle_receiver
            .as_mut()
            .and_then(ExtensionSessionLifecycleReceiver::try_next_model_control)
    }

    pub(crate) fn model_control_owner_is_current(
        &self,
        owner: &octet_agent::extension_process::ExtensionResourceOwner,
    ) -> bool {
        self.resource_owner.as_deref() == Some(owner.session_id.as_str())
            && self
                .processes
                .iter()
                .any(|process| process.resource_owner_is_live(owner))
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
        let start_reason = self.session_start_reason;
        if let Some(resource_owner) = self.resource_owner.clone() {
            let has_remote_ui = self.remote_ui_wake.is_some();
            let (deferred_processes, processes): (Vec<_>, Vec<_>) = self
                .processes
                .iter()
                .cloned()
                .partition(|process| session_hooks_need_frontend_pump(process, has_remote_ui));
            self.pending_session_hook_starts.extend(
                deferred_processes
                    .into_iter()
                    .filter(|process| process.declares_session_hooks())
                    .map(|process| (process, resource_owner.clone())),
            );
            match block_on_runtime(async move {
                start_session_hooks_all(&processes, &resource_owner, start_reason).await
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
        let reason = self.session_start_reason;
        for (process, owner) in self.pending_session_hook_starts.drain(..) {
            let tx = self.background_tx.clone();
            self.session_hook_start_tasks.push(tokio::spawn(async move {
                // The process owns its bounded request deadline. The shell is
                // free to service ui/open while this hook awaits its result.
                if let Err(error) = process
                    .start_session_hook_binding_with_reason(owner, reason)
                    .await
                {
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
        self.cancel_editor_autocomplete();
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
        self.clear_pi_mcp_registrations().await;
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
        // Terminal intent must fence restart/activation before session_end can
        // await, time out, or kill its generation. Rebuild release is not terminal.
        if let Some(manager) = &self.runtime_manager {
            manager.begin_shutdown();
        }
        for process in &self.processes {
            process.begin_shutdown();
        }
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

    #[cfg_attr(not(test), allow(dead_code))] // used by tests only
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

/// The Pi bridge ships in the official catalog, but every tool it carries
/// comes from a third-party Pi package.
const PI_BRIDGE_EXTENSION_NAME: &str = "octet-pi-compat";

/// First-party extensions keep a clashing tool name: their tools are the ones
/// octet ships and documents.
fn first_party_tool_owner(name: &str) -> bool {
    name != PI_BRIDGE_EXTENSION_NAME && crate::extension_bundle::is_official_bundle(name)
}

/// Splits startable extensions into those kept and the external ones whose
/// declared tools clash with a first-party extension's, keyed by name with the
/// reason. Runs in O(n log n) over declared tool names.
fn turn_off_tool_clashes(
    startable: Vec<DiscoveredExtension>,
) -> (Vec<DiscoveredExtension>, BTreeMap<String, String>) {
    let owners = startable
        .iter()
        .filter(|descriptor| first_party_tool_owner(&descriptor.manifest.name))
        .flat_map(|descriptor| {
            descriptor
                .manifest
                .contributes
                .tools
                .iter()
                .map(|tool| (tool.clone(), descriptor.manifest.name.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut clashes = BTreeMap::new();
    let kept = startable
        .into_iter()
        .filter(|descriptor| {
            if first_party_tool_owner(&descriptor.manifest.name) {
                return true;
            }
            let clashing = descriptor
                .manifest
                .contributes
                .tools
                .iter()
                .filter_map(|tool| {
                    owners
                        .get(tool)
                        .map(|owner| format!("`{tool}` (provided by {owner:?})"))
                })
                .collect::<Vec<_>>();
            if clashing.is_empty() {
                return true;
            }
            clashes.insert(
                descriptor.manifest.name.clone(),
                format!(
                    "its tool {} clashes with a first-party extension",
                    clashing.join(", ")
                ),
            );
            false
        })
        .collect();
    (kept, clashes)
}

/// Plain wording for a classified startup failure without host detail.
fn startup_failure_reason(
    failure: octet_agent::extension_runtime::ExtensionRuntimeFailure,
) -> &'static str {
    use octet_agent::extension_runtime::ExtensionRuntimeFailure as Failure;
    match failure {
        Failure::NotEligible => "it is not enabled and trusted",
        Failure::StaleSource => "its source changed while starting",
        Failure::Launch => "its process exited during startup",
        Failure::Protocol => "it failed the startup handshake",
        Failure::StartupTimeout => "it did not finish starting in time",
        Failure::ManagerClosed => "octet was shutting down",
    }
}

/// Runtime configuration projected for every activated process.
///
/// Boot and deferred attach share this projection so a late attach receives
/// exactly the host services the synchronous path would have provided.
struct ExtensionActivationPlan {
    workspace: PathBuf,
    python_runtime: Option<octet_agent::extension_process::PythonRuntimeConfig>,
    resource_paths: bool,
    host_state: ExtensionHostState,
    remote_ui: Option<Arc<tokio::sync::Notify>>,
    bulk_store: Option<octet_agent::BulkStorage>,
    flag_values: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    agent_sessions_tool_available: bool,
    session_lifecycle: Option<ExtensionSessionLifecycleService>,
    event_bus: Option<Arc<ExtensionEventBus>>,
    provider_registry: Option<Arc<octet_agent::ExtensionProviderRegistry>>,
}

impl ExtensionActivationPlan {
    fn runtime_for(&self, descriptor: &DiscoveredExtension) -> ExtensionRuntimeConfig {
        let mut runtime = ExtensionRuntimeConfig::new(self.workspace.clone());
        runtime.python_runtime = self.python_runtime.clone();
        runtime.resource_paths = self.resource_paths;
        runtime.provider_pipeline = true;
        runtime.host_state = self.host_state.clone();
        runtime.remote_ui = self.remote_ui.clone();
        // Both interactive frontends consume the same bounded asynchronous
        // transcript snapshots. Headless hosts have neither this wake nor the
        // presentation driver.
        runtime.transcript_render = runtime.remote_ui.is_some();
        // Generic request-scoped composition is offered to API 0.4 extensions.
        // Authority is attached later, only to live model-tool contexts, never
        // commands.
        runtime.tool_composition = true;
        runtime.bulk_store = self.bulk_store.clone();
        runtime.flag_values = self
            .flag_values
            .get(&descriptor.manifest.name)
            .cloned()
            .unwrap_or_default();
        runtime.agent_sessions = descriptor.manifest.name == SUBAGENTS_EXTENSION_NAME
            && self.agent_sessions_tool_available;
        runtime.session_lifecycle = if extension_session_lifecycle_eligible(descriptor) {
            self.session_lifecycle.clone()
        } else {
            None
        };
        if extension_session_lifecycle_eligible(descriptor) {
            runtime.event_bus = self.event_bus.clone();
        }
        runtime.provider_registry = self.provider_registry.clone();
        runtime
    }
}

/// One settled activation as the install path consumes it.
fn activation_start(
    activation: octet_agent::extension_runtime::ExtensionRuntimeActivation,
) -> (String, Result<ExtensionProcess, String>) {
    let name = activation.extension;
    let detail = activation.detail;
    match (activation.outcome, activation.process) {
        (ExtensionRuntimeActivationOutcome::Ready, Some(process)) => (name, Ok(process)),
        (outcome, _) => (
            name,
            Err(match outcome {
                ExtensionRuntimeActivationOutcome::Inactive => {
                    "extension is not eligible for activation".to_owned()
                }
                ExtensionRuntimeActivationOutcome::StaleSource => {
                    "extension source changed before activation".to_owned()
                }
                ExtensionRuntimeActivationOutcome::ResourceExhausted(error) => error.to_string(),
                ExtensionRuntimeActivationOutcome::Failed(failure) => {
                    detail.unwrap_or_else(|| startup_failure_reason(failure).to_owned())
                }
                ExtensionRuntimeActivationOutcome::Ready => {
                    "runtime activation returned no process".to_owned()
                }
            }),
        ),
    }
}

fn activation_to_start(
    activation: DeferredActivation,
) -> (String, Result<ExtensionProcess, String>) {
    match activation {
        DeferredActivation::Ready { name, process } => (name, Ok(process)),
        DeferredActivation::Failed { name, detail } => (name, Err(detail)),
    }
}

/// Starts one activation task per admitted extension and hands the caller the
/// state that later installs each settled handshake.
///
/// Independent extensions start concurrently (the same fan-out the synchronous
/// path uses); completion order is what decides attach order, exactly as if the
/// processes had reported to a frontend that was already live.
fn spawn_deferred_startup(
    binding: ExtensionSessionBinding,
    startable: Vec<DiscoveredExtension>,
    descriptors: &BTreeMap<String, DiscoveredExtension>,
    plan: &ExtensionActivationPlan,
    tool_clashes: BTreeMap<String, String>,
    agent_sessions_pending: bool,
) -> DeferredExtensionStartup {
    let (completions_tx, completions) = mpsc::unbounded_channel();
    let descriptors = descriptors.clone();
    let pending = startable
        .iter()
        .map(|descriptor| descriptor.manifest.name.clone())
        .collect::<BTreeSet<_>>();
    let pending_prompt_hooks = startable
        .iter()
        .filter(|descriptor| {
            descriptor
                .manifest
                .contributes
                .hooks
                .contains(&ExtensionHook::BeforePrompt)
        })
        .map(|descriptor| descriptor.manifest.name.clone())
        .collect::<BTreeSet<_>>();
    let mut tasks = Vec::new();
    for descriptor in startable {
        let name = descriptor.manifest.name.clone();
        let runtime = plan.runtime_for(&descriptor);
        let binding = binding.clone();
        let completions = completions_tx.clone();
        let task = tokio::spawn(async move {
            let activation = match binding.activate(&name, runtime).await {
                Ok(lease) => DeferredActivation::Ready {
                    name,
                    process: lease.process().clone(),
                },
                Err(error) => DeferredActivation::Failed {
                    name,
                    detail: clip_lifecycle_reason(&error.to_string(), 4 * 1024),
                },
            };
            let _ = completions.send(activation);
        });
        tasks.push(task);
    }
    drop(completions_tx);
    DeferredExtensionStartup {
        completions,
        queued: Vec::new(),
        pending,
        pending_prompt_hooks,
        descriptors,
        tool_clashes,
        agent_sessions_pending,
        tasks,
    }
}

/// Drains every settled activation without waiting for another handshake.
fn drain_deferred_activations(
    deferred: &mut DeferredExtensionStartup,
) -> Vec<(String, Result<ExtensionProcess, String>)> {
    let mut starts = std::mem::take(&mut deferred.queued);
    loop {
        match deferred.completions.try_recv() {
            Ok(activation) => queue_deferred_activation(deferred, activation),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                // Every activation task has reported. Anything still pending
                // settled without a result (an aborted task), which the caller
                // reports as a launch failure instead of a silent disappearance.
                for name in std::mem::take(&mut deferred.pending) {
                    starts.push((
                        name,
                        Err("extension activation did not report a result".to_owned()),
                    ));
                }
                deferred.pending_prompt_hooks.clear();
                break;
            }
        }
    }
    // Completions received above are queued by `queue_deferred_activation`;
    // include them in this pump before the caller can observe an empty pending
    // set and discard the deferred state.
    starts.extend(std::mem::take(&mut deferred.queued));
    starts
}

/// Records one settled activation as a start the install path will consume.
fn queue_deferred_activation(
    deferred: &mut DeferredExtensionStartup,
    activation: DeferredActivation,
) {
    let (name, result) = activation_to_start(activation);
    deferred.pending.remove(&name);
    deferred.pending_prompt_hooks.remove(&name);
    deferred.queued.push((name, result));
}

/// Marks a deferred startup complete: every task has reported, every process
/// that will ever attach has attached.
fn finish_deferred_startup(deferred: &mut DeferredExtensionStartup) {
    for task in deferred.tasks.drain(..) {
        task.abort();
    }
    crate::app::bootstrap::startup_phase("extensions.settled");
}

/// Whether one process's session start must run under the frontend's event
/// pump rather than the blocking attach boundary.
fn session_hooks_need_frontend_pump(process: &ExtensionProcess, has_remote_ui: bool) -> bool {
    // Resource startup must run under the frontend's event pump, even without
    // remote UI. It can await durable session work (or a real headless refusal)
    // before discovery is admitted.
    (process.supports_feature(octet_agent::extension_process::EXTENSION_FEATURE_RESOURCE_PATHS)
        && process
            .contributions()
            .hooks
            .contains(&ExtensionHook::ResourcesDiscover))
        || process.supports_feature("mcp_registration_v1")
        || (has_remote_ui && process.supports_feature(EXTENSION_FEATURE_REMOTE_UI))
}

/// One extension's status summary, for boot and for each late attach.
fn extension_summary(
    descriptor: &DiscoveredExtension,
    process: Option<&ExtensionProcess>,
    start_failures: &BTreeMap<String, String>,
    runtime_status: Option<octet_agent::extension_runtime::ExtensionRuntimeStatus>,
) -> ExtensionSummary {
    let contributions = process.map(ExtensionProcess::contributions);
    let health = process.map(ExtensionProcess::health_snapshot).or_else(|| {
        start_failures
            .get(&descriptor.manifest.name)
            .map(|error| ExtensionHealthSnapshot {
                state: ExtensionHealthState::Parked,
                generation: 0,
                pending_requests: 0,
                last_error: Some(error.clone()),
            })
    });
    let negotiated_features: Vec<String> = process
        .map(|process| process.negotiated_features().iter().cloned().collect())
        .unwrap_or_default();
    let running = process.is_some_and(ExtensionProcess::is_running);
    let (telemetry_schema, compatibility) = extension_compatibility(
        &descriptor.manifest.name,
        running,
        &negotiated_features,
        health.as_ref(),
    );
    let manifest_path = descriptor.manifest_path.clone();
    let manifest_digest = sha256_manifest(&manifest_path);
    let bundle_digest = installed_bundle_digest(&manifest_path);
    ExtensionSummary {
        name: descriptor.manifest.name.clone(),
        version: descriptor.manifest.version.clone(),
        manifest_path,
        manifest_digest,
        bundle_digest,
        source: descriptor.source,
        enabled: descriptor.activation.enabled,
        trusted: descriptor.activation.trust == ExtensionTrust::Trusted,
        running,
        api_version: process
            .map(|process| process.api_version().to_owned())
            .unwrap_or_else(|| descriptor.manifest.api_version.clone()),
        negotiated_features,
        telemetry_schema,
        compatibility,
        health,
        runtime: runtime_status,
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
        hooks: descriptor.manifest.contributes.hooks.clone(),
        ui: descriptor.manifest.contributes.ui.clone(),
        // Live declarations are overlaid by `summaries()` from the shared
        // registry; discovery alone has no provider state.
        providers: Vec::new(),
    }
}
